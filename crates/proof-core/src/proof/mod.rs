//! The canonical [`Proof`] type: a hash commitment plus one or more
//! signatures over it, with the metadata needed to anchor and verify it
//! later — and nothing else.
//!
//! Per the README's core invariant, a `Proof` must never be constructible
//! from, or capable of carrying, the original record data. Every
//! constructor here takes already-hashed bytes; there is no code path from
//! raw record bytes straight into a stored/anchored `Proof`.

use crate::error::{ProofError, Result};
use crate::hash::Digest;
use crate::sign::{ProofSignature, SigningPrivateKey, SigningPublicKey};
use serde::{Deserialize, Serialize};

/// Wire/anchoring format version. Bump when [`Proof`]'s serialized shape
/// changes in a way that isn't purely additive, and keep old verifiers
/// working by matching on this in [`Proof::verify`] rather than deleting
/// the old path.
pub const PROOF_FORMAT_VERSION: u16 = 1;

/// One party's attestation to a [`Proof`]'s digest.
///
/// Kept separate from the top-level signature list item type so a signer's
/// public key travels with their signature — a verifier should not need a
/// separate key-lookup step to check a multi-party proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attestation {
    /// Opaque identifier for the signer (e.g. a doctor or hospital ID from
    /// the README's example). Not validated or interpreted by this crate —
    /// callers define their own identity namespace.
    pub signer_id: String,
    public_key: SigningPublicKey,
    signature: ProofSignature,
}

impl Attestation {
    /// The signer's public key.
    ///
    /// This crate only checks that the signature is internally
    /// consistent — that *some* key produced it. Deciding whether
    /// `public_key()` belongs to a trusted signer (e.g. matching it against
    /// an organization's known keys) is a policy decision left to the
    /// caller; exposing the key is what makes that check possible at all.
    #[must_use]
    pub fn public_key(&self) -> &SigningPublicKey {
        &self.public_key
    }

    fn verify(&self, digest: &Digest) -> Result<()> {
        self.public_key.verify(digest.as_bytes(), &self.signature)
    }
}

/// A cryptographic proof that a record existed, in a specific state, at a
/// specific time, attested to by one or more signers — without containing
/// the record itself.
///
/// This is what gets anchored on-chain and handed to verifiers. Its `Debug`
/// output is safe to log: by construction it can only ever contain a
/// digest, public keys, signatures, and caller-supplied metadata strings —
/// never the record's plaintext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proof {
    format_version: u16,
    digest: Digest,
    attestations: Vec<Attestation>,
    /// Caller-supplied metadata: org ID, record ID, schema version, etc.
    /// (the README's "Record ID / Service ID / Version number" fields).
    /// Stored as opaque key-value pairs so this crate doesn't need to know
    /// about any particular industry's schema.
    metadata: Vec<(String, String)>,
    /// Unix timestamp (seconds) of proof creation, set once at build time.
    timestamp_unix: u64,
}

impl Proof {
    /// The digest this proof attests to.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// All attestations (signer ID + public key + signature) on this proof.
    #[must_use]
    pub fn attestations(&self) -> &[Attestation] {
        &self.attestations
    }

    /// Caller-supplied metadata key-value pairs.
    #[must_use]
    pub fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    /// Seconds since the Unix epoch at proof creation.
    #[must_use]
    pub fn timestamp_unix(&self) -> u64 {
        self.timestamp_unix
    }

    /// Verifies every attestation on this proof against `self.digest()`.
    ///
    /// Does **not** verify the digest against any original record — that
    /// requires the record bytes, which this type never holds. Callers that
    /// have the record should additionally call
    /// [`crate::hash::verify`] with `self.digest()` and their record bytes.
    ///
    /// Requires at least one attestation and all attestations to verify
    /// (an N-of-M threshold policy, per the README's multi-party approval
    /// feature, is a policy decision for a higher layer — this method
    /// enforces the baseline "every signature present is valid" invariant
    /// only).
    ///
    /// # Errors
    ///
    /// Returns [`ProofError::UnsupportedVersion`] if this proof's format
    /// version isn't recognized, or [`ProofError::InvalidSignature`] if
    /// there are no attestations or any attestation fails to verify.
    pub fn verify_attestations(&self) -> Result<()> {
        if self.format_version != PROOF_FORMAT_VERSION {
            return Err(ProofError::UnsupportedVersion(self.format_version));
        }
        if self.attestations.is_empty() {
            return Err(ProofError::InvalidSignature);
        }
        for attestation in &self.attestations {
            attestation.verify(&self.digest)?;
        }
        Ok(())
    }

    /// Serializes this proof to canonical bytes for anchoring or storage.
    ///
    /// Uses fixed-int-width bincode so the same logical proof always
    /// produces identical bytes across processes/machines — required
    /// because those bytes (or a hash of them) may themselves be anchored.
    ///
    /// # Errors
    ///
    /// Returns [`ProofError::Serialization`] if bincode encoding fails.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).map_err(ProofError::Serialization)
    }

    /// Deserializes a proof previously produced by [`Self::to_canonical_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`ProofError::Serialization`] if `bytes` isn't valid
    /// canonical proof encoding.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(ProofError::Serialization)
    }
}

/// Builds a [`Proof`] by attaching one or more signer attestations to a
/// digest.
///
/// Only takes a [`Digest`], never raw record bytes — see the module-level
/// docs for why that's a hard invariant rather than a convention.
pub struct ProofBuilder {
    digest: Digest,
    attestations: Vec<Attestation>,
    metadata: Vec<(String, String)>,
    timestamp_unix: u64,
}

