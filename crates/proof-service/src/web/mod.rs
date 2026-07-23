//! The axum web layer: a live-activity page (server-rendered HTML +
//! htmx, updated over Server-Sent Events) backed directly by
//! [`proof_connectors::InProcessProofSink`]'s history/broadcast — the UI
//! is just another `ProofSink` consumer, no different in kind from the
//! metrics recorder or a future anchoring consumer.

mod feed;
mod index;
mod static_assets;

use axum::Router;
use axum::routing::get;
use proof_connectors::InProcessProofSink;
use std::sync::Arc;

/// Shared state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    pub sink: Arc<InProcessProofSink>,
}

/// Builds the full router: the live index page, its SSE feed, and the
/// embedded static assets that page needs (htmx + its SSE extension).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index::index))
        .route("/feed", get(feed::feed))
        .with_state(state)
        .merge(static_assets::router())
}
