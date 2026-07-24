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
| "Anchor proof on blockchain" | ✅ **True today (persistent local chain, public testnet still pending)** | `proof-anchor`'s `EvmAnchor` anchors a proof's 32-byte digest to a deployed `solidity/src/ProofAnchor.sol` contract on any EVM-compatible chain. Verified three ways: (1) `tests/evm_live.rs` against ephemeral per-test Anvil nodes, (2) a manual deploy of the real contract bytecode to a *persistently-running* local Anvil node via `examples/deploy_evm_anchor.rs`, followed by a standalone anchor/verify/tamper-rejection smoke test against that specific deployed address, and (3) a full `proof-service --anchor-backend evm` run against the live Postgres CDC pipeline (`bil-test-postgres`) — real `INSERT`s produced real `ProofAnchored` events on-chain (confirmed via `cast logs`), with digests matching exactly what the UI and `/metrics` reported. Only the digest goes on-chain via an `anchor(bytes32)` call + `ProofAnchored` event — never raw record data or the full `Proof` structure. `LocalLogAnchor` remains available as a no-network-dependency alternative behind the same `ProofAnchor` trait. **Public testnet deployment (Base Sepolia) is still blocked**, not on code, but on funding the deployer wallet — faucet access was a problem in practice, so `.env`'s Base Sepolia RPC URL/keys are kept commented-out/ready but currently pointed at local Anvil instead (`BIL_EVM_RPC_URL=http://localhost:8545`). Local Anvil is not restart-durable by default — Anvil doesn't accept an arbitrary private key to pre-fund via a CLI flag, so reproducing this required starting a fresh Anvil (which seeds its own 10 well-known dev accounts) and `cast send`-ing test ETH from one of those into the address already in `.env`, then redeploying the contract. |
| "Zero data migration... add integrity on top of existing databases" | ✅ **True today** | `connector-postgres` is genuinely zero-touch — no schema changes, no app code changes, just CDC |
| "Prove a *specific claim*" (e.g. "customer is over 18", "temp stayed below 8°C") | ❌ **Not built at all** | This is arguably the actual product per the README's "Proof Cloud" differentiator ("define rules, generate proofs"). Today the system proves "this exact row existed and hasn't changed" — a data-integrity proof, not a selective-disclosure/predicate proof. There's no rule engine, no way to prove "age > 18" without revealing the birthdate. |
| "Verification API" (REST/gRPC for third parties to check a proof) | ✅ **True today (v1, REST/JSON — no gRPC)** | `proof-service`'s `web/api.rs`: `GET /api/v1/proofs/{digest}` looks up a proof + its `AnchorReceipt` this service produced; `POST /api/v1/verify` independently re-verifies any caller-supplied `Proof`+`AnchorReceipt` JSON pair (signatures + on-chain anchor state), with no requirement that this service has ever seen that proof before. Verified end-to-end against the real `EvmAnchor`/local-Anvil setup above: looked up a real proof by digest, round-tripped it through `/verify` (accepted), then confirmed a tampered digest and a mismatched receipt are both correctly rejected with distinct reasons. |
| "SDK" for other services to integrate in minutes | ❌ **Not built** | `proof-core` is a Rust crate, not a packaged, documented SDK with a stable public API contract. No language bindings, no versioned release. |
| Multi-party approvals, revocation, RBAC, compliance dashboards | ❌ **Not built** | Listed as "Technical Features" in the README; none exist. Multi-party attestation *does* exist in `proof-core` (N signers can attest one proof) but N-of-M threshold policy, revocation, and RBAC are explicitly deferred to "a higher layer" per the code's own docs. |
| Prometheus/Grafana observability | ✅ **True today, and beyond what the README even asked for** | Fully built and verified — this wasn't in the original pitch at all, it's operational maturity added during the build |

## The Honest Gap-to-Bridge, in Priority Order

