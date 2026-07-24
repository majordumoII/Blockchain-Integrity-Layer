# Blockchain Integrity Layer

> **Trust without disclosure** — cryptographic proof infrastructure for privacy-preserving business data sharing.

---

## Problem Statement

Organizations routinely need to prove facts about their data without revealing the data itself. Common questions include:

- *"Did this shipment stay below 8°C the whole time?"*
- *"Is this customer over 18?"*
- *"Did this supplier pass our audit?"*
- *"Does this bank customer satisfy KYC requirements?"*
- *"Did this AI model train on licensed data?"*

Today's answer: share documents, spreadsheets, or grant database access. This creates:

| Issue | Consequence |
|---|---|
| **Sensitive data exposure** | Privacy breaches, regulatory liability |
| **Mutual distrust** | Counterparties can't verify each other's claims independently |
| **Expensive compliance audits** | Manual review, forensic analysis, legal overhead |
| **Retroactive tampering** | No tamper-evident guarantees after the fact |
| **Custom integrations** | Every partnership requires bespoke data-sharing infrastructure |

The core unmet need: **a way to prove a claim is true without revealing the underlying data.**

---

## Business Impact & Value Proposition

**For each stakeholder:**

| Stakeholder | Before | After |
|---|---|---|
| **Healthcare** | Share patient records for research → HIPAA risk | Prove *"patient qualifies for study, age 40–60, has diabetes"* — zero PHI leaked |
| **Supply chain** | Reveal logistics partners, pricing, routes | Prove *"temperature maintained, no contamination, delivered on time"* — trade secrets protected |
| **Banking** | Share salary, tax returns, bank statements for underwriting | Prove *"income >$120k, debt ratio <20%, identity verified"* — no raw financial data |
| **AI / ML** | Expose proprietary training datasets to prove licensing | Prove *"model trained on licensed data, no copyrighted images"* — IP stays secret |

**Value drivers:**
- **↓ Audit costs** — cryptographic proofs replace manual forensic reviews
- **↓ Integration overhead** — standardized proof API vs. custom data pipelines
- **↑ Trust velocity** — verify in seconds, not weeks of due diligence
- **↓ Regulatory risk** — never touch or transmit sensitive data you don't need
- **↑ Interoperability** — one proof format works across industries

---

## Differentiator

> **Many blockchain projects stop at storing hashes on-chain.** We go further.

| Dimension | Existing approaches | This project |
|---|---|---|
| **Scope** | Immutable hash log only | Full **Proof Cloud** — define rules, generate proofs, verify via simple APIs |
| **Adoption model** | "Migrate your data to our chain" | **Zero data migration** — proofs are additive to existing databases |
| **Privacy** | Data visible to validators | Data never leaves the organization — only cryptographic commitments go on-chain |
| **Developer experience** | Smart contract complexity | **SDK + REST/gRPC API** — integrate in minutes |
| **Interoperability** | Chain-specific | Industry-agnostic: healthcare, supply chain, banking, AI, government |

**Philosophy:** Don't ask hospitals, banks, or governments to move their data to a blockchain. Let them keep their existing databases and microservices — just add cryptographic integrity guarantees on top. This **dramatically lowers the adoption barrier.**

---

## Solution: Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                   Your Application                   │
│                    (existing code)                    │
└───────────┬─────────────────────────┬───────────────┘
            │                         │
            ▼                         ▼
┌───────────────────────┐  ┌──────────────────────────┐
│  Database (existing)  │  │  SDK / Integrity Wrapper  │
│  (data never moves)   │  │  (Rust)                  │
└───────────────────────┘  │                           │
                           │  • SHA-256 hashing        │
                           │  • Digital signing        │
                           │  • Proof generation       │
                           │  • On-chain anchoring     │
                           └───────────┬───────────────┘
                                       │
                                       ▼
            ┌─────────────────────────────────────┐
            │         Blockchain Layer             │
            │  (tamper-evident proof ledger)       │
            │                                      │
            │  Record ID │ Hash │ Timestamp │ Sig  │
            │  ─────────────────────────────────── │
            │  abc123    │ 0x4f… │ 2026-07-23 │ ✅  │
            │  def456    │ 0x8a… │ 2026-07-23 │ ✅  │
            └─────────────────────────────────────┘
