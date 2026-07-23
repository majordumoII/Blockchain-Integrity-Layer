//! The unified error type returned by every fallible operation in this crate.

use thiserror::Error;

/// Errors produced anywhere in the proof generation / verification pipeline.
///
/// Variants deliberately avoid echoing input bytes or key material back to
/// the caller — error messages are logged and must never leak the record
/// data or secrets this crate exists to protect.
#[derive(Debug, Error)]
pub enum ProofError {
    /// A signature did not verify against its claimed public key and digest.
    #[error("signature verification failed")]
    InvalidSignature,

    /// A verifier used a different hash algorithm than the proof was built with.
    #[error(
        "hash algorithm mismatch: proof was hashed with {expected:?}, verifier used {actual:?}"
    )]
    HashAlgorithmMismatch {
        /// The algorithm the proof declares it used.
        expected: crate::hash::HashAlgorithm,
        /// The algorithm the verifier actually used.
        actual: crate::hash::HashAlgorithm,
    },

    /// A verifier used a different signature algorithm than the signer used.
    #[error(
        "signature algorithm mismatch: proof was signed with {expected:?}, verifier used {actual:?}"
    )]
    SignatureAlgorithmMismatch {
        /// The algorithm the public key expects.
        expected: crate::sign::SignatureAlgorithm,
        /// The algorithm the supplied signature was tagged with.
        actual: crate::sign::SignatureAlgorithm,
    },

    /// Key bytes could not be parsed into a valid key for their algorithm.
    #[error("malformed key material")]
    InvalidKey,

    /// Signature bytes could not be parsed into a valid signature for their algorithm.
    #[error("malformed signature encoding")]
    InvalidSignatureEncoding,

    /// Canonical (de)serialization of a proof failed.
    #[error("proof serialization failed")]
    Serialization(#[from] bincode::Error),

    /// A proof declared a format version this build does not know how to verify.
    #[error("proof failed schema/version validation: {0}")]
    UnsupportedVersion(u16),
}

/// Convenience alias for `Result<T, ProofError>`.
pub type Result<T> = core::result::Result<T, ProofError>;
