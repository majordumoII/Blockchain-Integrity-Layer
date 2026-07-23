//! Algorithm-agnostic digital signatures over proof digests.
//!
//! Mirrors the [`crate::hash`] module's shape: a [`SignatureAlgorithm`] tag
//! travels with every signature and public key so proofs stay verifiable
//! even if the default signing algorithm changes later, and multiple
//! signers (doctor + hospital, per the README's multi-party example) can
//! use different algorithms in the same proof.

use crate::error::{ProofError, Result};
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

/// Signature algorithms usable for proof attestation.
///
/// `Ed25519` is the default: deterministic (no per-signature RNG needed at
/// sign time beyond key generation, so no nonce-reuse key-leak class of
/// bug), fast to verify in batch, and small (64-byte signatures, 32-byte
/// keys). `EcdsaP256` is offered for environments that mandate NIST curves
/// (FIPS-adjacent enterprise/government contexts named in the README).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SignatureAlgorithm {
    /// Default. Deterministic, fast, small (64-byte sig, 32-byte key).
    #[default]
    Ed25519,
    /// NIST P-256 ECDSA, for FIPS-adjacent enterprise/government contexts.
    EcdsaP256,
}

impl std::fmt::Display for SignatureAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureAlgorithm::Ed25519 => write!(f, "Ed25519"),
            SignatureAlgorithm::EcdsaP256 => write!(f, "ECDSA-P256"),
        }
    }
}

/// A private signing key, tagged with its algorithm.
///
/// Wraps the underlying key bytes with `zeroize`-on-drop so key material is
/// wiped from memory as soon as it goes out of scope, rather than lingering
/// in freed heap/stack pages. `Debug` is intentionally not derived with the
/// key bytes visible — see the manual impl below.
#[derive(ZeroizeOnDrop)]
pub enum SigningPrivateKey {
    /// An Ed25519 signing key.
    Ed25519(#[zeroize(skip)] Box<SigningKey>),
    /// A NIST P-256 ECDSA signing key.
    EcdsaP256(#[zeroize(skip)] Box<p256::ecdsa::SigningKey>),
}

// `SigningKey`/`p256::ecdsa::SigningKey` already zeroize their own internal
// bytes on drop (both wrap `zeroize`-aware scalar types upstream); `#[zeroize(skip)]`
// above avoids double-zeroizing a type that doesn't implement `Zeroize` itself
// while still getting `ZeroizeOnDrop`'s drop-order guarantee for this enum.
impl std::fmt::Debug for SigningPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let alg = match self {
            SigningPrivateKey::Ed25519(_) => SignatureAlgorithm::Ed25519,
            SigningPrivateKey::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        };
        write!(f, "SigningPrivateKey({alg}, <redacted>)")
    }
}

impl SigningPrivateKey {
    /// Generates a new key pair for the given algorithm using a
    /// cryptographically secure RNG supplied by the caller.
    ///
    /// Taking the RNG as a parameter (rather than reaching for a global
    /// `OsRng` internally) keeps this function testable with a seeded RNG
    /// and makes the CSPRNG dependency explicit at every call site.
    pub fn generate<R: RngCore + CryptoRng>(algorithm: SignatureAlgorithm, rng: &mut R) -> Self {
        match algorithm {
            SignatureAlgorithm::Ed25519 => {
                SigningPrivateKey::Ed25519(Box::new(SigningKey::generate(rng)))
            }
            SignatureAlgorithm::EcdsaP256 => {
                SigningPrivateKey::EcdsaP256(Box::new(p256::ecdsa::SigningKey::random(rng)))
            }
        }
    }

    /// Which algorithm this key was generated for.
    #[must_use]
    pub fn algorithm(&self) -> SignatureAlgorithm {
        match self {
            SigningPrivateKey::Ed25519(_) => SignatureAlgorithm::Ed25519,
            SigningPrivateKey::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        }
    }

    /// Derives the corresponding public verifying key.
    #[must_use]
    pub fn public_key(&self) -> SigningPublicKey {
        match self {
            SigningPrivateKey::Ed25519(sk) => {
                SigningPublicKey::Ed25519(Box::new(sk.verifying_key()))
            }
            SigningPrivateKey::EcdsaP256(sk) => {
                SigningPublicKey::EcdsaP256(Box::new(*sk.verifying_key()))
            }
        }
    }

    /// Signs a digest's raw bytes, producing an algorithm-tagged signature.
    ///
    /// Signs over [`Digest::as_bytes`](crate::hash::Digest::as_bytes) — the
    /// commitment, never the original record — so signing never touches the
    /// sensitive data this crate exists to keep off-chain.
    #[must_use]
    pub fn sign(&self, digest_bytes: &[u8; 32]) -> ProofSignature {
        match self {
            SigningPrivateKey::Ed25519(sk) => {
                let sig = sk.sign(digest_bytes);
                ProofSignature::Ed25519(sig.to_bytes().to_vec())
            }
            SigningPrivateKey::EcdsaP256(sk) => {
                use p256::ecdsa::signature::Signer;
                let sig: p256::ecdsa::Signature = sk.sign(digest_bytes);
                ProofSignature::EcdsaP256(sig.to_bytes().to_vec())
            }
        }
    }
}

