//! The JSON verification API under `/api/v1` — the first
//! externally-consumable surface a counterparty (not a Rust process
//! sharing this repo) can use to check a proof, closing the "no
//! externally-consumable verification surface" gap from `ROADMAP.md`.
//!
//! Two endpoints, deliberately covering different trust starting points:
//! - `GET /api/v1/proofs/{digest}` — "what did this service commit?" Looks
//!   up a proof this service itself produced, by digest, from the same
//!   history `InProcessProofSink` already retains for the live UI.
//! - `POST /api/v1/verify` — "is this proof someone handed me genuine?"
//!   Takes a full `Proof` + `AnchorReceipt` a caller already has (both are
//!   plain JSON, no dependency on this service having ever seen them) and
//!   independently re-verifies signatures and the on-chain/log anchor,
//!   exactly like `ProofAnchor::verify()`'s own contract, no different
//!   whether the caller got them from this API's first endpoint or from
//!   anywhere else entirely.
//!
//! Request/response shapes live in `proof-api-types`, not here, so
//! `bil-client` (and any other client) depends on the exact same struct
//! definitions this handler serializes, rather than hand-rolled types
//! that can silently drift out of sync with this endpoint.

use super::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use proof_api_types::{ErrorResponse, ProofRecordResponse, VerifyRequest, VerifyResponse};

/// `GET /api/v1/proofs/{digest}` — `digest` is the lowercase-hex digest as
/// rendered by [`proof_core::hash::Digest::to_hex`] (the same string shown
/// in the UI's proof rows).
///
/// # Errors
///
/// Returns `404` if no proof with that digest is in this service's
/// retained history (see [`proof_connectors::InProcessProofSink`]'s
/// capacity — this is a lookup over what's retained, not a durable
/// database).
pub async fn get_proof_by_digest(
    State(state): State<AppState>,
    Path(digest): Path<String>,
) -> Response {
    let (history, _live) = state.sink.subscribe();
    let Some(record) = history
        .into_iter()
        .find(|r| r.proof.digest().to_hex() == digest)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("no proof with digest {digest} in retained history"),
            }),
        )
            .into_response();
    };

    Json(ProofRecordResponse {
        source_id: record.source_id.to_string(),
        source_position: record.source_position,
        proof: record.proof,
        receipt: record.receipt,
    })
    .into_response()
}

/// `POST /api/v1/verify` — re-verifies a caller-supplied `Proof` against
/// its own attestations, then against the anchor backend this service is
/// currently configured with.
///
/// Deliberately does not require the proof to be one this service has
/// ever seen before — the whole point of an on-chain (or hash-chained
/// local log) anchor is that a third party can check it without trusting
/// this process's memory.
///
/// # Errors
///
/// Never returns a non-2xx status for a merely-invalid proof — that's a
/// normal, expected outcome reported as `{"valid": false, "reason": ...}`
/// in the body, not an HTTP error. A non-2xx response here means the
/// request itself was malformed (bad JSON) or this anchor backend's
/// underlying ledger could not be reached at all.
pub async fn verify_proof(
    State(state): State<AppState>,
    Json(req): Json<VerifyRequest>,
) -> Response {
    let digest_hex = req.proof.digest().to_hex();

    if let Err(e) = req.proof.verify_attestations() {
        return Json(VerifyResponse {
            valid: false,
            digest_hex,
            reason: Some(format!("attestation verification failed: {e}")),
        })
        .into_response();
    }

    match state.anchor.verify(&req.receipt, &req.proof).await {
        Ok(()) => Json(VerifyResponse {
            valid: true,
            digest_hex,
            reason: None,
        })
        .into_response(),
        Err(e) => Json(VerifyResponse {
            valid: false,
            digest_hex,
            reason: Some(format!("anchor verification failed: {e}")),
        })
        .into_response(),
    }
}