```

**What the blockchain stores:**
- Record ID
- SHA-256 hash of the record
- Timestamp
- Digital signature(s)
- Service / organization ID
- Version number

**What the blockchain never stores:**
- ❌ Patient records (PHI)
- ❌ Financial statements
- ❌ Personal identifiable information (PII)
- ❌ Trade secrets
- ❌ Proprietary training data

---

## Walkthrough: End-to-End Flow

### Example: Healthcare — Proving a patient qualifies for a clinical trial

```
                        ┌─────────────┐
                        │  Doctor      │
                        │  updates     │
                        │  record      │
                        └──────┬──────┘
                               │
                               ▼
┌──────────────────────────────────────────────────┐
│                Hospital Database                  │
│  Record #1024: Age=47, Diagnosis=Diabetes, ...    │
│  (data stays here — never leaves the hospital)    │
└──────────────────────────┬───────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────┐
│           Integrity Wrapper (Rust SDK)            │
│                                                   │
│  1. Read the changed record                       │
│  2. SHA-256 → 0x4f8a2b1c...                      │
│  3. Sign with hospital's private key              │
│  4. Construct proof:                              │
│     { hash, timestamp, doctor_id, hospital_id }   │
│  5. Anchor proof on blockchain                    │
└──────────────────────────┬───────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────┐
│               Blockchain Ledger                   │
│                                                   │
│  Record #1024:                                    │
│  ├─ Hash: 0x4f8a2b1c...                         │
│  ├─ Timestamp: 2026-07-23T12:00:00Z             │
│  ├─ Signed by: Dr. Smith (0xab1...)             │
│  ├─ Org: Hospital XYZ (0x77e...)                │
│  ├─ Block: #291,482                             │
│  └─ Status: ✅ verified                          │
└──────────────────────────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────┐
│  Researcher (verifier)                            │
│                                                   │
│  Receives proof from patient:                     │
│  ✓ Hash matches the record                        │
│  ✓ Signature valid (Doctor + Hospital)            │
│  ✓ Timestamp consistent                           │
│  ✓ Record exists on-chain since block #291,482    │
│                                                   │
│  Result: "Patient qualifies ✓"                    │
│  (never saw name, DOB, address, or SSN)           │
└──────────────────────────────────────────────────┘
```

### Detailed Step Sequence

```
Step  Action                              Who               On-Chain?
────  ──────────────────────────────────  ────────────────  ─────────
  1   Doctor edits patient record         Application       No
  2   Database commits                    Database          No
  3   SDK intercepts change              Integrity SDK      No
  4   Hash the record                     Integrity SDK      No
  5   Sign the hash                       Integrity SDK      No
  6   Anchor proof on chain               Integrity SDK      ✅ Yes
  7   Return confirmation to app          Integrity SDK      No
  8   Later: verifier checks proof        Anyone with key    ✅ Yes
