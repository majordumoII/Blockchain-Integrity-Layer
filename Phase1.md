# Phase 1: Core Proof Generation Engine

## Starting Point Decision

Evaluated whether "core proof generation engine" was the correct first build target versus
alternatives (chain anchoring, API layer, SDK ergonomics). Confirmed it is: the README's flow is
hash → sign → anchor → verify, and both anchoring and verification depend on a proof format that
doesn't exist yet. Building anywhere else first would mean coding against a shape that hasn't been
designed and will likely change. Proof generation is the dependency root of the whole system.

## What Was Built

Restructured the repo from a single placeholder binary crate (`cargo init` default) into a **Cargo
workspace**, anticipating the README's later phases (chain connectors, verification portal, SDK,
compliance modules) as sibling crates. First workspace member: `crates/proof-core`.

### `proof-core` crate

Algorithm-agnostic proof generation primitives — hashing, signing, and canonical serialization —
with no blockchain, network, or storage code. Modules:

- **`hash`** — `HashAlgorithm` enum (BLAKE3 default, SHA-256 alternate) + `Digest`, a 32-byte
  digest tagged with the algorithm that produced it. `verify()` always re-hashes and compares via
  constant-time equality rather than trusting a stored digest at face value.
- **`sign`** — `SignatureAlgorithm` enum (Ed25519 default, ECDSA P-256 alternate),
  `SigningPrivateKey` (zeroize-on-drop, `Debug` impl redacts key bytes), `SigningPublicKey`,
  `ProofSignature`. Keys are generated from a caller-supplied CSPRNG rather than a global `OsRng`,
  keeping the crate testable with seeded RNGs.
- **`proof`** — `Proof` and `ProofBuilder`. A `Proof` is a digest + one-or-more `Attestation`s
  (signer ID + public key + signature) + opaque caller metadata (record ID, org ID, etc.) +
  creation timestamp. Canonical wire format is bincode, versioned via `PROOF_FORMAT_VERSION` so
  future format changes don't break verification of already-anchored proofs.
- **`error`** — single `ProofError` enum. Error messages are written to never echo record data or
  key material, since they may end up in logs.

### Key design decisions

| Decision | Rationale |
|---|---|
| Algorithm-agnostic enums (not hardcoded types) for hash/sign | Anchored proofs can never be revised after the fact, so the format had to support algorithm rotation from day one rather than a breaking change later |
| BLAKE3 + Ed25519 as defaults | Faster than SHA-256/ECDSA, no nonce-reuse key-leak class of bug (Ed25519 is deterministic), strong Rust ecosystem support |
| SHA-256 + ECDSA P-256 offered as alternates | Chain/regulatory compatibility (FIPS-adjacent, existing Merkle schemes) without making it the default |
| Canonical bincode for serialization | Fast, compact, deterministic byte output — required since proof bytes (or their hash) may themselves be anchored |
| Pure-Rust `p256` over `secp256k1` (C bindings) | Avoids a C dependency for supply-chain/audit-surface reasons; chain-specific curves can be added additively in a later anchoring crate if needed |
| Hard invariant: no code path from raw record bytes to `Proof` | Mirrors the README's core promise that sensitive data never leaves the caller's system — enforced by constructor signatures (`ProofBuilder::new` takes a `Digest`, never bytes), not just convention |
| `verify_attestations()` requires all-of-N attestations | N-of-M threshold policy is a decision for a higher layer; this crate only enforces the baseline "every signature present is valid" |

## Verification

- `cargo build --workspace` — clean
- `cargo test --workspace` — 19/19 passing, including a proptest (`verify_never_panics_and_agrees_with_rehash`) and tamper-detection tests for both the digest and the signature
- `cargo clippy --workspace --all-targets -- -W clippy::pedantic` — zero warnings
- `cargo fmt --check` — clean
- `cargo build --workspace --release` — clean (LTO, `panic = "abort"`, stripped)
- `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` enforced crate-wide

Two real bugs were caught and fixed during this pass, not just style nits:
1. `ed25519-dalek` and `p256` don't derive `serde::Serialize`/`Deserialize` without an explicit
   `serde` feature flag — the crate failed to compile until the workspace `Cargo.toml` enabled it.
2. The first version of the tamper-detection test flipped the last byte of a serialized `Proof`
   and expected verification to fail — but the last field in the struct is `timestamp_unix`, not
   the signature, so the test was corrupting the wrong data and passing for the wrong reason. Fixed
   by targeting the digest and signature bytes directly instead of guessing a byte offset.

## Next Steps (not yet started)

- **Anchoring trait** — a chain-agnostic interface that a `Proof` gets submitted to, so backends
  (permissioned ledgers, public chains, or a local/file-backed anchor for testing) are pluggable
  rather than hard-coded.
- A local/file-backed anchor implementation would be the fastest way to exercise the full
  hash → sign → anchor → verify loop end-to-end before committing to any real chain integration.
