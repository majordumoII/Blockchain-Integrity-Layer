//! Algorithm-agnostic content hashing.
//!
//! A [`Digest`] always carries the [`HashAlgorithm`] tag that produced it, so
//! a proof remains self-describing and verifiable decades after creation
//! even if the default algorithm changes.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Hash algorithms usable for record commitments.
///
/// `Blake3` is the default: it is faster than SHA-256 (SIMD/tree-parallel),
/// has a 256-bit security level, and has no known extension-length-style
/// footguns. `Sha256` is offered for chains/regulatory contexts that require
/// it explicitly (e.g. matching an existing Merkle scheme on a target chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HashAlgorithm {
    /// Default. Faster than SHA-256 and SIMD/tree-parallelizable.
    #[default]
    Blake3,
    /// For contexts that require it explicitly (e.g. an existing on-chain Merkle scheme).
    Sha256,
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashAlgorithm::Blake3 => write!(f, "BLAKE3"),
            HashAlgorithm::Sha256 => write!(f, "SHA-256"),
        }
    }
}

/// A 32-byte digest tagged with the algorithm that produced it.
///
/// Both supported algorithms happen to produce 32-byte outputs, which keeps
/// this type fixed-size and `Copy`. If a variable-length algorithm is added
/// later, this becomes an enum-of-arrays rather than a flat struct — that
/// change is confined to this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest {
    algorithm: HashAlgorithm,
    bytes: [u8; 32],
}

impl Digest {
    /// Which algorithm produced this digest.
    #[must_use]
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// The raw 32-byte digest value.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Hex-encodes the digest for display/logging. Digests are not secret —
    /// only the pre-image (the record) is — so this is safe to log.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for byte in self.bytes {
            use fmt::Write;
            let _ = write!(s, "{byte:02x}");
        }
        s
    }

    /// Constructs a digest from raw bytes without hashing anything.
    ///
    /// Intended for deserializing/reconstructing a digest that was already
    /// computed elsewhere (e.g. read off an anchored proof). Does not
    /// validate that `bytes` is a genuine output of `algorithm` — callers
    /// that need that guarantee must re-hash and compare.
    #[must_use]
    pub fn from_raw(algorithm: HashAlgorithm, bytes: [u8; 32]) -> Self {
        Self { algorithm, bytes }
    }
}

/// Hashes `data` under the given algorithm, producing a tagged [`Digest`].
#[must_use]
pub fn hash(algorithm: HashAlgorithm, data: &[u8]) -> Digest {
    match algorithm {
        HashAlgorithm::Blake3 => {
            let bytes = *blake3::hash(data).as_bytes();
            Digest { algorithm, bytes }
        }
        HashAlgorithm::Sha256 => {
            use sha2::{Digest as _, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(data);
            let out = hasher.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&out);
            Digest { algorithm, bytes }
        }
    }
}

/// Re-hashes `data` under `digest`'s algorithm and checks it matches.
///
/// This is the building block record-integrity checks are built on: proof
/// verification never trusts a stored digest at face value against new
/// data — it always recomputes.
#[must_use]
pub fn verify(digest: &Digest, data: &[u8]) -> bool {
    use subtle::ConstantTimeEq;

    let recomputed = hash(digest.algorithm, data);
    // Digest equality is not on a secret value (the hash of possibly-public
    // metadata), but comparing digests via `subtle` costs nothing and
    // removes any need to reason about it later.
    recomputed.bytes.ct_eq(&digest.bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_roundtrip_verifies() {
        let data = b"patient record #1024";
        let d = hash(HashAlgorithm::Blake3, data);
        assert!(verify(&d, data));
        assert!(!verify(&d, b"tampered record #1024"));
    }

    #[test]
    fn sha256_roundtrip_verifies() {
        let data = b"loan document v3";
        let d = hash(HashAlgorithm::Sha256, data);
        assert!(verify(&d, data));
        assert!(!verify(&d, b"loan document v4"));
    }

    #[test]
    fn algorithms_produce_different_digests_for_same_data() {
        let data = b"same input";
        let b3 = hash(HashAlgorithm::Blake3, data);
        let sha = hash(HashAlgorithm::Sha256, data);
        assert_ne!(b3.as_bytes(), sha.as_bytes());
    }

    #[test]
    fn to_hex_is_lowercase_and_64_chars() {
        let d = hash(HashAlgorithm::Blake3, b"x");
        let hex = d.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    proptest::proptest! {
        #[test]
        fn verify_never_panics_and_agrees_with_rehash(data: Vec<u8>, other: Vec<u8>) {
            let d = hash(HashAlgorithm::Blake3, &data);
            let expected = data == other;
            proptest::prop_assert_eq!(verify(&d, &other), expected);
        }
    }
}
