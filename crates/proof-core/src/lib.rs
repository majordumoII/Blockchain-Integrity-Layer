//! Core proof generation primitives for the Blockchain Integrity Layer.
//!
//! This crate is the dependency root of the system: it defines what a proof
//! *is* (hash + signature(s) + metadata, algorithm-tagged and canonically
//! serializable) independent of any blockchain, storage, or API concern.
//! Anchoring, verification services, and the SDK all build on top of the
//! [`Proof`] type defined here rather than each inventing their own shape.
//!
//! Deliberately out of scope for this crate: network I/O, chain-specific
//! anchoring, and storage. Those belong in downstream crates so this crate's
//! dependency tree — and therefore its audit surface — stays minimal.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::module_inception)]

pub mod error;
pub mod hash;
pub mod proof;
pub mod sign;

pub use error::{ProofError, Result};
pub use hash::{Digest, HashAlgorithm};
pub use proof::{Proof, ProofBuilder};
pub use sign::{ProofSignature, SignatureAlgorithm, SigningPrivateKey, SigningPublicKey};
