//! Chain-agnostic proof anchoring: commits `proof-core` [`Proof`]s to a
//! tamper-evident ledger and lets receipts be verified back against it.
//!
//! Per the README's core stance — "the blockchain is an implementation
//! detail, not the product" — [`ProofAnchor`] is the trait boundary that
//! keeps anchoring backends swappable. [`LocalLogAnchor`] is the first
//! implementation: a hash-chained local file, proving out the trait and
//! giving genuine tamper-evidence without a network/consensus dependency,
//! before committing to any particular chain's SDK.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod anchor;
mod local_log;

pub use anchor::{AnchorError, AnchorReceipt, ProofAnchor};
pub use local_log::LocalLogAnchor;
