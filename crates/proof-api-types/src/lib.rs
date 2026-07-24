//! Wire types for `proof-service`'s JSON verification API
//! (`GET /api/v1/proofs/{digest}`, `POST /api/v1/verify`).
//!
//! Kept in their own crate, separate from `proof-core` (what a proof
//! *is*) and `proof-service` (the binary that serves them), so both the
//! service and any client (e.g. `bil-client`) depend on the exact same
//! struct definitions rather than each hand-rolling matching JSON shapes
//! that can silently drift apart when a field is renamed on one side.
//!
//! `#![forbid(unsafe_code)]` and clippy-pedantic-clean, per this
//! workspace's standing bar for every crate.

#![forbid(unsafe_code)]

use proof_anchor::AnchorReceipt;
use proof_core::Proof;
use serde::{Deserialize, Serialize};

/// Response body for `GET /api/v1/proofs/{digest}`: a proof this service
/// produced, plus the source-level context and anchor receipt that go
/// with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofRecordResponse {
    /// Which source produced this proof (e.g. `"postgres:default"`).
    pub source_id: String,
    /// That source's position for the record this proof covers.
    pub source_position: String,
    /// The proof itself.
    pub proof: Proof,
    /// Where this proof was anchored.
    pub receipt: AnchorReceipt,
}

/// Request body for `POST /api/v1/verify`.
///
/// Deliberately does not require the proof to be one the target service
/// has ever seen before — the whole point of an on-chain (or
/// hash-chained local log) anchor is that a third party can check it
/// without trusting that service's memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// The proof being checked.
    pub proof: Proof,
    /// The receipt claiming where `proof` was anchored.
    pub receipt: AnchorReceipt,
}

/// Response body for `POST /api/v1/verify`.
///
/// A non-2xx HTTP status from that endpoint means the request itself was
/// malformed or the anchor backend's underlying ledger could not be
/// reached at all — a merely-invalid proof is still a `200` response
/// with `valid: false` here, since "this proof doesn't check out" is a
/// normal, expected outcome, not a server error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    /// Whether every attestation and the anchor both checked out.
    pub valid: bool,
    /// The checked proof's digest, hex-encoded, echoed back for the
    /// caller's convenience (e.g. to correlate a batch of verify calls).
    pub digest_hex: String,
    /// `None` when `valid` is `true`; otherwise a human-readable reason,
    /// covering both "signatures don't check out" and "anchor doesn't
    /// match" failure modes under one field so callers don't need to
    /// branch on which stage failed to just show a user why.
    pub reason: Option<String>,
}

/// Error body returned for non-2xx responses (e.g. `404` from
/// `GET /api/v1/proofs/{digest}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Human-readable error message. Not intended to be parsed by
    /// callers beyond display — match on the HTTP status code for
    /// program logic instead.
    pub error: String,
}
