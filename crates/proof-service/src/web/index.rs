//! The `GET /` handler: renders the current proof history as the initial
//! page load, then hands off to the SSE feed (`GET /feed`) for live
//! updates. Askama compiles `templates/index.html` at build time, so a
//! template/field mismatch is a compile error here, not a runtime
//! surprise.

use super::AppState;
use crate::web::feed::ProofRowTemplate;
use askama::Template;
use axum::extract::State;
use axum::response::Html;

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    history: Vec<String>,
}

pub async fn index(State(state): State<AppState>) -> Html<String> {
    let (history, _live) = state.sink.subscribe();
    let rendered_rows = history
        .iter()
        .map(|record| {
            ProofRowTemplate::from(record)
                .render()
                .unwrap_or_else(|e| format!("<!-- template error: {e} -->"))
        })
        .collect();

    let page = IndexTemplate {
        history: rendered_rows,
    };
    Html(
        page.render()
            .unwrap_or_else(|e| format!("template error: {e}")),
    )
}
