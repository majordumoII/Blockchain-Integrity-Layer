//! Algorithm-agnostic content hashing.
//!
//! A [`Digest`] always carries the [`HashAlgorithm`] tag that produced it, so
//! a proof remains self-describing and verifiable decades after creation
//! even if the default algorithm changes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Read};

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
///
/// For content too large to hold in memory as one `&[u8]` (e.g. a
/// multi-gigabyte object from a blob store), use [`HashAlgorithm::hasher`]
/// instead and feed it incrementally.
#[must_use]
pub fn hash(algorithm: HashAlgorithm, data: &[u8]) -> Digest {
    let mut hasher = algorithm.hasher();
    hasher.update(data);
    hasher.finalize()
}

/// One in-progress incremental hash computation.
///
/// Exists so callers with content too large (or arriving in chunks too
/// awkward) to hold as a single `&[u8]` — e.g. streaming an object
/// straight from a cloud storage SDK's response body — can hash it
/// without ever buffering the whole thing, by repeatedly calling
/// [`Self::update`] as bytes arrive and [`Self::finalize`] once done.
/// Produces bit-for-bit the same [`Digest`] as calling [`hash`] on the
/// concatenation of every chunk passed to `update` — this is the same
/// invariant [`HashAlgorithm::Blake3`]'s and [`HashAlgorithm::Sha256`]'s
/// own incremental APIs already guarantee; this type only threads the
/// algorithm tag through so the result comes out as a [`Digest`], not a
/// bare hash implementation's own output type.
pub enum Hasher {
    /// Wraps `blake3`'s own incremental hasher.
    Blake3(Box<blake3::Hasher>),
    /// Wraps `sha2`'s own incremental hasher.
    Sha256(Box<sha2::Sha256>),
}

impl Hasher {
    /// Feeds more bytes into this hash computation. May be called any
    /// number of times before [`Self::finalize`].
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Blake3(hasher) => {
                hasher.update(data);
            }
            Hasher::Sha256(hasher) => {
                use sha2::Digest as _;
                hasher.update(data);
            }
        }
    }

    /// Consumes every byte fed to [`Self::update`] so far and produces the
    /// final tagged [`Digest`].
    #[must_use]
    pub fn finalize(self) -> Digest {
        match self {
            Hasher::Blake3(hasher) => Digest {
                algorithm: HashAlgorithm::Blake3,
                bytes: *hasher.finalize().as_bytes(),
            },
            Hasher::Sha256(hasher) => {
                use sha2::Digest as _;
                let out = hasher.finalize();
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&out);
                Digest {
                    algorithm: HashAlgorithm::Sha256,
                    bytes,
                }
            }
        }
    }
}

impl HashAlgorithm {
    /// Starts a fresh incremental [`Hasher`] for this algorithm. See
    /// [`Hasher`] for when to reach for this instead of [`hash`].
    #[must_use]
    pub fn hasher(self) -> Hasher {
        match self {
            HashAlgorithm::Blake3 => Hasher::Blake3(Box::new(blake3::Hasher::new())),
            HashAlgorithm::Sha256 => {
                use sha2::Digest as _;
                Hasher::Sha256(Box::new(sha2::Sha256::new()))
            }
        }
    }
}

/// Hashes the entirety of `reader` under the given algorithm without ever
/// holding its full contents in memory at once — reads and feeds it to a
/// [`Hasher`] in fixed-size chunks.
///
/// For a caller already receiving bytes as async chunks (e.g. a cloud
/// storage SDK's streamed response body) rather than a [`Read`], calling
/// [`HashAlgorithm::hasher`] directly and feeding each chunk to
/// [`Hasher::update`] as it arrives is the more natural fit — this
/// function is the synchronous, "I already have something `Read`-shaped"
/// convenience built on the same primitive.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if reading from `reader` fails
/// partway through; no partial [`Digest`] is returned in that case, since
/// a digest over an incomplete read would silently misrepresent the
/// source's actual content.
pub fn hash_reader(algorithm: HashAlgorithm, mut reader: impl Read) -> io::Result<Digest> {
    let mut hasher = algorithm.hasher();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
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

    #[test]
    fn blake3_incremental_hasher_matches_one_shot() {
        let data = b"a chunk that would arrive across several update() calls";
        let one_shot = hash(HashAlgorithm::Blake3, data);

        let mut hasher = HashAlgorithm::Blake3.hasher();
        hasher.update(&data[..10]);
        hasher.update(&data[10..30]);
        hasher.update(&data[30..]);
        let streamed = hasher.finalize();

        assert_eq!(one_shot, streamed);
    }

    #[test]
    fn sha256_incremental_hasher_matches_one_shot() {
        let data = b"a chunk that would arrive across several update() calls";
        let one_shot = hash(HashAlgorithm::Sha256, data);

        let mut hasher = HashAlgorithm::Sha256.hasher();
        hasher.update(&data[..10]);
        hasher.update(&data[10..30]);
        hasher.update(&data[30..]);
        let streamed = hasher.finalize();

        assert_eq!(one_shot, streamed);
    }

    #[test]
    fn hasher_with_zero_updates_matches_hashing_empty_slice() {
        let one_shot = hash(HashAlgorithm::Blake3, b"");
        let streamed = HashAlgorithm::Blake3.hasher().finalize();
        assert_eq!(one_shot, streamed);
    }

    #[test]
    fn hash_reader_matches_one_shot_across_a_multi_chunk_read() {
        // Larger than hash_reader's internal 64 KiB read buffer, so this
        // exercises more than one read() iteration, not just a single
        // buffer's worth.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let one_shot = hash(HashAlgorithm::Blake3, &data);

        let streamed = hash_reader(HashAlgorithm::Blake3, data.as_slice())
            .expect("reading from a slice cannot fail");

        assert_eq!(one_shot, streamed);
    }

    #[test]
    fn hash_reader_propagates_io_errors() {
        struct FailingReader;
        impl std::io::Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("simulated read failure"))
            }
        }

        let result = hash_reader(HashAlgorithm::Blake3, FailingReader);
        assert!(result.is_err());
    }

    proptest::proptest! {
        #[test]
        fn hasher_chunking_never_changes_the_result(data: Vec<u8>, split_at: usize) {
            let one_shot = hash(HashAlgorithm::Blake3, &data);

            let split = split_at % (data.len() + 1);
            let mut hasher = HashAlgorithm::Blake3.hasher();
            hasher.update(&data[..split]);
            hasher.update(&data[split..]);
            let streamed = hasher.finalize();

            proptest::prop_assert_eq!(one_shot, streamed);
        }
    }
}
