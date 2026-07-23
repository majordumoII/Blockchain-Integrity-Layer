# Instructions: Proving proof-core Works

This walks through how to see the Phase 1 proof generation engine (`crates/proof-core`) actually
work, end to end, on a real file — no Rust-reading required to interpret the result.

## Prerequisites

- Rust toolchain installed (`cargo --version` should work)
- A terminal in the repo root (`Blockchain-Integrity-Layer/`)

## Run the demo

```bash
cargo run -p proof-core --example demo -- <path-to-any-file>
```

Example, using the repo's own README as the sample "record":

```bash
cargo run -p proof-core --example demo -- README.md
```

## What it does

The demo takes the file you point it at and walks through the full hash → sign → build → verify
loop out loud:

1. **Hashes the file** with BLAKE3, prints the digest.
2. **Generates a signing key** (Ed25519) and signs the digest.
3. **Builds a proof** — digest + signer attestation + metadata — and shows its canonical byte size.
4. **Verifies the proof against the untouched file** — all checks should pass.
5. **Tampers with the record** (flips one byte) and re-verifies — the digest check should fail,
   while the signature and signer-identity checks still pass. This isolates *which* check is
   catching the problem.
6. **Simulates a forgery** — builds a second proof over the same digest but signed with a
   *different* key, pretending to be the original signer — and re-verifies. The signature is
   internally valid (it really is a correct signature, just from the wrong signer), but the
   signer-identity check against the trusted key fails. This is the actual attack a real verifier
   must defend against: a valid signature alone is not enough — you also have to check *whose*
   key it is.

Each step prints `[OK]` or `[FAIL]` next to what it checked, so the three failure modes a proof
system needs to catch are visible directly in the terminal output, not just asserted in test code.

## Run the automated test suite

```bash
cargo test --workspace
```

19 tests currently pass, covering hashing, signing, multi-party attestations, canonical
serialization round-trips, and tamper detection (including a property-based test that hashes and
compares random byte strings).

## Verify code quality gates

```bash
cargo clippy --workspace --all-targets -- -W clippy::pedantic   # must be zero warnings
cargo fmt --check                                                # must be clean
cargo build --workspace --release                                # release profile builds cleanly
```

## What this does *not* yet prove

- **No blockchain anchoring yet.** The proof never leaves memory in this demo — there is no
  network call, no chain, no persistence. That is the next phase.
- **No trusted-key registry.** The demo hardcodes "the trusted key" as the key it just generated,
  standing in for what would normally be a lookup against an organization's known keys. Deciding
  who counts as a trusted signer is explicitly left to a layer above `proof-core`.
- **No CLI/API surface for real use.** `examples/demo.rs` exists to demonstrate the crate's
  internals, not as a production entry point.
