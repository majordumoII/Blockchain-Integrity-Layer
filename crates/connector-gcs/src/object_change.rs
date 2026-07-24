//! Canonical, serializable representation of a single GCS object-finalize
//! event, and its encoding into the bytes `proof-core`'s pipeline hashes.
//!
//! Mirrors `connector-postgres`'s `change.rs`: that crate's `RowChange` is
//! a canonical encoding of the row's actual column values, so
//! `pipeline.rs`'s BLAKE3 hash over it commits to real, re-derivable
//! content. The equivalent here cannot be "the object's raw bytes" —
//! that would defeat the entire point of streaming the hash (this
//! connector exists specifically because objects can be too large to
//! buffer). Instead, [`ObjectChange`] carries the object's identity
//! (bucket/name/generation) plus its content digest, which this
//! connector already computed by streaming the object through
//! `proof_core::hash::Hasher` (see [`crate::source::GcsSource::hash_object`]).
//!
//! This keeps the chain of trust intact and independently re-derivable: a
//! verifier with the object's bytes in hand streams them through the same
//! hash algorithm to get `content_digest`, reconstructs the same
//! `ObjectChange`, and re-hashes *that* with BLAKE3 (`pipeline.rs`'s
//! fixed hash step) to confirm it matches what was actually anchored —
//! exactly the same "canonical struct, not raw bytes, but still
//! independently re-derivable" shape `RowChange` already established for
//! Postgres, not a shortcut specific to this connector.

use proof_core::hash::{Digest, HashAlgorithm};
use serde::{Deserialize, Serialize};

/// An owned, serializable encoding of a [`Digest`] — `proof-core`'s own
/// `Digest` type doesn't derive these directly since it isn't meant to be
/// embedded inside other hashed structures in general, but this
/// connector's canonical record shape needs to carry one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedDigest {
    algorithm_tag: u8,
    bytes: [u8; 32],
}

impl From<Digest> for EncodedDigest {
    fn from(digest: Digest) -> Self {
        Self {
            algorithm_tag: encode_algorithm_tag(digest.algorithm()),
            bytes: *digest.as_bytes(),
        }
    }
}

/// `HashAlgorithm` is `#[non_exhaustive]` (new algorithms may be added as
/// new variants without a breaking-change bump), so this match cannot be
/// exhaustive at the type level — but it must stay exhaustive in
/// practice: silently reusing an existing tag for a future variant would
/// corrupt this connector's canonical encoding rather than fail loudly.
/// Panicking here (rather than returning a placeholder tag) is the
/// correct response to "this crate needs updating for a new algorithm,"
/// not a recoverable runtime condition.
fn encode_algorithm_tag(algorithm: HashAlgorithm) -> u8 {
    match algorithm {
        HashAlgorithm::Blake3 => 0,
        HashAlgorithm::Sha256 => 1,
        other => panic!(
            "connector-gcs's ObjectChange encoding does not yet have a tag for {other:?}; \
             add one rather than reusing an existing tag"
        ),
    }
}

/// A single GCS `OBJECT_FINALIZE` event, in the canonical shape that gets
/// bincode-encoded and hashed by `proof-service`'s pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectChange {
    pub bucket: String,
    pub object_name: String,
    /// The object generation, GCS's own monotonically-increasing version
    /// counter for this object name — included so the same object name
    /// being overwritten later produces a genuinely different
    /// `ObjectChange` (and thus a different proof), not an accidental
    /// digest collision with the previous version.
    pub generation: String,
    /// The object's content digest, computed by streaming its bytes
    /// through `proof_core::hash::Hasher` without ever buffering the
    /// whole object in memory.
    pub content_digest: EncodedDigest,
}

impl ObjectChange {
    /// Encodes this change to the canonical bytes that get hashed.
    ///
    /// # Errors
    ///
    /// Returns a `bincode::Error` if encoding fails (bincode encoding of
    /// this type's fields cannot practically fail, but the signature is
    /// kept fallible to match `bincode::serialize`'s own contract rather
    /// than papering over it with an `unwrap`).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proof_core::hash::{self as core_hash};

    fn sample_change(object_name: &str, generation: &str, content: &[u8]) -> ObjectChange {
        let digest = core_hash::hash(HashAlgorithm::Blake3, content);
        ObjectChange {
            bucket: "corporate-raw-docs".to_string(),
            object_name: object_name.to_string(),
            generation: generation.to_string(),
            content_digest: digest.into(),
        }
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let a = sample_change("reports/q3.pdf", "1", b"the report content");
        let b = sample_change("reports/q3.pdf", "1", b"the report content");
        assert_eq!(
            a.to_canonical_bytes().unwrap(),
            b.to_canonical_bytes().unwrap()
        );
    }

    #[test]
    fn different_content_produces_different_canonical_bytes() {
        let a = sample_change("reports/q3.pdf", "1", b"the report content");
        let b = sample_change("reports/q3.pdf", "1", b"a completely different report");
        assert_ne!(
            a.to_canonical_bytes().unwrap(),
            b.to_canonical_bytes().unwrap()
        );
    }

    #[test]
    fn same_content_different_generation_produces_different_canonical_bytes() {
        // Same object name and content re-uploaded later must not collide
        // with the earlier version's proof.
        let a = sample_change("reports/q3.pdf", "1", b"the report content");
        let b = sample_change("reports/q3.pdf", "2", b"the report content");
        assert_ne!(
            a.to_canonical_bytes().unwrap(),
            b.to_canonical_bytes().unwrap()
        );
    }

    #[test]
    fn different_algorithms_produce_different_encoded_digests_for_same_bytes() {
        let blake3_digest = core_hash::hash(HashAlgorithm::Blake3, b"same content");
        let sha256_digest = core_hash::hash(HashAlgorithm::Sha256, b"same content");
        let encoded_blake3: EncodedDigest = blake3_digest.into();
        let encoded_sha256: EncodedDigest = sha256_digest.into();
        assert_ne!(encoded_blake3, encoded_sha256);
    }

    #[test]
    fn content_digest_matches_independently_rehashing_the_same_bytes() {
        // Simulates a verifier: given the object's bytes, re-derive
        // content_digest the same way GcsSource::hash_object does (a
        // one-shot hash here stands in for the streamed Hasher path,
        // which proof-core's own tests already prove are bit-identical)
        // and confirm it matches what got embedded in the ObjectChange.
        let content = b"the report content";
        let change = sample_change("reports/q3.pdf", "1", content);

        let rehashed = core_hash::hash(HashAlgorithm::Blake3, content);
        let rehashed_encoded: EncodedDigest = rehashed.into();

        assert_eq!(change.content_digest, rehashed_encoded);
    }
}
