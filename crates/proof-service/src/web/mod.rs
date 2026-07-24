//! The axum web layer: a live-activity page (server-rendered HTML +
//! htmx, updated over Server-Sent Events) backed directly by
//! [`proof_connectors::InProcessProofSink`]'s history/broadcast — the UI
//! is just another `ProofSink` consumer, no different in kind from the
//! metrics recorder or a future anchoring consumer — a small JSON
//! verification API (`api`) for external, non-Rust callers, and a
//! human-facing compliance dashboard (`compliance`) over that same data.

mod api;
mod compliance;
mod feed;
mod index;
mod static_assets;

use axum::Router;
use axum::routing::{get, post};
use metrics_exporter_prometheus::PrometheusHandle;
use proof_anchor::ProofAnchor;
use proof_connectors::InProcessProofSink;
use std::sync::Arc;

/// Shared state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    pub sink: Arc<InProcessProofSink>,
    pub anchor: Arc<dyn ProofAnchor>,
    /// Lets the compliance dashboard's aggregate view read this
    /// process's own live counter values in-process (see
    /// `compliance.rs`'s module docs for why that beats a second
    /// bookkeeping mechanism).
    pub metrics_handle: PrometheusHandle,
}

/// Builds the full router: the live index page, its SSE feed, the
/// embedded static assets that page needs (htmx + its SSE extension),
/// the JSON verification API under `/api/v1`, and the compliance
/// dashboard under `/compliance`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index::index))
        .route("/feed", get(feed::feed))
        .route("/api/v1/proofs/{digest}", get(api::get_proof_by_digest))
        .route("/api/v1/verify", post(api::verify_proof))
        .route("/compliance", get(compliance::compliance_index))
        .route("/compliance/{digest}", get(compliance::compliance_record))
        .route(
            "/compliance/{digest}/verify",
            post(compliance::compliance_verify),
        )
        .with_state(state)
        .merge(static_assets::router())
}
