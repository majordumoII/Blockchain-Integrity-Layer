# Roadmap: Closing the Gap Between the Pitch and the Build

This is a gap analysis between `README.md`/`README.original.md`'s problem statement and what's
actually built and verified as of the `proof-anchor` phase, plus the prioritized work left to close
that gap. Written as a snapshot — re-derive it from the code and README rather than trusting it
once new phases land.

## The Problem Statement, Restated Precisely

Both README docs converge on one sentence: **"prove a claim is true without revealing the
underlying data."** The mechanism promised: hash → sign → anchor (on-chain) → verify, with the data
never leaving the source system, and the chain storing only commitments (hash, timestamp,
signature, org ID, version) — never raw content.

## Does the current build deliver on it?

**Partially — the cryptographic core is real and verified; the "on-chain" and "prove a specific
claim" pieces are not yet built.**

| README claim | Status | Evidence |
|---|---|---|
| "Data never leaves the organization" | ✅ **True today** | `connector-postgres` hashes rows in-process via CDC; raw record bytes never appear in a `Proof` — enforced by the type signatures (`ProofBuilder::new` takes a `Digest`, not bytes), not just convention |
| "Cryptographic commitments" (hash + signature) | ✅ **True today** | `proof-core` — BLAKE3/SHA-256 hashing, Ed25519/ECDSA signing, canonical versioned `Proof` format, verified with 23+ passing tests including tamper and forged-signer detection |
| "Anchor proof on blockchain" | ⚠️ **Not yet — anchored to a local hash-chained log, not a blockchain** | `proof-anchor`'s `LocalLogAnchor` gives real tamper-evidence (each entry hashes the previous), but it's a single local file, not a distributed ledger. No consensus, no third-party verifiability without trusting whoever holds that file. This is the single biggest gap between the pitch and the build. |
| "Zero data migration... add integrity on top of existing databases" | ✅ **True today** | `connector-postgres` is genuinely zero-touch — no schema changes, no app code changes, just CDC |
| "Prove a *specific claim*" (e.g. "customer is over 18", "temp stayed below 8°C") | ❌ **Not built at all** | This is arguably the actual product per the README's "Proof Cloud" differentiator ("define rules, generate proofs"). Today the system proves "this exact row existed and hasn't changed" — a data-integrity proof, not a selective-disclosure/predicate proof. There's no rule engine, no way to prove "age > 18" without revealing the birthdate. |
| "Verification API" (REST/gRPC for third parties to check a proof) | ❌ **Not built** | Verification currently only happens via `ProofAnchor::verify()` called from within the same Rust process/crate. No external-facing endpoint a counterparty (the researcher, the auditor) could hit. |
| "SDK" for other services to integrate in minutes | ❌ **Not built** | `proof-core` is a Rust crate, not a packaged, documented SDK with a stable public API contract. No language bindings, no versioned release. |
| Multi-party approvals, revocation, RBAC, compliance dashboards | ❌ **Not built** | Listed as "Technical Features" in the README; none exist. Multi-party attestation *does* exist in `proof-core` (N signers can attest one proof) but N-of-M threshold policy, revocation, and RBAC are explicitly deferred to "a higher layer" per the code's own docs. |
| Prometheus/Grafana observability | ✅ **True today, and beyond what the README even asked for** | Fully built and verified — this wasn't in the original pitch at all, it's operational maturity added during the build |

## The Honest Gap-to-Bridge, in Priority Order

**1. Blockchain anchoring is the crux gap.** The entire premise — "trust without disclosure,"
verifiable by a third party who doesn't trust you — depends on the commitment being anchored
somewhere neither party unilaterally controls. `LocalLogAnchor` proves the *design* works (the
trait boundary, the tamper-evidence mechanics) but doesn't yet deliver the trust model. This needs
a real chain-backed `ProofAnchor` (testnet first, given the portability/vendor-risk lessons from
the GCP/Azure managed-blockchain research — see the portability memory).

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
zero-touch data capture, real observability. What's *not* yet built is the two things that actually
make the pitch's headline examples true: **an actual blockchain anchor** (not a local log) and **a
predicate/claim layer** that can prove "over 18" without revealing the birthdate. Until both exist,
the honest characterization is "tamper-evident audit trail infrastructure," not yet "prove a claim
without revealing the data" — which is a real and valuable thing, just a narrower claim than the
README makes.
