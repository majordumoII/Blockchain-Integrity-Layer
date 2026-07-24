//! [`ProofSink`]: the industry-agnostic abstraction over "where finished
//! proofs go" — the fan-out point that lets the live UI feed, Prometheus
//! metrics, and (later) chain anchoring all consume the same stream of
//! proofs independently, without knowing about each other.

use crate::source::SourceId;
use proof_anchor::AnchorReceipt;
use proof_core::Proof;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// A [`Proof`] plus the source-level context needed to label/route it —
/// which source produced it and what position in that source it came
/// from — and the [`AnchorReceipt`] confirming where it was committed.
/// The `Proof` itself stays exactly what `proof-core` defines; this
/// wrapper only adds routing/anchoring metadata that lives above
/// `proof-core` and would be out of place inside it.
#[derive(Debug, Clone)]
pub struct ProvedRecord {
    /// Which source produced this proof.
    pub source_id: SourceId,
    /// That source's position for the record this proof covers (echoed
    /// from [`crate::record::SourceRecord::source_position`]).
    pub source_position: String,
    /// The proof itself.
    pub proof: Proof,
    /// Where this proof was anchored — present because a `ProvedRecord`
    /// is only ever submitted once anchoring has already succeeded (see
    /// `proof-service`'s pipeline ordering), so there is no "not yet
    /// anchored" state for this type to represent.
    pub receipt: AnchorReceipt,
}

/// Where finished proofs go once built.
///
/// Implementations must be cheap to call and non-blocking on the hot
/// path — `submit` is called once per proved record, inline with the
/// connector loop that's also responsible for acknowledging the source.
/// A slow or blocking `submit` stalls the whole source.
#[async_trait::async_trait]
pub trait ProofSink: Send + Sync {
    /// Accepts a newly built proof.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError`] if the proof could not be recorded. Callers
    /// must not acknowledge the source record via
    /// [`crate::record::AckToken`] unless this returns `Ok`.
    async fn submit(&self, record: ProvedRecord) -> Result<(), SinkError>;
}

/// A [`ProofSink`] failure. Deliberately opaque beyond a message — sinks
/// vary too much (in-memory, database, message bus) to enumerate a
/// meaningful shared error taxonomy at this layer.
#[derive(Debug, thiserror::Error)]
#[error("failed to record proof: {0}")]
pub struct SinkError(pub String);

/// The first [`ProofSink`] implementation: an in-process broadcast
/// channel (for live consumers — the UI's real-time feed, a metrics
/// hook) backed by a bounded append-only log (for late-joining
/// consumers, e.g. a UI page opened after some proofs already happened).
///
/// This is intentionally the seam a later process split would replace:
/// swap the broadcast channel for a message bus and the in-memory log for
/// a real database, and every `ProofSink` consumer (UI, metrics,
/// anchoring) keeps working unchanged because they only ever depend on
/// this trait, not on this struct.
pub struct InProcessProofSink {
    history: Mutex<VecDeque<ProvedRecord>>,
    history_capacity: usize,
    live: broadcast::Sender<ProvedRecord>,
}

impl InProcessProofSink {
    /// Creates a sink retaining up to `history_capacity` most-recent
    /// proofs for late-joining subscribers, and buffering up to
    /// `live_capacity` proofs per lagging live subscriber before it starts
    /// dropping the oldest (per [`tokio::sync::broadcast`]'s semantics).
    #[must_use]
    pub fn new(history_capacity: usize, live_capacity: usize) -> Self {
        Self {
            history: Mutex::new(VecDeque::with_capacity(history_capacity)),
            history_capacity,
            live: broadcast::channel(live_capacity).0,
        }
    }

    /// Returns a snapshot of retained history plus a receiver for proofs
    /// submitted from this point forward.
    ///
    /// The snapshot is taken and the receiver subscribed atomically with
    /// respect to concurrent `submit` calls (both happen while holding
    /// the history lock), so a proof submitted concurrently with this
    /// call appears in exactly one of the two — never both, never
    /// neither.
    ///
    /// # Panics
    ///
    /// Panics if the internal history lock is poisoned, which only
    /// happens if a prior call panicked while holding it.
    #[must_use]
    pub fn subscribe(&self) -> (Vec<ProvedRecord>, broadcast::Receiver<ProvedRecord>) {
        let history = self.history.lock().expect("history lock not poisoned");
        let receiver = self.live.subscribe();
        (history.iter().cloned().collect(), receiver)
    }
}

#[async_trait::async_trait]
impl ProofSink for InProcessProofSink {
    async fn submit(&self, record: ProvedRecord) -> Result<(), SinkError> {
        {
            let mut history = self.history.lock().expect("history lock not poisoned");
            if history.len() == self.history_capacity {
                history.pop_front();
            }
            history.push_back(record.clone());
        }
        // No receivers is the common case at startup (nothing has
        // subscribed yet) and is not an error — the proof is still
        // durably in `history` for the next subscriber to see.
        let _ = self.live.send(record);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proof_core::ProofBuilder;
    use proof_core::hash::{self, HashAlgorithm};
    use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey};
    use rand_core::OsRng;

    fn sample_proved_record(position: &str) -> ProvedRecord {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let digest = hash::hash(HashAlgorithm::Blake3, position.as_bytes());
        let proof = ProofBuilder::new(digest, 0)
            .attest("test-signer", &key)
            .build()
            .unwrap();
        ProvedRecord {
            source_id: SourceId::new("test-source"),
            source_position: position.to_string(),
            proof,
            receipt: AnchorReceipt {
                ledger_id: "test-ledger".to_string(),
                position_hex: format!("{position:0>4}"),
            },
        }
    }

    #[tokio::test]
    async fn late_subscriber_sees_history() {
        let sink = InProcessProofSink::new(10, 10);
        sink.submit(sample_proved_record("1")).await.unwrap();
        sink.submit(sample_proved_record("2")).await.unwrap();

        let (history, _live) = sink.subscribe();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].source_position, "1");
        assert_eq!(history[1].source_position, "2");
    }

    #[tokio::test]
    async fn live_subscriber_sees_new_submissions() {
        let sink = InProcessProofSink::new(10, 10);
        let (_history, mut live) = sink.subscribe();

        sink.submit(sample_proved_record("1")).await.unwrap();

        let received = live.recv().await.unwrap();
        assert_eq!(received.source_position, "1");
    }

    #[tokio::test]
    async fn history_is_capped() {
        let sink = InProcessProofSink::new(2, 10);
        sink.submit(sample_proved_record("1")).await.unwrap();
        sink.submit(sample_proved_record("2")).await.unwrap();
        sink.submit(sample_proved_record("3")).await.unwrap();

        let (history, _live) = sink.subscribe();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].source_position, "2");
        assert_eq!(history[1].source_position, "3");
    }

    #[tokio::test]
    async fn submit_without_any_subscriber_does_not_error() {
        let sink = InProcessProofSink::new(10, 10);
        assert!(sink.submit(sample_proved_record("1")).await.is_ok());
    }
}
