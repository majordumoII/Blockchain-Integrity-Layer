# Instructions: Proving the Blockchain Integrity Layer Works, Milestone by Milestone

This walks through how to see every milestone built so far actually work, end to end, against real
systems — no Rust-reading required to interpret the results. Each milestone below is independently
runnable; skip to whichever one you care about, or run them in order to retrace the whole build.

## Prerequisites

- Rust toolchain (`cargo --version` should work)
- Docker (for the Postgres milestones)
- [Foundry](https://getfoundry.sh/) (`brew install foundry`) — provides `anvil`/`forge`/`cast`, for
  the blockchain-anchoring milestones
- A terminal in the repo root (`Blockchain-Integrity-Layer/`)

---

## Milestone 1 — Core proof generation engine (`proof-core`)

The foundation everything else builds on: hash → sign → build → verify, independent of any
blockchain, storage, or API concern.

```bash
cargo run -p proof-core --example demo -- README.md
```

This walks through the full loop out loud, printing `[OK]`/`[FAIL]` at each step:

1. **Hashes the file** with BLAKE3, prints the digest.
2. **Generates a signing key** (Ed25519) and signs the digest.
3. **Builds a proof** — digest + signer attestation + metadata — and shows its canonical byte size.
4. **Verifies the proof against the untouched file** — all checks pass.
5. **Tampers with the record** (flips one byte) and re-verifies — the digest check fails, while the
   signature and signer-identity checks still pass, isolating *which* check catches the problem.
6. **Simulates a forgery** — a second proof over the same digest, signed by a *different* key
   pretending to be the original signer — and re-verifies. The signature is internally valid (it
   really is a correct signature, just from the wrong signer), but the signer-identity check fails.
   This is the actual attack a real verifier must defend against: a valid signature alone isn't
   enough — you also have to check *whose* key it is.

**What this milestone does not yet prove:** no blockchain anchoring (nothing leaves memory), no
trusted-key registry (the demo hardcodes "the trusted key" as the key it just generated), no
CLI/API surface for production use.

---

## Milestone 2 — Connector abstraction + Postgres CDC connector

`RecordSource`/`AckToken`/`ProofSink` — the industry-agnostic traits linking external data sources
to `proof-core` — and their first concrete implementation, `connector-postgres`, which watches a
table via **logical replication (CDC)**: zero application-code changes on the source side.

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

These are **live integration tests** against a real Postgres instance (not mocked byte fixtures) —
they run real `psql` INSERT/UPDATE/DELETE statements and assert the decoded output and
replication-slot advancement are correct. They skip (print and return, not fail) if the instance
isn't reachable, so `cargo test --workspace` stays green without Docker.

---

## Milestone 3 — `proof-service`: the binary tying it together, with a live UI

The process that actually runs the system: `RecordSource` → hash → sign → `ProofSink`, serving a
live-activity UI. This is also the milestone the later ones (anchoring, verification API, SDK,
compliance dashboard) all run inside — see **Milestone 11** below for the single script that spins
up this whole picture at once.

```bash
cargo run -p proof-service -- \
  --pg-host localhost --pg-port 5433 --pg-user postgres --pg-password testpass --pg-dbname bil_test \
  --pg-slot bil_slot --pg-publication bil_pub \
  --source-id "postgres:patients"
```

With that running, in another terminal:

```bash
psql -h localhost -p 5433 -U postgres -d bil_test -c "
  INSERT INTO patients (patient_ref, diagnosis_code) VALUES ('DEMO-001', 'J45.909');
"
```

Open `http://localhost:8080/` — the row you just inserted appears live (Server-Sent Events, no
refresh needed), showing its digest and signer.

---

## Milestone 4 — Observability: Prometheus + Grafana, provisioned as code

```bash
cd observability && docker compose up -d && cd ..
```

- Prometheus: `http://localhost:9091`
- Grafana: `http://localhost:3000` (admin/admin) — dashboard auto-loads, no manual setup

With `proof-service` running (Milestone 3) and a few more rows inserted, the Grafana dashboard shows
live counters (`bil_records_observed_total`, `bil_proofs_generated_total`,
`bil_records_failed_total`) and pipeline latency, faceted by source/table.

---

## Milestone 5 — Chain-backed anchoring (`EvmAnchor` + `ProofAnchor.sol`)

Anchoring a proof's digest to a real blockchain — a smart contract deployed to any EVM chain, local
Anvil for this walkthrough. `LocalLogAnchor` (a hash-chained append-only local log, no network
dependency) is the other backend and needs no separate setup — it's the default.

```bash
anvil    # leave running in its own terminal; prints funded dev accounts + private keys

cd solidity && forge build && cd ..   # only needed after editing ProofAnchor.sol — the build
                                        # artifact is already checked in

BIL_EVM_RPC_URL=http://localhost:8545 \
BIL_EVM_DEPLOYER_PRIVATE_KEY=<paste Anvil's printed Private Key [0]> \
cargo run -p proof-anchor --example deploy_evm_anchor
```

This prints a deployed contract address. Then run the crate's own test suite, which spins up a
**fresh real local Anvil node per test** (not mocked) and exercises the full trait contract
including cross-receipt tamper detection:

```bash
cargo test -p proof-anchor --test evm_live -- --nocapture
```

Skips (doesn't fail) if `anvil` isn't on `PATH`, so `cargo test --workspace` stays green without
Foundry installed.

---

## Milestone 6 — Verification API (`GET`/`POST` on `proof-service`)

The first externally-consumable verification surface — previously a counterparty had no way to
check a proof except running Rust against this repo.

With `proof-service` running and at least one row proved (Milestones 3 + 5 combined — see
Milestone 11's `start.sh` for the one-command version):

```bash
# Look up a proof + its real anchor receipt by digest
curl -s http://localhost:8080/api/v1/proofs/<digest-hex> | jq

# Independently re-verify a proof + receipt pair
curl -s -X POST http://localhost:8080/api/v1/verify \
  -H 'content-type: application/json' \
  -d @<(curl -s http://localhost:8080/api/v1/proofs/<digest-hex> | jq '{proof, receipt}') | jq
```

`{"valid": true, ...}` confirms both the signatures and the on-chain anchor were independently
re-checked, not read from a cache.

---

## Milestone 7 — SDK (`bil-client`) + shared wire types (`proof-api-types`)

A typed Rust HTTP client over the verification API, so integrating doesn't require hand-rolling
JSON calls against undocumented shapes.

```bash
cargo test -p bil-client --test api_live -- --nocapture
```

This test builds `proof-service`'s real router in-process (bypassing CDC/Anvil setup, since it's
about the HTTP/JSON client contract), wired to a genuine `LocalLogAnchor` backed by a temp file
(not a mock), seeds one proof, binds to a real ephemeral TCP port, and exercises `get_proof` +
`verify` through the actual `Client` type.

---

## Milestone 8 — Streaming hash API (`proof_core::hash::Hasher`, `hash_reader`)

Closes the gap that blocked object/blob storage as a data source: hashing content too large to
buffer as one `&[u8]` (e.g. a multi-GB blob) without ever holding the whole thing in memory.

```bash
cargo test -p proof-core hash:: -- --nocapture
```

Includes a property-based test confirming chunking never changes the resulting digest — i.e.
hashing the same content in one shot vs. in arbitrary-sized pieces always agrees.

---

## Milestone 9 — Second connector: `connector-gcs` (Google Cloud Storage via Pub/Sub)

The first non-database `RecordSource` — watches a GCS bucket via Pub/Sub `OBJECT_FINALIZE`
notifications, no write-side changes required. Requires exactly-once delivery on the subscription
(fails loudly rather than silently degrading if it isn't configured).

```bash
cargo test -p connector-gcs -- --nocapture
```

Unit tests cover notification parsing and canonical object-change encoding without needing live GCP
credentials. **This connector was additionally verified against real GCP infrastructure** (a real
bucket, a dedicated Pub/Sub topic/exactly-once subscription, and a real `gsutil cp` upload that
triggered a real notification the connector received, downloaded, hashed, and acknowledged) — that
verification requires your own GCP project and credentials, so it isn't reproducible from this repo
alone; see `CLAUDE.md`'s `connector-gcs` section for the exact commands used.

---

## Milestone 10 — Compliance dashboard

Human-readable views over the same verification logic the API uses — an aggregate coverage table
per source, and a per-record audit trail with a live "Verify now" button.

See **Milestone 11** below — this is easiest to see running via `start.sh`, which wires up the full
pipeline this dashboard reads from in one command.

```bash
cargo test -p proof-service compliance:: -- --nocapture
```

runs the unit tests (Prometheus-text parsing, coverage-percent math) without needing the live
services up.

---

## Milestone 11 — See everything running together: `start.sh` / `stop.sh`

The fastest way to see Milestones 3–6 and 10 working together against a real pipeline: real
Postgres CDC, real Anvil-anchored proofs, the live feed, the verification API, and the compliance
dashboard, all in one running `proof-service` process.

```bash
./start.sh
```

Brings up (idempotently — safe to re-run):
1. `bil-test-postgres` (creates it with the demo schema on first run, starts it if already created)
2. A fresh local Anvil node (fresh every run, since its chain state doesn't persist)
3. A fresh `ProofAnchor.sol` deploy to that Anvil node, writing the address into `.env`
4. `proof-service` itself, wired to both, logging live to `.demo/proof-service.log`

When it prints `Ready:`, open:
- `http://localhost:8080/` — live feed
- `http://localhost:8080/compliance` — aggregate compliance dashboard

Insert a row (the command is also printed by `start.sh` itself):

```bash
PGPASSWORD=testpass psql -h localhost -p 5433 -U postgres -d bil_test -c \
  "INSERT INTO patients (patient_ref, diagnosis_code) VALUES ('DEMO-001', 'J45.909');"
```

Watch it land in the feed, click its digest to reach its compliance page, click "Verify now" for a
live re-check of signatures + on-chain anchor, then check `/compliance` again for updated
observed/proved/coverage% counts.

```bash
./stop.sh          # stops proof-service + anvil, leaves Postgres up for next time
./stop.sh --all    # also stops Postgres
```

---

## Verify code quality gates (applies to every milestone)

```bash
cargo build --workspace                                          # build all crates
cargo build --workspace --release                                 # release build (LTO, panic=abort, stripped)
cargo test --workspace                                             # run all tests
cargo clippy --workspace --all-targets -- -W clippy::pedantic       # must be zero warnings
cargo fmt --check                                                   # must be clean
```

Every crate in this workspace is held to `#![forbid(unsafe_code)]` and `clippy::pedantic`
zero-warnings as a bar for every milestone above, not just the earliest ones.

## What is not yet built

See `ROADMAP.md` and `GAPS-DATA-SOURCES.md` for the honest, current gap analysis — as of this
writing, the largest standing gaps are: a claim/predicate layer (proving "age > 18" without
revealing the birthdate itself), a producer-side ergonomic SDK (`integrity.commit(record)?`), and
RBAC/revocation/N-of-M attestation policy enforcement.
