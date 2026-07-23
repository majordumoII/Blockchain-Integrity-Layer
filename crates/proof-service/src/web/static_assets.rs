//! Vendored JS assets (htmx core + its SSE extension), embedded into the
//! binary via `include_str!` rather than served from disk — keeps
//! `proof-service` a single self-contained deployable, and avoids a
//! runtime dependency on a CDN.
//!
//! Vendored from `htmx.org@2.0.10` and `htmx-ext-sse@2.2.4`; see
//! `static/` in this crate for the original files and their licenses.

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

const HTMX_JS: &str = include_str!("../../static/htmx.min.js");
const SSE_EXT_JS: &str = include_str!("../../static/sse.min.js");

async fn htmx_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], HTMX_JS)
}

async fn sse_ext_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        SSE_EXT_JS,
    )
}

/// Routes serving the embedded static assets under `/static/*`.
pub fn router() -> Router {
    Router::new()
        .route("/static/htmx.min.js", get(htmx_js))
        .route("/static/sse.min.js", get(sse_ext_js))
}
