//! [`ProofAnchor`]: the chain-agnostic abstraction over "where anchored
//! proofs actually get committed" — a local hash-chained log today, a
//! permissioned ledger or public chain later, without any caller-visible
//! difference beyond which impl is constructed. Matches the README's
//! explicit stance that the blockchain is an implementation detail: "the
//! product is the SDK/API ergonomics," not any one chain's SDK.

use proof_core::Proof;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A tamper-evident receipt proving a [`Proof`] was anchored at a
/// specific position in a specific ledger.
///
/// Deliberately opaque beyond `ledger_id` and `position_hex` — a caller
/// should not need to know whether `position_hex` is a local log's entry
/// hash, an Ethereum transaction hash, or a Hyperledger block/tx
/// reference to store or display a receipt. Only the anchor
/// implementation that produced it knows how to interpret it well enough
/// to verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorReceipt {
    /// Identifies which ledger/anchor instance produced this receipt
    /// (e.g. `"local-log:./anchor.log"`), so a receipt is self-describing
    /// about where to go to verify it rather than requiring the caller to
    /// already know.
    pub ledger_id: String,
    /// Opaque, implementation-defined position reference, hex-encoded so
    /// it's safe to log/display/store as a plain string regardless of
    /// what it represents internally.
    pub position_hex: String,
}

impl fmt::Display for AnchorReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.ledger_id, self.position_hex)
    }
}

/// Errors from anchoring or verifying against a ledger.
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    /// The proof could not be canonically encoded before anchoring.
    #[error("failed to serialize proof for anchoring: {0}")]
    Serialization(#[from] proof_core::ProofError),

    /// The underlying ledger's storage (file, database, RPC call, ...) failed.
    #[error("underlying ledger storage error: {0}")]
    Storage(String),

    /// No entry exists at the position a receipt referenced.
    #[error("no entry found at the position referenced by this receipt")]
    NotFound,

    /// The ledger's tamper-evidence chain failed to verify at some position.
    #[error("ledger integrity check failed at position {position}: {reason}")]
    IntegrityViolation {
        /// The position at which verification failed.
        position: u64,
        /// Human-readable detail on what specifically didn't check out.
        reason: String,
    },

    /// A receipt was presented to an anchor instance that didn't produce it.
    #[error(
        "receipt is for a different ledger (receipt: {receipt_ledger}, this anchor: {this_ledger})"
    )]
    WrongLedger {
        /// The ledger ID recorded on the receipt.
        receipt_ledger: String,
        /// This anchor instance's own ledger ID.
        this_ledger: String,
    },
}

/// Commits [`Proof`]s to a tamper-evident ledger and lets a receipt be
/// checked back against it later.
///
/// Implementations decide their own notion of "position" and "ledger
/// identity" — see [`AnchorReceipt`] — but must uphold one invariant:
/// once [`Self::anchor`] returns a receipt, [`Self::verify`] against that
/// receipt must keep confirming the same `Proof` for as long as the
/// underlying ledger exists, and must detect (not silently ignore) any
/// tampering with that entry or anything the chain depends on before it.
///
/// `verify` takes the `Proof` being checked as an argument rather than
/// reconstructing one from ledger data, because not every backend stores
/// enough to reconstruct it: an on-chain anchor should only ever commit a
/// [`Proof`]'s 32-byte digest (matching the README's "the blockchain
/// never stores raw content" stance), so it has nothing to rebuild a full
/// `Proof` from. Re-deriving trust by re-hashing the caller-supplied
/// proof and checking it against what the ledger actually committed
/// mirrors `proof_core::hash::verify`'s digest/data split.
#[async_trait::async_trait]
pub trait ProofAnchor: Send + Sync {
    /// A stable identifier for this anchor instance, echoed into every
    /// [`AnchorReceipt`] it produces.
    fn ledger_id(&self) -> &str;

    /// Commits `proof` to the ledger, returning a receipt that can later
    /// be passed to [`Self::verify`].
    ///
    /// # Errors
    ///
    /// Returns [`AnchorError::Serialization`] if `proof` cannot be
    /// canonically encoded, or [`AnchorError::Storage`] if the
    /// underlying ledger write fails.
    async fn anchor(&self, proof: &Proof) -> Result<AnchorReceipt, AnchorError>;

    /// Confirms that `proof` is the exact proof committed at `receipt`'s
    /// position, checking the ledger's tamper-evidence chain up to that
    /// point.
    ///
    /// # Errors
    ///
    /// Returns [`AnchorError::WrongLedger`] if `receipt` names a
    /// different ledger, [`AnchorError::NotFound`] if no entry exists at
    /// its position, or [`AnchorError::IntegrityViolation`] if the chain
    /// leading to that position has been tampered with, or if `proof`
    /// does not match what was actually anchored there.
    async fn verify(&self, receipt: &AnchorReceipt, proof: &Proof) -> Result<(), AnchorError>;
}
