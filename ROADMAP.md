# Roadmap: Closing the Gap Between the Pitch and the Build

This is a gap analysis between `README.md`/`README.original.md`'s problem statement and what's
actually built and verified, plus the prioritized work left to close that gap. Written as a
snapshot — re-derive it from the code and README rather than trusting it once new phases land.

**Update:** the chain-backed anchor gap (originally the #1 priority below) is now closed —
`proof-anchor`'s `EvmAnchor` + `solidity/src/ProofAnchor.sol` anchor proof digests to a real EVM
smart contract, verified end-to-end against a local Anvil node and against `proof-service`'s full
Postgres-CDC-to-anchor pipeline. See the updated table and priority list below; the original
per-priority writeups are kept but marked resolved rather than rewritten, so this stays an honest
history of the gap rather than a claim it was always closed.

## The Problem Statement, Restated Precisely

Both README docs converge on one sentence: **"prove a claim is true without revealing the
underlying data."** The mechanism promised: hash → sign → anchor (on-chain) → verify, with the data
never leaving the source system, and the chain storing only commitments (hash, timestamp,
signature, org ID, version) — never raw content.

## Does the current build deliver on it?

**Mostly — the cryptographic core and the on-chain anchor are real and verified; "prove a specific
claim" (vs. "this row is unchanged") is the remaining large piece not yet built.**

| README claim | Status | Evidence |
|---|---|---|
| "Data never leaves the organization" | ✅ **True today** | `connector-postgres` hashes rows in-process via CDC; raw record bytes never appear in a `Proof` — enforced by the type signatures (`ProofBuilder::new` takes a `Digest`, not bytes), not just convention |
| "Cryptographic commitments" (hash + signature) | ✅ **True today** | `proof-core` — BLAKE3/SHA-256 hashing, Ed25519/ECDSA signing, canonical versioned `Proof` format, verified with 23+ passing tests including tamper and forged-signer detection |
| "Anchor proof on blockchain" | ✅ **True today (testnet-ready)** | `proof-anchor`'s `EvmAnchor` anchors a proof's 32-byte digest to a deployed `solidity/src/ProofAnchor.sol` contract on any EVM-compatible chain (verified against a local Anvil node in `tests/evm_live.rs`, and end-to-end through `proof-service`'s real Postgres CDC pipeline). Only the digest goes on-chain via an `anchor(bytes32)` call + `ProofAnchored` event — never raw record data or the full `Proof` structure. `LocalLogAnchor` remains available as a no-network-dependency alternative behind the same `ProofAnchor` trait. Not yet deployed to a persistent public testnet (that's an operational step — deploy via `examples/deploy_evm_anchor.rs` — not a code gap). |
| "Zero data migration... add integrity on top of existing databases" | ✅ **True today** | `connector-postgres` is genuinely zero-touch — no schema changes, no app code changes, just CDC |
| "Prove a *specific claim*" (e.g. "customer is over 18", "temp stayed below 8°C") | ❌ **Not built at all** | This is arguably the actual product per the README's "Proof Cloud" differentiator ("define rules, generate proofs"). Today the system proves "this exact row existed and hasn't changed" — a data-integrity proof, not a selective-disclosure/predicate proof. There's no rule engine, no way to prove "age > 18" without revealing the birthdate. |
| "Verification API" (REST/gRPC for third parties to check a proof) | ❌ **Not built** | Verification currently only happens via `ProofAnchor::verify()` called from within the same Rust process/crate. No external-facing endpoint a counterparty (the researcher, the auditor) could hit. |
| "SDK" for other services to integrate in minutes | ❌ **Not built** | `proof-core` is a Rust crate, not a packaged, documented SDK with a stable public API contract. No language bindings, no versioned release. |
| Multi-party approvals, revocation, RBAC, compliance dashboards | ❌ **Not built** | Listed as "Technical Features" in the README; none exist. Multi-party attestation *does* exist in `proof-core` (N signers can attest one proof) but N-of-M threshold policy, revocation, and RBAC are explicitly deferred to "a higher layer" per the code's own docs. |
| Prometheus/Grafana observability | ✅ **True today, and beyond what the README even asked for** | Fully built and verified — this wasn't in the original pitch at all, it's operational maturity added during the build |

## The Honest Gap-to-Bridge, in Priority Order

**1. ~~Blockchain anchoring is the crux gap.~~ RESOLVED.** The entire premise — "trust without
disclosure," verifiable by a third party who doesn't trust you — depends on the commitment being
anchored somewhere neither party unilaterally controls. `LocalLogAnchor` proved the *design* works
(the trait boundary, the tamper-evidence mechanics) but didn't deliver the trust model on its own.
`EvmAnchor` now does: it's chain-agnostic beyond "some EVM JSON-RPC endpoint" (portable across Base
Sepolia, any other EVM testnet/mainnet, or a permissioned EVM chain like Hyperledger Besu — no
Base-specific or otherwise vendor-specific logic), and `ProofAnchor::verify()`'s signature changed
from `verify(receipt) -> Proof` to `verify(receipt, proof) -> ()` to make this honest: an on-chain
anchor only ever commits a digest, so it has nothing to reconstruct a full `Proof` from, only
enough to confirm a caller-supplied proof matches what was actually anchored (mirroring
`proof_core::hash::verify`'s digest/data split). What's left here is operational, not architectural:
deploying to a persistent public testnet and deciding on a production key-management story for the
anchoring signer (today it's a single env-var private key, fine for testnet, not for production).

**2. There's no "claim" abstraction — only "this row is unchanged."** The README's headline
examples ("is this customer over 18," "did this shipment stay below 8°C") all require proving a
*derived predicate*, not just data integrity. Right now the system can prove a record hasn't been
tampered with since a point in time — it cannot prove "age > 18" without disclosing age. That
requires either (a) a rules engine that evaluates predicates before hashing only the boolean result
+ a reference to the source record, or (b) actual zero-knowledge proof techniques. Neither exists
yet. This is likely the single largest conceptual gap between what's built and what's pitched — the
current system is a **tamper-evidence layer**, not yet a **selective-disclosure proof system**.

**3. No externally-consumable verification surface.** A counterparty (patient's researcher, bank's
auditor) has no way to check a proof today except by running Rust code against the same repo. A
REST/gRPC verification endpoint is comparatively cheap to build on top of what exists
(`ProofAnchor::verify()` already does the hard part) and would close a real gap fast.

**4. No packaged SDK.** Once anchoring and verification exist, wrapping `proof-core` as a
versioned, documented, embeddable SDK (possibly with FFI bindings for non-Rust callers) is what
makes "integrate in minutes" true rather than aspirational.

**5. Everything else** (RBAC, revocation, N-of-M policy enforcement, compliance dashboards) is real
but secondary — these are hardening/enterprise-readiness features that matter for a paying
customer, not for proving the core concept works.

## Bottom Line

What's built is a solid, honestly-verified **foundation layer**: real cryptographic integrity, real
zero-touch data capture, real observability, and now a **real blockchain anchor** — `EvmAnchor`
commits proof digests to a deployed EVM smart contract, third-party verifiable without trusting
this service, not just tamper-evident to whoever holds a local file. What's *not* yet built is the
other big piece that would make the pitch's headline examples fully true: **a predicate/claim
layer** that can prove "over 18" without revealing the birthdate. Until that exists, the honest
characterization is "tamper-evident, chain-anchored audit trail infrastructure," not yet "prove a
claim without revealing the data" — which is a real and valuable thing, just a narrower claim than
the README makes. The remaining gaps (verification API, packaged SDK, RBAC/revocation/compliance
dashboards) are real but smaller lifts once the claim layer exists.
