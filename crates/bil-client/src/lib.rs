//! `bil-client`: a small, typed HTTP client for `proof-service`'s JSON
//! verification API (`GET /api/v1/proofs/{digest}`, `POST /api/v1/verify`).
//!
//! This is the README's "SDK" promise made concrete for the verification
//! side: a counterparty (an auditor, a researcher, another company's
//! service) that has no Rust code sharing this repo can add this crate,
//! point it at a `proof-service` instance's base URL, and check a proof
//! in a couple of lines — no hand-rolled HTTP/JSON, no risk of the wire
//! shape silently drifting out of sync, since [`proof_api_types`] is the
//! single source of truth for the request/response bodies both this
//! crate and the service itself use.
//!
//! ```no_run
//! # async fn example() -> Result<(), bil_client::ClientError> {
//! use bil_client::Client;
//!
//! let client = Client::new("http://localhost:8080")?;
//!
//! // "What did this service commit for this digest?"
//! if let Some(record) = client.get_proof("deadbeef...").await? {
//!     println!("found proof anchored at {}", record.receipt.position_hex);
//! }
//!
//! // "Is this proof (from wherever I got it) actually genuine?"
//! # let proof: proof_core::Proof = unimplemented!();
//! # let receipt: proof_anchor::AnchorReceipt = unimplemented!();
//! let result = client.verify(proof, receipt).await?;
//! assert!(result.valid);
//! # Ok(())
//! # }
//! ```
//!
//! `#![forbid(unsafe_code)]` and clippy-pedantic-clean, per this
//! workspace's standing bar for every crate.

#![forbid(unsafe_code)]

use proof_anchor::AnchorReceipt;
use proof_api_types::{ErrorResponse, ProofRecordResponse, VerifyRequest, VerifyResponse};
use proof_core::Proof;

/// Errors from talking to a `proof-service` instance.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// `base_url` could not be parsed as a URL, or building a request
    /// URL from it failed.
    #[error("invalid base URL: {0}")]
    InvalidUrl(String),

    /// The HTTP request itself failed (connection refused, timeout,
    /// TLS error, ...) — the server was not reached at all.
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// The server responded with a status this client does not treat as
    /// a normal outcome (i.e. not `200` and not the documented `404` for
    /// [`Client::get_proof`]), along with whatever body it returned.
    #[error("server returned {status}: {body}")]
    Server {
        /// The HTTP status code.
        status: reqwest::StatusCode,
        /// The response body, or a placeholder if it could not be read.
        body: String,
    },
}

/// A typed client for one `proof-service` instance's verification API.
///
/// Cheap to clone (wraps a single [`reqwest::Client`], which itself
/// pools connections internally) — construct one per target service and
/// reuse it across calls rather than building a new one per request.
#[derive(Debug, Clone)]
pub struct Client {
    base_url: reqwest::Url,
    http: reqwest::Client,
}

impl Client {
    /// Points a new client at `base_url` (e.g. `"http://localhost:8080"`
    /// or a deployed service's public URL). Does not itself make any
    /// network call — connection errors surface from the first actual
    /// request.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidUrl`] if `base_url` cannot be parsed
    /// as a URL.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, ClientError> {
        let base_url = base_url
            .as_ref()
            .parse()
            .map_err(|e| ClientError::InvalidUrl(format!("{}: {e}", base_url.as_ref())))?;
        Ok(Self {
            base_url,
            http: reqwest::Client::new(),
        })
    }

    /// Looks up a proof this service produced by its hex-encoded digest
    /// (as rendered by [`proof_core::hash::Digest::to_hex`]).
    ///
    /// Returns `Ok(None)` for a `404` (no proof with that digest in the
    /// service's retained history) rather than an error — that is a
    /// normal, expected outcome for a lookup, not a failure.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Request`] if the service could not be
    /// reached, or [`ClientError::Server`] for any non-`200`/`404`
    /// response.
    pub async fn get_proof(
        &self,
        digest_hex: impl AsRef<str>,
    ) -> Result<Option<ProofRecordResponse>, ClientError> {
        let url = self
            .base_url
            .join(&format!("/api/v1/proofs/{}", digest_hex.as_ref()))
            .map_err(|e| ClientError::InvalidUrl(e.to_string()))?;

        let response = self.http.get(url).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::server_error(response).await);
        }

        Ok(Some(response.json::<ProofRecordResponse>().await?))
    }

    /// Independently re-verifies `proof` against `receipt` — signatures
    /// via [`Proof::verify_attestations`], anchor state via the target
    /// service's configured [`proof_anchor::ProofAnchor`] backend. Works
    /// for any genuinely-anchored proof, not only ones this service
    /// itself produced.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Request`] if the service could not be
    /// reached, or [`ClientError::Server`] for a non-`200` response. A
    /// proof that fails verification is still `Ok` — see
    /// [`VerifyResponse::valid`].
    pub async fn verify(
        &self,
        proof: Proof,
        receipt: AnchorReceipt,
    ) -> Result<VerifyResponse, ClientError> {
        let url = self
            .base_url
            .join("/api/v1/verify")
            .map_err(|e| ClientError::InvalidUrl(e.to_string()))?;

        let response = self
            .http
            .post(url)
            .json(&VerifyRequest { proof, receipt })
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(Self::server_error(response).await);
        }

        Ok(response.json::<VerifyResponse>().await?)
    }

    async fn server_error(response: reqwest::Response) -> ClientError {
        let status = response.status();
        let body = match response.json::<ErrorResponse>().await {
            Ok(err) => err.error,
            Err(_) => "<response body was not the expected error shape>".to_string(),
        };
        ClientError::Server { status, body }
    }
}
