//! The core loop: pull records from a [`RecordSource`], hash + sign them
//! via `proof-core`, anchor the resulting [`Proof`] to a tamper-evident
//! ledger, submit the anchored proof (with its receipt) to a
//! [`ProofSink`], then acknowledge the source — in that order, so a
//! crash before any of those three steps succeeds leaves the source able
//! to redeliver the record rather than silently treating it as durably
//! proved when it isn't yet (see `proof_connectors::AckToken`'s docs for
//! why ack ordering matters). Anchoring happens before the sink submit so
//! every `ProvedRecord` the sink (and anything reading its history, like
//! a verification API) sees already carries a real `AnchorReceipt`.

use crate::metrics;
use proof_anchor::ProofAnchor;
use proof_connectors::{ProofSink, ProvedRecord, RecordSource, SourceId};
use proof_core::ProofBuilder;
use proof_core::hash::{self, HashAlgorithm};
use proof_core::sign::SigningPrivateKey;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

/// Runs the observe → hash → sign → submit → anchor → acknowledge loop
/// forever (until the source errors out, which callers should treat as
/// this connector needing a restart/backoff, not the whole service
/// exiting).
///
/// `signing_key` attests every proof this pipeline produces — see
/// `main.rs` for why a single freshly generated key is the deliberate
/// choice for this stage rather than a persisted/rotated one.
///
/// # Panics
///
/// Panics if the system clock reports a time before the Unix epoch.
pub async fn run(
    mut source: impl RecordSource,
    sink: Arc<dyn ProofSink>,
    anchor: Arc<dyn ProofAnchor>,
    signing_key: Arc<SigningPrivateKey>,
    source_id: SourceId,
) {
    loop {
        let started_at = Instant::now();
        let (record, ack) = match source.next().await {
            Ok(pair) => pair,
            Err(e) => {
                error!(source = %source_id, error = %e, "record source failed; stopping this pipeline");
                metrics::record_failed(&source_id, "source");
                return;
            }
        };

        let table = record
            .metadata_value("table")
            .unwrap_or("unknown")
            .to_string();
        metrics::record_observed(&source_id, &table);

        let digest = hash::hash(HashAlgorithm::Blake3, &record.bytes);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970")
            .as_secs();

        let mut builder =
            ProofBuilder::new(digest, timestamp).attest(source_id.as_str(), &signing_key);
        for (key, value) in &record.metadata {
            builder = builder.metadata(key.clone(), value.clone());
        }
        let proof = match builder.build() {
            Ok(p) => p,
            Err(e) => {
                // Cannot happen in practice (we always attest at least
                // once above), but if it ever did, this record must not
                // be acknowledged — better to redeliver it forever than
                // silently drop it.
                error!(source = %source_id, error = %e, "failed to build proof; record will be redelivered");
                metrics::record_failed(&source_id, "build");
                continue;
            }
        };

        // Anchoring is this project's actual durability guarantee, so it
        // gates the ack (and, below, the sink submission): a crash before
        // it succeeds must leave the record redeliverable rather than
        // silently ending up "acked but never anchored." Anchoring before
        // the sink submit (rather than after, as in an earlier version of
        // this loop) also means the `ProvedRecord` handed to the sink can
        // carry the real `AnchorReceipt` a verification API can look up,
        // instead of the sink only ever seeing pre-anchor state.
        let receipt = match anchor.anchor(&proof).await {
            Ok(r) => r,
            Err(e) => {
                error!(source = %source_id, error = %e, "anchoring failed; record will be redelivered");
                metrics::record_failed(&source_id, "anchor");
                continue;
            }
        };

        let proved = ProvedRecord {
            source_id: source_id.clone(),
            source_position: record.source_position.clone(),
            proof: proof.clone(),
            receipt: receipt.clone(),
        };
        if let Err(e) = sink.submit(proved).await {
            error!(source = %source_id, error = %e, "sink rejected proof; record will be redelivered");
            metrics::record_failed(&source_id, "sink");
            continue;
        }

        // Only acknowledge after the proof has been anchored and the sink
        // has durably accepted it — this is the ordering `AckToken`'s
        // contract depends on.
        if let Err(e) = ack.ack().await {
            warn!(source = %source_id, error = %e, "failed to acknowledge source; record may be redelivered");
            metrics::record_failed(&source_id, "ack");
            continue;
        }

        metrics::record_proved(&source_id, &table, started_at);
        info!(
            source = %source_id,
            table = %table,
            position = %record.source_position,
            receipt = %receipt,
            "proved and anchored record"
        );
    }
}