```

### Verification — anyone can confirm:

- ✅ The record **existed** at a point in time
- ✅ The record has **not been modified** since
- ✅ **Who** signed it (doctor, hospital, both)
- ✅ **When** it happened
- ✅ Without **ever exposing the record contents**

---

## Technical Features

| Feature | Description |
|---|---|
| **Proof Generation SDK** | Lightweight Rust library — embed in any service |
| **Verification API** | REST/gRPC endpoints for proof validation |
| **Digital Signatures** | Ed25519 / ECDSA — multi-signature support |
| **Role-Based Access Control** | Granular permissions for proof creation & verification |
| **Timestamped Audit Trails** | Immutable, ordered event log |
| **Data Versioning** | Track every change to a record |
| **Revocation Support** | Invalidate compromised proofs |
| **Multi-Party Approvals** | Require N-of-M signatures |
| **Compliance Dashboards** | Pre-built views for auditors |

---

## Industry Applications

```
Healthcare         Supply Chain        Banking            AI / ML
─────────          ─────────────       ──────────         ──────────
Patient proofs     Temp monitoring     Income verify       Data provenance
Research consent   Chain of custody    KYC/AML checks      Licensed training
Audit trails       Quality assurance   Loan underwriting   Model attestation
Insurance claims   Recall tracking     Fraud detection     Inference proofs
```

---

## Revenue Model

SaaS — not a token project.

| Tier | Price | Who |
|---|---|---|
| **Developer** | Free | Individuals prototyping |
| **Startup** | $99/mo | Early-stage companies |
| **Business** | $499/mo | Growing organizations |
| **Enterprise** | Custom | Regulated industries, high volume |

**Revenue drivers:** API usage fees, compliance reporting add-ons, custom integrations.

**Value to customer:** Dramatically reduced audit costs, simplified compliance, zero data migration.

---

## Expansion Opportunities

Once organizations trust your proof infrastructure, the same platform extends to:

- **Digital identity** — verifiable credentials without a central registry
- **Educational credentials** — degree verification without transcripts
- **Professional certifications** — license status, continuing education
- **Property ownership** — title chain without revealing owners
- **Medical records** — cross-institution proofs without sharing records
- **Carbon credit verification** — auditable environmental claims
- **Manufacturing QA** — supply chain certification at scale
- **AI model provenance** — training lineage, data licensing proofs

The **same proof infrastructure** — different industries, one standard.

---

## Why Rust?

```
┌────────────────────────────┐
│  Why Rust is the right fit │
├────────────────────────────┤
│ • Memory safety (no CVEs)  │
│ • Zero-cost abstractions   │
│ • Fast cryptographic ops   │
│ • Small binary footprint   │
│ • Easy to embed (FFI/SDK)  │
│ • Excellent networking     │
│   (Tokio, gRPC, HTTP)      │
└────────────────────────────┘
```

Think of it like Nginx, Redis, or Postgres — a **lightweight, trusted service** that runs beside every application and just works.

---

## Quick Start

This is a Cargo **workspace**. From the repo root:

```bash
# Build every crate
cargo build --workspace

# Run the proof-core demo: hashes a file, signs it, builds a proof,
# verifies it, then shows tamper detection and forged-signer detection
cargo run -p proof-core --example demo -- README.md

# Run the full test suite
cargo test --workspace

# Release build (LTO, panic=abort, stripped)
cargo build --workspace --release
```

To see the whole system running live (connector → proof-core → live UI → metrics → dashboard):

```bash
# 1. Start a local Postgres with logical replication (see connector-postgres's
#    tests/*_live.rs for the one-time table/publication/slot setup)

# 2. Start proof-service, pointed at it
BIL_PG_PASSWORD=... BIL_PG_DBNAME=... cargo run -p proof-service
# → live UI at http://localhost:8080, Prometheus metrics at :9090/metrics

