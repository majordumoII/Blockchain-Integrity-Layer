# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

This is a Cargo **workspace**, in active implementation of the vision described in `README.md`.
Three crates exist so far — `proof-core` (proof generation), `proof-connectors` (connector traits),
`connector-postgres` (first concrete connector) — with more planned (see below). See `Cargo.toml`'s
`[workspace] members` list for the current authoritative set.

**Deployment topology decision (load-bearing, don't relitigate casually):** the project will ship as
a single `proof-service` binary for now — one process running the connector(s), Prometheus
`/metrics`, and the UI — rather than splitting connectors and the API/UI into separate services. This
was a deliberate simplicity-now tradeoff: the split is meant to stay *cheap* later because
`proof-connectors`' `ProofSink` trait is the seam a future split would replace (swap the in-process
broadcast+history impl for a message bus), not because a split isn't wanted eventually. Don't
casually merge `RecordSource`/`ProofSink` consumers directly into connector internals — that's the
coupling that would make a later split expensive.

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

Every crate in this workspace is held to `#![forbid(unsafe_code)]` and expected to pass
`clippy::pedantic` with zero warnings — keep new code to the same bar. Every public `Result`-returning
function needs a `# Errors` doc section; every public getter/builder method needs `#[must_use]`
(clippy pedantic enforces both).

`connector-postgres` also has **live integration tests** (`tests/connection_live.rs`,
`replication_live.rs`, `source_live.rs`) that require a real Postgres instance with
`wal_level = logical`:

```bash
docker run -d --name bil-test-postgres -e POSTGRES_PASSWORD=testpass -e POSTGRES_DB=bil_test \
  -p 5433:5432 postgres:16 -c wal_level=logical -c max_replication_slots=4 -c max_wal_senders=4

psql -h localhost -p 5433 -U postgres -d bil_test -c "
  CREATE TABLE patients (id SERIAL PRIMARY KEY, patient_ref TEXT NOT NULL, diagnosis_code TEXT);
  CREATE PUBLICATION bil_pub FOR TABLE patients;
  SELECT pg_create_logical_replication_slot('bil_slot', 'pgoutput');
"

cargo test -p connector-postgres --tests -- --nocapture
```

These tests skip (print and return, not fail) if the instance isn't reachable, so `cargo test
--workspace` stays green without Docker — but any change to the connection/replication/pgoutput code
must be re-verified against a real instance, not just the unit tests.

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

## Architecture: `proof-connectors`

Defines the two traits that link external enterprise data sources to `proof-core`, industry-agnostic
by design so a Postgres row, a supply-chain handoff event, and a loan decision all flow through the
same shapes:

- `RecordSource` — where records-to-prove come from. `next()` yields `(SourceRecord, AckToken)`
  pairs; at-least-once delivery; the caller must only call `AckToken::ack()` after the record has
  been successfully submitted to a `ProofSink`, so a crash between hashing and acking can't silently
  drop or duplicate a record.
- `ProofSink` — where finished proofs go. First implementation, `InProcessProofSink`, is a broadcast
  channel (live consumers: the UI's real-time feed) backed by a capped history buffer (late-joining
  consumers see recent history first). This is the seam a later multi-process split would replace —
  every consumer (UI, metrics, future anchoring) depends only on this trait, never on
  `InProcessProofSink`'s internals.

## Architecture: `connector-postgres`

The first concrete `RecordSource`: watches a Postgres table via **logical replication (CDC)** — zero
application-code changes required on the source side, matching the README's "data never moves"
claim literally. Requires the target DB to have `wal_level = logical` and a publication + replication
slot already created (this crate deliberately does not create those schema-level objects itself).

**Why this is hand-rolled instead of using a library**: `tokio-postgres` has no support at all for
the replication protocol (`CopyBoth` mode), and its startup/auth handshake is a private
implementation detail that can't be reused externally. The only pgoutput-decoding crate on
crates.io (`pgoutput`) was a 0.0.x single-maintainer crate — an unacceptable supply-chain risk for a
security-focused project — so the connection (startup + SCRAM-SHA-256/MD5 auth), the
`START_REPLICATION`/`CopyBoth` loop, and the `pgoutput` binary decoder are all hand-written here on
top of `postgres-protocol` (the same low-level crate `tokio-postgres` itself uses for wire
encode/decode). **This was verified against a real, disposable Postgres 16 Docker container, not
just unit tests against assumed byte layouts** — see `tests/*_live.rs`, which run real `psql`
INSERT/UPDATE/DELETE statements and assert the decoded output and replication-slot advancement are
correct. Any change to `pgoutput.rs`/`replication.rs`/`connection.rs` should be re-verified the same
way, not just against unit tests with hand-constructed byte fixtures.

Module layout: `connection.rs` (raw TCP + startup + auth) → `replication.rs` (`START_REPLICATION`,
`CopyBoth` framing, keepalive/standby-status) → `pgoutput.rs` (binary decoder) → `change.rs`
(canonical bincode encoding of a row change — the actual hashed bytes, kept separate from the decoder
so a future pgoutput refactor can't silently change what gets hashed) → `source.rs` (the
`RecordSource` impl wiring it all together, LSN-based acknowledgment via `Arc<Mutex<ReplicationStream>>`
shared between the read loop and issued `AckToken`s).

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
