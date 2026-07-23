# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

This is a Cargo **workspace**, in early implementation of the vision described in `README.md`.
`crates/proof-core` is the first crate: algorithm-agnostic proof generation (hashing + signing +
canonical serialization), with no blockchain, network, or storage code yet. Future crates (chain
anchoring, verification API, SDK ergonomics layer) will be added as workspace members alongside it —
see `Cargo.toml`'s `[workspace] members` list for the current set.

## Commands

Run from the repo root (workspace root):

```bash
cargo build --workspace              # build all crates
cargo build --workspace --release    # release build (LTO, panic=abort, stripped)
cargo test --workspace               # run all tests
cargo test -p proof-core <name>      # run a single test by name in one crate
cargo clippy --workspace --all-targets -- -W clippy::pedantic   # this crate must stay pedantic-clean
cargo fmt                            # format
cargo fmt --check                    # verify formatting in CI
```

`proof-core` is held to `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]`, and is expected to
pass `clippy::pedantic` with zero warnings — keep new code in that crate to the same bar. Every
public `Result`-returning function needs a `# Errors` doc section; every public getter/builder
method needs `#[must_use]` (clippy pedantic enforces both).

## Architecture: `proof-core`

`proof-core` is the dependency root of the whole system — it defines what a `Proof` *is*,
independent of any blockchain, storage, or API concern. Downstream crates (anchoring, verification
service, SDK) will depend on this crate's types rather than each inventing their own proof shape.

Module layout:
- `hash` — `HashAlgorithm` (BLAKE3 default, SHA-256 alternate) + `Digest`, a 32-byte tagged hash.
  `verify()` always re-hashes and compares rather than trusting a stored digest.
- `sign` — `SignatureAlgorithm` (Ed25519 default, ECDSA P-256 alternate), `SigningPrivateKey`
  (zeroize-on-drop, `Debug` redacts key bytes), `SigningPublicKey`, `ProofSignature`.
- `proof` — `Proof` (digest + one-or-more `Attestation`s + opaque metadata + timestamp) and
  `ProofBuilder`. Canonical wire format is bincode, versioned via `PROOF_FORMAT_VERSION`.
- `error` — single `ProofError` enum for the crate; messages never echo record data or key material.

**Hard invariant, enforced by the type signatures, not just convention**: there is no code path from
raw record bytes into a `Proof`. `ProofBuilder::new` takes a `Digest`, never bytes; signing methods
take digest bytes, never the record. This mirrors the README's core promise that sensitive data never
leaves the caller's system — a reviewer should treat any change that threads raw record bytes deeper
into this crate as a design regression, not a convenience worth accepting.

**Algorithm-agnostic by design**: every crypto choice (`HashAlgorithm`, `SignatureAlgorithm`) is a
tagged enum stored alongside its output, not a hardcoded type. This was a deliberate tradeoff
(more upfront enum/match boilerplate) to keep anchored proofs verifiable indefinitely even if the
default algorithm changes later — anchored proofs cannot be revised after the fact, so the format
had to get this right from the first version. New algorithms are added as new enum variants, not by
replacing existing ones.

**Multi-party attestation**: `ProofBuilder::attest()` can be called multiple times (e.g. doctor +
hospital, per the README's example) with different signers using different algorithms in the same
proof. `Proof::verify_attestations()` currently requires *all* attestations present to verify
(all-of-N) — N-of-M threshold policy is intentionally left to a higher layer, not baked into this
crate.

## Product Vision (from README.md)

The intended product is a **"Blockchain Integrity Layer"** — a Rust SDK/service that lets
organizations prove facts about their data (e.g. "customer is over 18", "shipment stayed below 8°C",
"income > $120k") without revealing the underlying sensitive data itself.

Key architectural principles driving design decisions:

- **Data never moves.** Existing databases/microservices stay as-is. The integrity layer sits
  alongside the application/data-access layer, intercepting writes to hash → sign → anchor →
  confirm, without requiring any data migration.
- **The blockchain is an implementation detail**, not the product. Anchoring should be
  abstractable/pluggable across backends (permissioned ledgers, public chains) rather than
  hard-coded to one chain.
- **The product is the SDK/API ergonomics** — integration should feel as simple as
  `integrity.commit(record)?` or a single annotation/macro, not smart-contract-level complexity.
- Target consumers are industry-agnostic: healthcare, supply chain, banking, AI/ML provenance,
  government records — the same proof primitives apply across all of them.

Full context and rationale (problem statement, phased roadmap, revenue model, alternate product
framings that were considered) is in `README.md`; `README.original.md` is an earlier draft/brainstorm
of the same vision kept for reference.
