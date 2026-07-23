//! [`LocalLogAnchor`]: an append-only, hash-chained local file — the
//! first concrete [`ProofAnchor`] implementation.
//!
//! Each entry commits to the previous entry's hash (a simple Merkle-style
//! chain), so altering or deleting any past entry changes every hash from
//! that point forward — the same tamper-evidence property a real
//! blockchain provides, without a network/consensus dependency. This
//! exists to prove out the [`ProofAnchor`] trait boundary before
//! committing to any particular chain's SDK, wallet, or fee model; a
//! future chain-backed anchor implements the same trait and is a
//! drop-in replacement for callers.

use crate::anchor::{AnchorError, AnchorReceipt, ProofAnchor};
use proof_core::Proof;
use proof_core::hash::{self, HashAlgorithm};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

/// The hash of an empty/nonexistent previous entry, used as
/// `prev_entry_hash` for the first entry in a log. A fixed sentinel
/// (rather than e.g. all-zero bytes) so it's unambiguous in a hex dump
/// that a chain genuinely starts here rather than a previous hash simply
/// happening to be zero.
const GENESIS_HASH: [u8; 32] = *b"BIL-LOCAL-LOG-ANCHOR-GENESIS-V1\0";

/// One entry in the hash chain, as stored on disk.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LogEntry {
    index: u64,
    prev_entry_hash: [u8; 32],
    proof_bytes: Vec<u8>,
    /// `hash(index_le || prev_entry_hash || proof_bytes)` — recomputed
    /// and checked on every read, never trusted from disk at face value.
    entry_hash: [u8; 32],
}

impl LogEntry {
    fn compute_hash(index: u64, prev_entry_hash: &[u8; 32], proof_bytes: &[u8]) -> [u8; 32] {
        let mut buf = Vec::with_capacity(8 + 32 + proof_bytes.len());
        buf.extend_from_slice(&index.to_le_bytes());
        buf.extend_from_slice(prev_entry_hash);
        buf.extend_from_slice(proof_bytes);
        *hash::hash(HashAlgorithm::Blake3, &buf).as_bytes()
    }
}

/// An append-only, hash-chained anchor backed by a single local file.
///
/// Safe for concurrent use from multiple async tasks within one process
/// (writes are serialized via an internal mutex) but — being a plain
/// file, not a proper embedded database — is not safe for concurrent use
/// from multiple processes against the same path.
pub struct LocalLogAnchor {
    ledger_id: String,
    path: PathBuf,
    /// Held across the whole read-tip-then-append sequence in
    /// [`Self::anchor`] — without it, two concurrent `anchor()` calls
    /// could both read the same chain tip and append two entries
    /// claiming the same index/`prev_entry_hash`, corrupting the chain.
    /// `Arc` so the guard can be moved into a `spawn_blocking` closure
    /// alongside the path.
    write_lock: Arc<Mutex<()>>,
}

impl LocalLogAnchor {
    /// Opens (creating if absent) a hash-chained log at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`AnchorError::Storage`] if `path`'s parent directory
    /// doesn't exist or the file can't be created/opened.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AnchorError> {
        let path = path.into();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| AnchorError::Storage(format!("opening {}: {e}", path.display())))?;
        let ledger_id = format!("local-log:{}", path.display());
        Ok(Self {
            ledger_id,
            path,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Reads and hash-verifies every entry from the start of the log up
    /// to and including `up_to_index`, returning that final entry.
    ///
    /// Recomputing from the genesis on every call is deliberately
    /// simple rather than caching a "last known good" position — this is
    /// a verification path, not a hot path, and re-deriving trust from
    /// the chain's start on every check is the property that actually
    /// makes tampering detectable regardless of which entry was altered.
    fn read_and_verify_chain(path: &Path, up_to_index: u64) -> Result<LogEntry, AnchorError> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| AnchorError::Storage(format!("opening {}: {e}", path.display())))?;
        let mut prev_hash = GENESIS_HASH;
        let mut found: Option<LogEntry> = None;

        for expected_index in 0..=up_to_index {
            let entry = read_one_entry(&mut file)
                .map_err(|e| AnchorError::Storage(format!("reading log entry: {e}")))?
                .ok_or(AnchorError::NotFound)?;

            if entry.index != expected_index {
                return Err(AnchorError::IntegrityViolation {
                    position: expected_index,
                    reason: format!("expected index {expected_index}, found {}", entry.index),
                });
            }
            if entry.prev_entry_hash != prev_hash {
                return Err(AnchorError::IntegrityViolation {
                    position: expected_index,
                    reason: "prev_entry_hash does not match the preceding entry".to_string(),
                });
            }
            let recomputed =
                LogEntry::compute_hash(entry.index, &entry.prev_entry_hash, &entry.proof_bytes);
            if recomputed != entry.entry_hash {
                return Err(AnchorError::IntegrityViolation {
                    position: expected_index,
                    reason: "stored entry_hash does not match recomputed hash".to_string(),
                });
            }

            prev_hash = entry.entry_hash;
            if expected_index == up_to_index {
                found = Some(entry);
            }
        }

        found.ok_or(AnchorError::NotFound)
    }

    /// The hash the *next* appended entry should chain from, i.e. the
    /// last entry's hash, or [`GENESIS_HASH`] if the log is empty.
    fn tip_hash_and_next_index(path: &Path) -> Result<([u8; 32], u64), AnchorError> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| AnchorError::Storage(format!("opening {}: {e}", path.display())))?;
        let mut prev_hash = GENESIS_HASH;
        let mut next_index = 0u64;

        while let Some(entry) = read_one_entry(&mut file)
            .map_err(|e| AnchorError::Storage(format!("reading log entry: {e}")))?
        {
            prev_hash = entry.entry_hash;
            next_index = entry.index + 1;
        }
        Ok((prev_hash, next_index))
    }
}

