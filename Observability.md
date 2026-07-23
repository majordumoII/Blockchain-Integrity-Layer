# Observability: proof-service + Prometheus + Grafana

## Scope

Covers two phases built back-to-back: **Phase 3** (`proof-service`, the binary that actually runs
the system) and **Phase 4** (Prometheus + Grafana, provisioned as code). Together they turn the
Phase 1/2 work (proof generation, Postgres CDC connector) from a library and a set of tests into a
running system with a live UI and dashboards.

## Decisions

| Decision | Rationale |
|---|---|
| Single `proof-service` binary (not split into separate connector/API processes) | Simplicity now, without foreclosing a later split — `proof_connectors::ProofSink` is the seam a split would replace (swap the in-process broadcast+history impl for a message bus), so nothing has to be redesigned later, only re-implemented behind the same trait |
| Server-rendered HTML (Askama) + htmx/SSE, not a JS framework/SPA | One language/toolchain for the whole repo; the live feed only needs "swap in new HTML fragments," which htmx does natively over Server-Sent Events without a build step |
| htmx + its SSE extension vendored and embedded via `include_str!`, not loaded from a CDN | Keeps `proof-service` a single self-contained deployable binary with no runtime dependency on an external host |
| Fresh Ed25519 signing key generated at service startup, not persisted/rotated | No deployment/key-management story exists yet at this stage; already-produced proofs stay independently verifiable against the logged public key regardless of what the service does on a later run |
| Prometheus + Grafana over alternatives | Standard choice for a Rust service (`metrics` + `metrics-exporter-prometheus` crates are first-class); no requirement in this project (single service, no distributed tracing need) argues for anything like OpenTelemetry/Tempo instead |
| Grafana dashboard provisioned as code (checked-in JSON + provisioning YAML), not built by hand in the UI | Reproducible — `docker compose up` gives anyone the same dashboard, no manual click-ops, and the definition is reviewable/diffable like any other code change |

## What was built

### `crates/proof-service`

- **`pipeline.rs`** — the actual loop: `RecordSource.next()` → BLAKE3 hash → Ed25519 sign (via
  `proof-core`'s `ProofBuilder`) → `ProofSink.submit()` → `AckToken.ack()`. Acknowledgment only
  happens after the sink durably accepts the proof, so a crash between hashing and acking leaves the
  source able to redeliver rather than silently dropping the record.
- **`web/`** — axum routes: `GET /` (initial history, server-rendered), `GET /feed` (Server-Sent
  Events pushing new proofs live), `/static/*` (embedded htmx + SSE extension). The UI reads from
  `InProcessProofSink` the same way metrics or a future anchoring consumer would — no special access.
- **`metrics.rs`** — Prometheus counters (`bil_records_observed_total`, `bil_proofs_generated_total`,
  `bil_records_failed_total`) and a latency histogram (`bil_proof_pipeline_seconds`), all labeled by
  `source_id`/`table`.
- **`main.rs`** — clap-based config (Postgres connection, slot/publication, listen addresses; every
  field also readable from an env var).

### `observability/`

- `docker-compose.yml` — Prometheus + Grafana. `proof-service` is expected to run on the host (it
  needs a route to Postgres), so Prometheus reaches it via `host.docker.internal:9090`, with
  `extra_hosts: host-gateway` added so that resolves on Linux, not just Docker Desktop.
- `grafana/provisioning/` — datasource and dashboard auto-load on Grafana startup.
- `grafana/dashboards/proof-service.json` — panels: records observed/sec, proofs generated/sec,
  total counts, failed-records-by-stage, and pipeline latency p50/p95/p99.

## Verification

Both phases were verified against real, disposable infrastructure — not just "the code compiles" or
"the YAML parses":

1. Started a disposable Postgres 16 Docker container with `wal_level = logical`, a `patients` table,
   publication, and replication slot.
2. Started `proof-service` pointed at it. Ran real `INSERT`/`UPDATE` statements via `psql`.
3. Confirmed: the pipeline logged "proved record" for each; `curl localhost:9090/metrics` showed
   correct counts and labels; `curl localhost:8080/` rendered real proof rows with real BLAKE3
   digests; an SSE client (`curl -N .../feed`) received a new row within ~1 second of a live INSERT.
4. Brought up the `observability/` Docker Compose stack alongside the same running `proof-service`.
   Confirmed Prometheus's own `/api/v1/query?query=up` reported the scrape target healthy, and that
   `bil_proofs_generated_total` reflected the real count. Confirmed Grafana's provisioned datasource
   and dashboard existed via its API, and queried through **Grafana's own datasource-proxy endpoint**
   (the same code path a dashboard panel uses, not just Prometheus directly) to confirm the exact
   value a panel would render.
5. Tore down all test containers/processes afterward — none of this is meant to run persistently.

## Troubleshooting: default ports already in use

`proof-service` defaults to `127.0.0.1:8080` (web UI) and `127.0.0.1:9090` (Prometheus metrics).
Nothing about these is special — they're just `clap` defaults — so on a machine that already has
something bound to 8080/9090/9091 (an unrelated SSH tunnel, another local service, a previous
`observability/` Grafana stack, etc.) the service will fail to bind, or — more confusingly — you'll
hit an *unrelated* service on that port and get a response that isn't from `proof-service` at all
(e.g. a stray `{"detail":"Not Found"}` is FastAPI/uvicorn's 404 body, not axum's — a clear tell you're
not actually talking to this project).

Check what's listening before assuming the service itself is broken:

```bash
lsof -nP -iTCP:8080 -sTCP:LISTEN
lsof -nP -iTCP:9090 -sTCP:LISTEN
```

If either is occupied, override the listen addresses via env vars (or the equivalent `--web-addr` /
`--metrics-addr` flags) rather than fighting the conflict:

```bash
BIL_WEB_ADDR=127.0.0.1:8090 BIL_METRICS_ADDR=127.0.0.1:9190 \
  BIL_PG_HOST=localhost BIL_PG_PORT=5433 BIL_PG_USER=postgres BIL_PG_PASSWORD=testpass \
  BIL_PG_DBNAME=bil_test BIL_PG_SLOT=bil_slot BIL_PG_PUBLICATION=bil_pub \
  cargo run -p proof-service
```

To leave it running in the background instead of blocking the current shell (e.g. to browse to it
afterward, rather than the disposable start-verify-teardown pattern used elsewhere in this doc), use
`nohup ... & disown` so it survives the shell session ending.

## Next Steps (not yet started)

- **Blockchain anchoring** — a chain-agnostic interface a `Proof` gets submitted to, plus a first
  concrete backend (a local/file-backed anchor would be the fastest way to close the loop before
  committing to any real chain integration).
- Verification API, SDK/CLI tooling, additional connectors (webhook/event ingest, file/object
  storage) remain on the roadmap per `README.md`'s Project Status.