**1. ~~Blockchain anchoring is the crux gap.~~ RESOLVED (against a real chain, local not public
yet).** The entire premise — "trust without disclosure," verifiable by a third party who doesn't
trust you — depends on the commitment being anchored somewhere neither party unilaterally
controls. `LocalLogAnchor` proved the *design* works (the trait boundary, the tamper-evidence
mechanics) but didn't deliver the trust model on its own. `EvmAnchor` now does: it's chain-agnostic
beyond "some EVM JSON-RPC endpoint" (portable across Base Sepolia, any other EVM testnet/mainnet,
or a permissioned EVM chain like Hyperledger Besu — no Base-specific or otherwise vendor-specific
logic), and `ProofAnchor::verify()`'s signature changed from `verify(receipt) -> Proof` to
`verify(receipt, proof) -> ()` to make this honest: an on-chain anchor only ever commits a digest,
so it has nothing to reconstruct a full `Proof` from, only enough to confirm a caller-supplied proof
matches what was actually anchored (mirroring `proof_core::hash::verify`'s digest/data split).
This has now been exercised end-to-end against a persistently-running local Anvil node (not just
the ephemeral per-test nodes `evm_live.rs` spins up) — contract deployed via
`examples/deploy_evm_anchor.rs`, then `proof-service --anchor-backend evm` run against the real
Postgres CDC pipeline, producing real on-chain `ProofAnchored` events for real `INSERT`s. What's
left is genuinely operational, not architectural: (a) **deploying to a persistent public testnet**
— attempted against Base Sepolia via QuikNode, blocked in practice by faucet-funding the deployer
wallet, not by any code issue — and (b) deciding on a production key-management story for the
anchoring signer (today it's a single env-var private key, fine for local/testnet, not for
production).

**2. There's no "claim" abstraction — only "this row is unchanged."** The README's headline
examples ("is this customer over 18," "did this shipment stay below 8°C") all require proving a
*derived predicate*, not just data integrity. Right now the system can prove a record hasn't been
tampered with since a point in time — it cannot prove "age > 18" without disclosing age. That
requires either (a) a rules engine that evaluates predicates before hashing only the boolean result
+ a reference to the source record, or (b) actual zero-knowledge proof techniques. Neither exists
yet. This is likely the single largest conceptual gap between what's built and what's pitched — the
current system is a **tamper-evidence layer**, not yet a **selective-disclosure proof system**.

**3. ~~No externally-consumable verification surface.~~ RESOLVED (REST/JSON; gRPC not offered).**
A counterparty (patient's researcher, bank's auditor) previously had no way to check a proof except
by running Rust code against this repo. `proof-service` now exposes `GET /api/v1/proofs/{digest}`
and `POST /api/v1/verify` — the latter takes a `Proof`+`AnchorReceipt` JSON body from *any* caller,
not just ones this service already knows about, and re-verifies both the signatures
(`Proof::verify_attestations()`) and the anchor (`ProofAnchor::verify()`), reporting which stage
failed if either does. This required one real design change beyond "add a route": `ProvedRecord`
(what `ProofSink`/`InProcessProofSink` retains) previously had no `receipt` field at all — anchoring
happened *after* the sink submit in `pipeline.rs` and the resulting `AnchorReceipt` was only ever
logged, never stored. Fixed by reordering the pipeline to anchor before submitting to the sink, and
adding `receipt: AnchorReceipt` to `ProvedRecord` (`proof-connectors` now depends on `proof-anchor`
for that type — no cycle, `proof-anchor` doesn't depend back on `proof-connectors`). What's left:
no gRPC (REST/JSON only), and lookups are bounded by `InProcessProofSink`'s capped in-memory history
— a proof older than that cap, or from a previous process run, isn't reachable via `GET
/api/v1/proofs/{digest}` even though it's still independently verifiable via `POST /api/v1/verify`
if the caller already has the `Proof`+`AnchorReceipt` from elsewhere (e.g. their own records, or a
block explorer). A durable proof store (see priority 1's still-open items) would close that.

**4. No packaged SDK.** Now that anchoring and verification both exist, wrapping `proof-core` (and
the new verification API) as a versioned, documented, embeddable SDK (possibly with FFI bindings for
non-Rust callers, or simply a thin typed HTTP client for the API above) is what makes "integrate in
minutes" true rather than aspirational. This is the next item to close.

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