#[async_trait::async_trait]
impl ProofAnchor for LocalLogAnchor {
    fn ledger_id(&self) -> &str {
        &self.ledger_id
    }

    async fn anchor(&self, proof: &Proof) -> Result<AnchorReceipt, AnchorError> {
        let proof_bytes = proof.to_canonical_bytes()?;
        let path = self.path.clone();
        let ledger_id = self.ledger_id.clone();

        // Held for the entire read-tip-then-append sequence below (see
        // the field doc on `write_lock`) — released only once the guard
        // moved into the blocking closure is dropped at its end.
        let lock = Arc::clone(&self.write_lock);
        let guard = lock.lock_owned().await;

        // File I/O is blocking; run it on a blocking-friendly thread so
        // this doesn't stall the async runtime while holding the lock
        // across a disk write.
        let guard_path = path.clone();
        let entry_index = task::spawn_blocking(move || -> Result<u64, AnchorError> {
            let _guard = guard; // held until this closure returns
            let (prev_hash, next_index) = Self::tip_hash_and_next_index(&guard_path)?;
            let entry_hash = LogEntry::compute_hash(next_index, &prev_hash, &proof_bytes);
            let entry = LogEntry {
                index: next_index,
                prev_entry_hash: prev_hash,
                proof_bytes,
                entry_hash,
            };
            append_entry(&guard_path, &entry)
                .map_err(|e| AnchorError::Storage(format!("appending log entry: {e}")))?;
            Ok(next_index)
        })
        .await
        .map_err(|e| AnchorError::Storage(format!("anchor task panicked: {e}")))??;

        Ok(AnchorReceipt {
            ledger_id,
            position_hex: format!("{entry_index:016x}"),
        })
    }

    async fn verify(&self, receipt: &AnchorReceipt, proof: &Proof) -> Result<(), AnchorError> {
        if receipt.ledger_id != self.ledger_id {
            return Err(AnchorError::WrongLedger {
                receipt_ledger: receipt.ledger_id.clone(),
                this_ledger: self.ledger_id.clone(),
            });
        }
        let index = u64::from_str_radix(&receipt.position_hex, 16).map_err(|_| {
            AnchorError::Storage(format!(
                "malformed receipt position: {}",
                receipt.position_hex
            ))
        })?;

        let path = self.path.clone();
        let entry = task::spawn_blocking(move || Self::read_and_verify_chain(&path, index))
            .await
            .map_err(|e| AnchorError::Storage(format!("verify task panicked: {e}")))??;

        let anchored =
            Proof::from_canonical_bytes(&entry.proof_bytes).map_err(AnchorError::Serialization)?;
        if anchored.digest() != proof.digest() {
            return Err(AnchorError::IntegrityViolation {
                position: index,
                reason: "supplied proof's digest does not match the anchored entry".to_string(),
            });
        }
        Ok(())
    }
}

/// Serializes one entry with a 4-byte little-endian length prefix (so
/// reads know exactly how many bytes to take, without scanning for a
/// delimiter that could theoretically appear inside proof bytes) and
/// appends it to the file at `path`.
fn append_entry(path: &Path, entry: &LogEntry) -> std::io::Result<()> {
    let encoded = bincode::serialize(entry).expect("LogEntry always serializes");
    let len = u32::try_from(encoded.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "entry of {} bytes exceeds the 4-byte length-prefix limit",
                encoded.len()
            ),
        )
    })?;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(&encoded)?;
    file.flush()
}