/// A public verifying key, tagged with its algorithm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SigningPublicKey {
    /// An Ed25519 verifying key.
    Ed25519(Box<VerifyingKey>),
    /// A NIST P-256 ECDSA verifying key.
    EcdsaP256(Box<p256::ecdsa::VerifyingKey>),
}

impl SigningPublicKey {
    /// Which algorithm this key verifies signatures for.
    #[must_use]
    pub fn algorithm(&self) -> SignatureAlgorithm {
        match self {
            SigningPublicKey::Ed25519(_) => SignatureAlgorithm::Ed25519,
            SigningPublicKey::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        }
    }

    /// Verifies `signature` over `digest_bytes` was produced by this key.
    ///
    /// Returns [`ProofError::SignatureAlgorithmMismatch`] rather than
    /// silently rejecting when the signature and key algorithms differ, so
    /// callers can distinguish "wrong key" from "wrong algorithm entirely"
    /// during rollout/migration between algorithms.
    ///
    /// # Errors
    ///
    /// Returns [`ProofError::InvalidSignatureEncoding`] if `signature`'s bytes
    /// aren't a validly-encoded signature for its algorithm,
    /// [`ProofError::SignatureAlgorithmMismatch`] if `signature`'s algorithm
    /// doesn't match this key's, or [`ProofError::InvalidSignature`] if the
    /// signature is well-formed but does not verify.
    pub fn verify(&self, digest_bytes: &[u8; 32], signature: &ProofSignature) -> Result<()> {
        match (self, signature) {
            (SigningPublicKey::Ed25519(vk), ProofSignature::Ed25519(sig_bytes)) => {
                let sig_bytes: &[u8; 64] = sig_bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ProofError::InvalidSignatureEncoding)?;
                let sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
                vk.verify(digest_bytes, &sig)
                    .map_err(|_| ProofError::InvalidSignature)
            }
            (SigningPublicKey::EcdsaP256(vk), ProofSignature::EcdsaP256(sig_bytes)) => {
                use p256::ecdsa::signature::Verifier;
                let sig = p256::ecdsa::Signature::from_slice(sig_bytes)
                    .map_err(|_| ProofError::InvalidSignatureEncoding)?;
                vk.verify(digest_bytes, &sig)
                    .map_err(|_| ProofError::InvalidSignature)
            }
            _ => Err(ProofError::SignatureAlgorithmMismatch {
                expected: self.algorithm(),
                actual: signature.algorithm(),
            }),
        }
    }
}

/// An algorithm-tagged signature over a digest.
///
/// Both variants store raw signature bytes as `Vec<u8>` — Ed25519 signatures
/// are always exactly 64 bytes and this is validated in [`SigningPublicKey::verify`]
/// via `Signature::from_bytes`, which itself requires a 64-byte input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofSignature {
    /// A 64-byte Ed25519 signature.
    Ed25519(Vec<u8>),
    /// A DER/fixed-width-encoded ECDSA P-256 signature.
    EcdsaP256(Vec<u8>),
}

impl ProofSignature {
    /// Which algorithm produced this signature.
    #[must_use]
    pub fn algorithm(&self) -> SignatureAlgorithm {
        match self {
            ProofSignature::Ed25519(_) => SignatureAlgorithm::Ed25519,
            ProofSignature::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    fn rng() -> impl RngCore + CryptoRng {
        OsRng
    }

    #[test]
    fn ed25519_sign_and_verify_roundtrip() {
        let sk = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let pk = sk.public_key();
        let digest = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"record");
        let sig = sk.sign(digest.as_bytes());
        assert!(pk.verify(digest.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn ecdsa_p256_sign_and_verify_roundtrip() {
        let sk = SigningPrivateKey::generate(SignatureAlgorithm::EcdsaP256, &mut rng());
        let pk = sk.public_key();
        let digest = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"record");
        let sig = sk.sign(digest.as_bytes());
        assert!(pk.verify(digest.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn tampered_digest_fails_verification() {
        let sk = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let pk = sk.public_key();
        let digest = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"record");
        let sig = sk.sign(digest.as_bytes());
        let tampered = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"tampered");
        assert!(pk.verify(tampered.as_bytes(), &sig).is_err());
    }

    #[test]
    fn wrong_key_fails_verification() {
        let sk_a = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let sk_b = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let digest = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"record");
        let sig = sk_a.sign(digest.as_bytes());
        assert!(sk_b.public_key().verify(digest.as_bytes(), &sig).is_err());
    }

    #[test]
    fn cross_algorithm_verify_is_reported_as_mismatch_not_invalid_signature() {
        let sk = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let other_sk = SigningPrivateKey::generate(SignatureAlgorithm::EcdsaP256, &mut rng());
        let digest = crate::hash::hash(crate::hash::HashAlgorithm::Blake3, b"record");
        let sig = sk.sign(digest.as_bytes());
        let err = other_sk
            .public_key()
            .verify(digest.as_bytes(), &sig)
            .unwrap_err();
        assert!(matches!(err, ProofError::SignatureAlgorithmMismatch { .. }));
    }

    #[test]
    fn debug_impl_does_not_print_key_bytes() {
        let sk = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut rng());
        let debug_str = format!("{sk:?}");
        assert!(debug_str.contains("redacted"));
    }
}