impl ProofBuilder {
    /// Starts building a proof for `digest`, timestamped at `timestamp_unix`
    /// (seconds since epoch).
    ///
    /// The timestamp is taken as a parameter rather than read from the
    /// system clock internally, keeping this crate free of a `SystemTime`
    /// dependency and making proof construction deterministic in tests.
    /// Callers building real proofs should pass
    /// `SystemTime::now().duration_since(UNIX_EPOCH)`.
    #[must_use]
    pub fn new(digest: Digest, timestamp_unix: u64) -> Self {
        Self {
            digest,
            attestations: Vec::new(),
            metadata: Vec::new(),
            timestamp_unix,
        }
    }

    /// Adds a signer's attestation over this proof's digest.
    ///
    /// Signs internally (rather than accepting a pre-made [`ProofSignature`])
    /// so it is not possible to attach a signature computed over the wrong
    /// bytes.
    #[must_use]
    pub fn attest(mut self, signer_id: impl Into<String>, key: &SigningPrivateKey) -> Self {
        let signature = key.sign(self.digest.as_bytes());
        self.attestations.push(Attestation {
            signer_id: signer_id.into(),
            public_key: key.public_key(),
            signature,
        });
        self
    }

    /// Attaches an opaque metadata key-value pair (record ID, org ID, ...).
    #[must_use]
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }

    /// Finalizes the proof. Fails if no attestations were added — an
    /// unsigned "proof" attests to nothing and must not be constructible.
    ///
    /// # Errors
    ///
    /// Returns [`ProofError::InvalidSignature`] if no attestations were added.
    pub fn build(self) -> Result<Proof> {
        if self.attestations.is_empty() {
            return Err(ProofError::InvalidSignature);
        }
        Ok(Proof {
            format_version: PROOF_FORMAT_VERSION,
            digest: self.digest,
            attestations: self.attestations,
            metadata: self.metadata,
            timestamp_unix: self.timestamp_unix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{self, HashAlgorithm};
    use crate::sign::SignatureAlgorithm;
    use rand_core::OsRng;

    fn sample_digest() -> Digest {
        hash::hash(HashAlgorithm::Blake3, b"patient record #1024")
    }

    #[test]
    fn single_signer_proof_builds_and_verifies() {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .metadata("hospital_id", "hospital-xyz")
            .build()
            .unwrap();

        assert!(proof.verify_attestations().is_ok());
        assert_eq!(proof.attestations().len(), 1);
        assert_eq!(
            proof.metadata(),
            &[("hospital_id".to_string(), "hospital-xyz".to_string())]
        );
    }

    #[test]
    fn multi_party_proof_requires_all_signatures_valid() {
        let doctor_key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let hospital_key = SigningPrivateKey::generate(SignatureAlgorithm::EcdsaP256, &mut OsRng);
        let proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &doctor_key)
            .attest("hospital-xyz", &hospital_key)
            .build()
            .unwrap();

        assert_eq!(proof.attestations().len(), 2);
        assert!(proof.verify_attestations().is_ok());
    }

    #[test]
    fn unsigned_proof_cannot_be_built() {
        let result = ProofBuilder::new(sample_digest(), 0).build();
        assert!(matches!(result, Err(ProofError::InvalidSignature)));
    }

    #[test]
    fn canonical_bytes_roundtrip() {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .build()
            .unwrap();

        let bytes = proof.to_canonical_bytes().unwrap();
        let restored = Proof::from_canonical_bytes(&bytes).unwrap();
        assert!(restored.verify_attestations().is_ok());
        assert_eq!(restored.digest(), proof.digest());
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .metadata("k", "v")
            .build()
            .unwrap();

        let bytes_a = proof.to_canonical_bytes().unwrap();
        let bytes_b = proof.to_canonical_bytes().unwrap();
        assert_eq!(bytes_a, bytes_b);
    }

    #[test]
    fn tampered_digest_bytes_fail_signature_verification() {
        // The digest is the first field bincode writes (algorithm tag +
        // 32 digest bytes), so corrupting an early byte hits it directly
        // rather than relying on where signature bytes happen to land.
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .build()
            .unwrap();

        let mut bytes = proof.to_canonical_bytes().unwrap();
        // format_version: u16 (2 bytes, little-endian for bincode 1.x's
        // fixint encoding) is the very first field; the digest follows.
        let digest_byte_offset = 2 + 4; // format_version + HashAlgorithm enum tag
        bytes[digest_byte_offset] ^= 0xFF;

        // Corrupting a byte may also just break bincode framing, which is
        // an equally acceptable rejection outcome, so only assert in the
        // Ok case rather than requiring `from_canonical_bytes` to succeed.
        if let Ok(restored) = Proof::from_canonical_bytes(&bytes) {
            assert!(restored.verify_attestations().is_err());
        }
    }

    #[test]
    fn tampered_signature_bytes_fail_verification() {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let mut proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .build()
            .unwrap();

        // Corrupt the signature directly (rather than guessing a byte
        // offset in the serialized form) so the test's intent can't drift
        // out of sync with the struct's field order.
        match &mut proof.attestations[0].signature {
            ProofSignature::Ed25519(bytes) | ProofSignature::EcdsaP256(bytes) => bytes[0] ^= 0xFF,
        }

        assert!(proof.verify_attestations().is_err());
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
        let mut proof = ProofBuilder::new(sample_digest(), 1_753_270_000)
            .attest("dr-smith", &key)
            .build()
            .unwrap();
        proof.format_version = 9999;
        assert!(matches!(
            proof.verify_attestations(),
            Err(ProofError::UnsupportedVersion(9999))
        ));
    }
}