# 3. Start the Prometheus + Grafana stack
cd observability && docker compose up -d
# → Grafana at http://localhost:3000 (admin/admin), dashboard auto-provisioned
```

`connector-postgres` also has live integration tests that exercise a real Postgres logical
replication stream end to end; they require a local Postgres instance with `wal_level = logical`
and are skipped (not failed) if one isn't reachable. See that crate's `tests/*_live.rs` files for
the one-time `docker run` / table / publication / slot setup.

### Anchoring to a real EVM chain

By default `proof-service` anchors to `LocalLogAnchor` (a local hash-chained file). To anchor to a
real chain instead, via `EvmAnchor` and the checked-in `solidity/src/ProofAnchor.sol` contract:

```bash
# 1. (One-time) install Foundry if you don't have it, and compile the contract
brew install foundry
cd solidity && forge build && cd ..

# 2. Deploy the contract to any EVM JSON-RPC endpoint — a local Anvil node for
#    testing, or a real testnet like Base Sepolia
anvil   # in a separate terminal, for local testing

BIL_EVM_RPC_URL=http://localhost:8545 \
BIL_EVM_DEPLOYER_PRIVATE_KEY=<funded-account-private-key> \
  cargo run -p proof-anchor --example deploy_evm_anchor
# → prints the deployed contract address

# 3. Point proof-service at it
BIL_PG_PASSWORD=... BIL_PG_DBNAME=... \
BIL_ANCHOR_BACKEND=evm \
BIL_EVM_RPC_URL=http://localhost:8545 \
BIL_EVM_PRIVATE_KEY=<anchoring-account-private-key> \
BIL_EVM_CONTRACT_ADDRESS=<address-from-step-2> \
  cargo run -p proof-service
```

Only a proof's 32-byte digest is ever sent on-chain (via the contract's `anchor(bytes32)` function
and `ProofAnchored` event) — never raw record data or even the full `Proof` structure, matching
this project's "the blockchain never stores raw content" stance. `EvmAnchor` has no chain-specific
logic; the same code works against any EVM-compatible network, testnet or mainnet, public or
permissioned. `proof-anchor`'s `tests/evm_live.rs` exercises this against a real local Anvil node
(skipped, not failed, if `anvil` isn't on `PATH`) — see that crate's docs for details.

---

## Project Structure

```
.
├── Cargo.toml                        # Workspace manifest
├── crates/
│   ├── proof-core/                   # Hashing, signing, canonical Proof/ProofBuilder
│   ├── proof-connectors/             # RecordSource / ProofSink traits (industry-agnostic)
│   ├── connector-postgres/           # RecordSource impl: Postgres logical replication (CDC)
│   ├── proof-anchor/                 # ProofAnchor trait + LocalLogAnchor + EvmAnchor
│   └── proof-service/                # Binary: connector → proof-core → ProofSink/anchor, UI + metrics
├── solidity/                         # ProofAnchor.sol (the EvmAnchor contract) + Foundry config
├── observability/                    # Prometheus + Grafana (dashboard-as-code via docker-compose)
├── README.md                         # This file
├── ROADMAP.md                        # Gap analysis: README pitch vs. what's built, prioritized
├── Phase1.md                         # Phase 1 build notes (proof-core)
├── Observability.md                  # proof-service + Prometheus/Grafana build notes
├── Instructions.md                   # How to see proof-core work end to end
└── .gitignore                        # VCS ignore rules
```

---

## Project Status

**v0.1.0** — Core proof generation, a live data-source connector, a running service with
observability, and tamper-evident anchoring — including a genuinely chain-backed anchor — all
verified end-to-end against a real database and a real EVM chain (local Anvil node; deployable
as-is to Base Sepolia or any other EVM-compatible network).

- [x] Cargo workspace scaffolded
- [x] Core proof generation engine (`proof-core`): algorithm-agnostic hashing (BLAKE3/SHA-256),
      signing (Ed25519/ECDSA P-256), canonical versioned `Proof` format, multi-party attestation
- [x] Connector abstraction (`proof-connectors`): `RecordSource` (industry-agnostic input) and
      `ProofSink` (broadcast + history, for the live UI feed / metrics / anchoring to share)
- [x] First real connector (`connector-postgres`): hand-rolled Postgres logical replication client
      (no third-party replication crate — verified against a live Postgres 16 instance) that turns
      `INSERT`/`UPDATE`/`DELETE` row changes into provable records with zero source-side code changes
- [x] `proof-service`: single binary wiring the connector → `proof-core` → `ProofSink` + anchor,
      serving a live-activity UI (server-rendered HTML + htmx/SSE) and Prometheus `/metrics`
- [x] Observability: Prometheus + Grafana via `docker-compose`, with the dashboard provisioned as
      code (`observability/grafana/dashboards/proof-service.json`) — no manual dashboard clicking
- [x] Anchoring (`proof-anchor`): chain-agnostic `ProofAnchor` trait with two implementations —
      `LocalLogAnchor` (hash-chained append-only local file, no network dependency) and `EvmAnchor`
      (a minimal deployed smart contract on any EVM-compatible chain, only ever committing a proof's
      32-byte digest on-chain, never raw proof/record data) — both verified end-to-end, `EvmAnchor`
      against a real local Anvil node and `proof-service`'s full pipeline
- [x] Chain-backed anchor implementation (testnet-ready): `EvmAnchor` + `solidity/src/ProofAnchor.sol`
- [x] Verification API (v1): `GET /api/v1/proofs/{digest}` and `POST /api/v1/verify` on
      `proof-service`, the first externally-consumable way to check a proof without running Rust
      against this repo
- [ ] SDK (Rust crate) / CLI tooling
- [ ] Compliance dashboard
- [ ] Additional connectors (webhook/event ingest, file/object storage)


<img width="979" height="499" alt="image" src="https://github.com/user-attachments/assets/454be779-3192-46b8-9128-ec734655e295" />

---

## License

MIT — see [LICENSE](LICENSE).