/// Reads one length-prefixed entry from the current file position,
/// advancing past it. Returns `Ok(None)` at a clean end-of-file (no
/// partial length prefix present).
fn read_one_entry(file: &mut std::fs::File) -> std::io::Result<Option<LogEntry>> {
    let mut len_buf = [0u8; 4];
    match file.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    let entry: LogEntry = bincode::deserialize(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proof_core::ProofBuilder;
    use proof_core::hash::{self as core_hash, HashAlgorithm};
    use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey};
    use rand_core::OsRng;

    fn sample_proof(seed: &str) -> Proof {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let digest = core_hash::hash(HashAlgorithm::Blake3, seed.as_bytes());
        ProofBuilder::new(digest, 1_753_270_000)
            .attest("test-signer", &key)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn anchor_and_verify_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = LocalLogAnchor::open(dir.path().join("log")).unwrap();

        let proof = sample_proof("record-1");
        let receipt = anchor.anchor(&proof).await.unwrap();
        assert_eq!(receipt.position_hex, "0000000000000000");

        anchor.verify(&receipt, &proof).await.unwrap();
    }

    #[tokio::test]
    async fn multiple_entries_chain_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = LocalLogAnchor::open(dir.path().join("log")).unwrap();

        let proof_a = sample_proof("record-a");
        let proof_b = sample_proof("record-b");
        let receipt_a = anchor.anchor(&proof_a).await.unwrap();
        let receipt_b = anchor.anchor(&proof_b).await.unwrap();

        assert_eq!(receipt_a.position_hex, "0000000000000000");
        assert_eq!(receipt_b.position_hex, "0000000000000001");

        anchor.verify(&receipt_a, &proof_a).await.unwrap();
        anchor.verify(&receipt_b, &proof_b).await.unwrap();
    }

    #[tokio::test]
    async fn tampering_with_an_earlier_entry_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let anchor = LocalLogAnchor::open(&path).unwrap();

        let proof_a = sample_proof("record-a");
        let proof_b = sample_proof("record-b");
        anchor.anchor(&proof_a).await.unwrap();
        let receipt_b = anchor.anchor(&proof_b).await.unwrap();

        // Flip a byte inside the raw file, landing inside the first
        // entry's encoded proof bytes (well past the 4-byte length
        // prefix + the fixed-size index/hash fields).
        let mut bytes = std::fs::read(&path).unwrap();
        let tamper_at = bytes.len() / 4;
        bytes[tamper_at] ^= 0xFF;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);

        // Verifying the *second* entry requires re-checking the chain
        // from the start, so tampering with the first entry must be
        // caught even though we're asking about the second.
        let result = anchor.verify(&receipt_b, &proof_b).await;
        assert!(matches!(
            result,
            Err(AnchorError::IntegrityViolation { .. })
        ));
    }

    #[tokio::test]
    async fn verify_rejects_a_receipt_from_a_different_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = LocalLogAnchor::open(dir.path().join("log")).unwrap();
        let proof = sample_proof("record-1");
        let mut receipt = anchor.anchor(&proof).await.unwrap();
        receipt.ledger_id = "local-log:/somewhere/else".to_string();

        let result = anchor.verify(&receipt, &proof).await;
        assert!(matches!(result, Err(AnchorError::WrongLedger { .. })));
    }

    #[tokio::test]
    async fn verify_rejects_an_out_of_range_position() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = LocalLogAnchor::open(dir.path().join("log")).unwrap();
        let proof = sample_proof("record-1");
        anchor.anchor(&proof).await.unwrap();

        let bogus_receipt = AnchorReceipt {
            ledger_id: anchor.ledger_id().to_string(),
            position_hex: "0000000000000099".to_string(),
        };
        let result = anchor.verify(&bogus_receipt, &proof).await;
        assert!(matches!(result, Err(AnchorError::NotFound)));
    }

    #[tokio::test]
    async fn concurrent_anchors_do_not_corrupt_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = Arc::new(LocalLogAnchor::open(dir.path().join("log")).unwrap());

        let mut handles = Vec::new();
        for i in 0..20 {
            let anchor = Arc::clone(&anchor);
            handles.push(tokio::spawn(async move {
                let proof = sample_proof(&format!("record-{i}"));
                let receipt = anchor.anchor(&proof).await.unwrap();
                (proof, receipt)
            }));
        }
        let mut pairs = Vec::new();
        for handle in handles {
            pairs.push(handle.await.unwrap());
        }

        // Every receipt must occupy a distinct position — concurrent
        // writers racing on the same "next index" would violate this.
        let mut positions: Vec<&str> = pairs.iter().map(|(_, r)| r.position_hex.as_str()).collect();
        positions.sort_unstable();
        positions.dedup();
        assert_eq!(
            positions.len(),
            20,
            "all 20 receipts must have unique positions"
        );

        // And the whole chain must still verify from genesis.
        for (proof, receipt) in &pairs {
            assert!(anchor.verify(receipt, proof).await.is_ok());
        }
    }
}
