//! The `GET /feed` handler: a Server-Sent Events stream of newly proved
//! records, rendered server-side as HTML fragments (`proof_row.html`) so
//! the browser only ever needs htmx's SSE extension to swap in new
//! content — no client-side templating or JSON parsing required.

use super::AppState;
use askama::Template;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use proof_connectors::ProvedRecord;
use std::convert::Infallible;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

#[derive(Template)]
#[template(path = "proof_row.html")]
pub struct ProofRowTemplate {
    source_id: String,
    table: String,
    change_kind: String,
    source_position: String,
    attestation_count: usize,
    timestamp: String,
    digest_hex: String,
}

impl From<&ProvedRecord> for ProofRowTemplate {
    fn from(record: &ProvedRecord) -> Self {
        let table = record
            .proof
            .metadata()
            .iter()
            .find(|(k, _)| k == "table")
            .map_or("unknown", |(_, v)| v.as_str())
            .to_string();
        let change_kind = record
            .proof
            .metadata()
            .iter()
            .find(|(k, _)| k == "change_kind")
            .map_or("unknown", |(_, v)| v.as_str())
            .to_string();

        Self {
            source_id: record.source_id.to_string(),
            table,
            change_kind,
            source_position: record.source_position.clone(),
            attestation_count: record.proof.attestations().len(),
            timestamp: record.proof.timestamp_unix().to_string(),
            digest_hex: record.proof.digest().to_hex(),
        }
    }
}

pub async fn feed(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // History is intentionally not replayed here: the index page already
    // rendered it server-side on load, so replaying it again over the
    // SSE connection that page immediately opens would double every row
    // a fresh page-load sees.
    let (_history, live) = state.sink.subscribe();
    let stream = BroadcastStream::new(live).filter_map(|result| match result {
        Ok(record) => {
            let rendered = ProofRowTemplate::from(&record)
                .render()
                .unwrap_or_else(|e| format!("<!-- template error: {e} -->"));
            Some(Ok(Event::default().event("message").data(rendered)))
        }
        // A lagging subscriber (buffer overflowed) drops some messages;
        // that's reported as a stream error, not fatal to the
        // connection — the client just misses those rows and keeps
        // receiving subsequent ones.
        Err(BroadcastStreamRecvError::Lagged(_)) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
