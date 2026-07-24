//! The axum web layer: a live-activity page (server-rendered HTML +
//! htmx, updated over Server-Sent Events) backed directly by
//! [`proof_connectors::InProcessProofSink`]'s history/broadcast — the UI
//! is just another `ProofSink` consumer, no different in kind from the
//! metrics recorder or a future anchoring consumer — plus a small JSON
//! verification API (`api`) for external, non-Rust callers.

mod api;
mod feed;
mod index;
mod static_assets;

use axum::Router;
use axum::routing::{get, post};
use proof_anchor::ProofAnchor;
use proof_connectors::InProcessProofSink;
use std::sync::Arc;

/// Shared state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    pub sink: Arc<InProcessProofSink>,
    pub anchor: Arc<dyn ProofAnchor>,
}

/// Builds the full router: the live index page, its SSE feed, the
/// embedded static assets that page needs (htmx + its SSE extension),
/// and the JSON verification API under `/api/v1`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index::index))
        .route("/feed", get(feed::feed))
        .route("/api/v1/proofs/{digest}", get(api::get_proof_by_digest))
        .route("/api/v1/verify", post(api::verify_proof))
        .with_state(state)
        .merge(static_assets::router())
}
