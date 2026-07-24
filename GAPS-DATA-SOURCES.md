# Gap Analysis: Databases, Object/Blob Storage, and File Systems

Written the same way as `ROADMAP.md`: a snapshot, re-derive it from the code rather than trusting it
once new connectors land. Scope is narrower than `ROADMAP.md` — this is specifically "is the
`RecordSource`/pipeline design viable for these three classes of data source," not the broader
claim-layer/SDK gaps `ROADMAP.md` already covers.

## The question, restated precisely

`RecordSource` (`crates/proof-connectors/src/source.rs`) is designed to be industry- and
system-agnostic: `next()` yields `(SourceRecord, AckToken)` pairs, where `SourceRecord` is just
`bytes: Vec<u8>` + opaque `metadata` + an opaque `source_position` string. The question is whether
that shape — proven so far against exactly one system, Postgres logical replication — actually holds
up against databases generally, cloud object/blob storage, and file systems, or whether each reveals
an assumption the trait silently baked in.

## Short answer

**Object/blob storage is the closest to viable today — its one real gap (streaming hashing for large
objects) is now closed in `proof-core`, leaving only the connector itself (the event-notification
wiring) to build. Databases-beyond-Postgres are architecturally fine but each vendor is roughly its
own `connector-postgres`-sized effort — there is no shortcut across vendors. File systems are the
weakest fit: the trait assumes a durable "this change is committed, and I can acknowledge past it"
primitive that a plain filesystem does not provide.**

## 1. Databases (beyond Postgres)

**Verdict: architecturally sound, but "just implement `RecordSource`" undersells the effort.**

`connector-postgres` is ~1,280 lines (`connection.rs` + `replication.rs` + `pgoutput.rs` +
`change.rs` + `source.rs`) of hand-rolled protocol work, verified against a real live Postgres
instance — see `CLAUDE.md`'s own accounting of why (`tokio-postgres` has no replication-protocol
support at all, and the only `pgoutput`-decoding crate on crates.io was an unacceptable 0.0.x
supply-chain risk). That precedent generalizes as a *pattern*, not as reusable code:

- **MySQL** — binlog replication (`ROW`-format binlog events), a completely different wire protocol
  and event model from `pgoutput`. Would need its own hand-rolled (or carefully vetted third-party)
  binlog client.
- **SQL Server** — either CDC capture tables (polling-based, different consistency model) or CT
  (Change Tracking), neither resembling logical replication.
- **Oracle** — LogMiner or (commercially) GoldenGate; typically requires special licensing/tooling
  most self-hosted OSS projects don't assume access to.
- **Snowflake** — explicitly named in `CLAUDE.md`'s "portability target" as a real, not-hypothetical,
  requirement. This is the case most different from Postgres: Snowflake has no transactional
  replication slot concept at all. The native primitives are **Streams** (a change-tracking view over
  a table, consumed by querying it, not a push protocol) and **Snowpipe/Snowpipe Streaming** (for
  ingest, not CDC-out). A `RecordSource` impl here would poll/consume a Stream and treat "advanced the
  stream offset" as the ack — a materially different implementation shape from Postgres's
  `START_REPLICATION`/`CopyBoth` loop, even though it fits the same trait.

**What this means concretely:** the trait boundary (`RecordSource`) does not need to change to
support more databases. But there is no "generic SQL CDC" shortcut — each vendor is its own
protocol-level project, on the order of what `connector-postgres` already took, and each one needs
its own live-instance verification (per `CLAUDE.md`'s standing rule that replication code is only
trustworthy once checked against a real instance, not hand-constructed byte fixtures). Budget
accordingly: this is N separate substantial efforts, not one connector-abstraction afternoon each.

## 2. Object / blob cloud storage (S3, GCS, Azure Blob)

**Verdict: best near-term fit of the three. One concrete, currently-real gap: large-object hashing.**

### Why this fits well

Event notifications (S3 Event Notifications → SQS/SNS/EventBridge, GCS Pub/Sub object
notifications, Azure Event Grid for Blob Storage) map onto `RecordSource` more naturally than
database CDC does — it's closer in shape to a webhook/event source (already one of the two
"Additional connectors" the README's own roadmap lists) than to replication-log parsing. No
provider-specific wire protocol to hand-roll; these are well-documented, broadly-used pub/sub
mechanisms with mature client libraries. `source_position` maps cleanly onto e.g. an SQS message
receipt handle or a GCS Pub/Sub ack ID; acking is a single well-defined API call, same shape as
`connector-postgres`'s `LsnAckToken`.

### The real gap (now closed in `proof-core`): `SourceRecord.bytes: Vec<u8>` assumes small, in-memory content

```rust
pub struct SourceRecord {
    pub bytes: Vec<u8>,
    // ...
}
```

This is correct for a database row (bytes-of-a-row is inherently small) but wrong for a blob that
could be gigabytes. Two problems stack here:

1. **Memory.** Buffering a multi-GB object fully into a `Vec<u8>` just to hash it is a real resource
   problem at scale, not a style nitpick — this is exactly the kind of thing that works fine in a demo
   and falls over the first time someone anchors a video file or a large dataset export.
2. **`proof-core`'s hashing API is not streaming.** `proof_core::hash::hash(algorithm, data: &[u8]) ->
   Digest` (`crates/proof-core/src/hash/mod.rs`) takes a full byte slice. `blake3` itself *does*
   support incremental hashing natively (`blake3::Hasher::update`/`update_reader`) — this is not a
   library limitation, it's that `proof-core` never needed to expose it yet because every source so
   far has been small-record-shaped.

**Status: closed.** `proof-core`'s `hash` module now exposes `HashAlgorithm::hasher() -> Hasher`
(an incremental hasher wrapping `blake3::Hasher`/`sha2::Sha256`'s own native incremental APIs,
with `update(&mut self, &[u8])` and `finalize(self) -> Digest`) alongside a `hash_reader(algorithm,
impl Read) -> io::Result<Digest>` convenience for anything already `Read`-shaped (e.g. a local file).
A future blob connector reading chunks off an async SDK response body would call `hasher.update()`
directly in its own read loop rather than going through `hash_reader` (which is sync, by design —
`proof-core` stays free of any async/tokio dependency, matching its role as the dependency root
every other crate builds on). Verified bit-for-bit identical output to the existing one-shot `hash()`
for both algorithms, across arbitrary chunk splits (a proptest over random split points), the empty
case, and a real multi-read-iteration streamed read larger than the internal 64 KiB buffer — plus
that a genuine I/O error partway through `hash_reader` propagates rather than silently returning a
digest over a truncated read.

### A secondary, smaller gap: double at-least-once

Cloud pub/sub systems (SQS, GCS Pub/Sub, Event Grid) are themselves at-least-once, same as this
project's own `RecordSource`/`AckToken` contract. That's not a conflict — re-anchoring an identical
digest is wasteful but harmless, not incorrect — but it means a blob connector's dedup story needs
to be thought through explicitly (e.g. keying on object version/ETag) rather than assumed away,
since two independent at-least-once layers compound rather than cancel out.

### Not a gap, just a note

Object storage typically already provides a strong content hash (S3 ETag for non-multipart uploads,
GCS's `crc32c`/`md5Hash`). It's tempting to just anchor the provider's hash directly and skip
re-hashing — but that would mean trusting the cloud provider's own computation instead of
independently re-deriving it, which is a real trust-model downgrade from what `proof-core::hash`
does everywhere else (`verify()` always re-hashes and compares, never trusts a stored value at face
value). Streaming re-hash, not "trust the provider's ETag," is the approach consistent with the rest
of this codebase.

## 3. File systems

**Verdict: weakest fit — this is a missing primitive, not a missing connector.**

### The core problem: no durable "this is committed" signal

Every `RecordSource` so far (and everything reasoned about above) has one thing in common: the
underlying system provides some durable signal of "this change is committed and here is a cursor to
resume from" — a WAL position (Postgres), a Stream offset (Snowflake), a pub/sub message with an ack
ID (S3/GCS events). `AckToken`'s entire design (see `crates/proof-connectors/src/record.rs`'s module
docs) assumes acknowledging is meaningful because the source has its own durable notion of "advance
past this."

A plain filesystem has no equivalent primitive:

- **`inotify`/`FSEvents`/`ReadDirectoryChangesW`** (Linux/macOS/Windows respectively — already a
  portability problem in themselves, in tension with `CLAUDE.md`'s explicit "favor portable, standard
  interfaces" principle, since each OS needs separate code) report *filesystem events*, not
  *committed changes*. There is no "offset" to resume from after a crash — if the watcher process was
  down, events that occurred during the gap are simply gone; the only fallback is a full re-scan,
  which doesn't tell you what changed, only current state.
- **Partial writes are directly observable and unguarded against.** A watcher can see (and hash) a
  file mid-write — most applications write via `write()` calls that aren't atomic from an outside
  observer's perspective, so "file modified" events can fire multiple times during a single logical
  write, some of which observe a half-written, semantically-invalid intermediate state. A database's
  WAL only ever exposes *committed* transactions; a filesystem watcher has no equivalent "don't show
  me uncommitted writes" guarantee at all. Anchoring a proof over a half-written file's digest would
  be a genuinely wrong proof, not just an inefficiency — this is a correctness gap, not a performance
  one.
- **No natural at-least-once/redelivery story.** `RecordSource`'s contract explicitly allows
  redelivery after a crash (see its doc comment), which every backing system above provides for free
  via its own durable cursor. A filesystem watcher restarted after a crash has no way to know what it
  missed except polling/re-scanning the whole tree and diffing against last-known state — workable,
  but a fundamentally different (and much more expensive) implementation shape than every other
  source, and one this project hasn't designed for.

### What would actually be needed

Not a connector — a design decision about what "committed" even means for a filesystem, likely one
of:
- **Punt on true CDC**: periodic full-tree scan + content-hash diffing (like a backup tool), accepting
  it's not real-time and can miss transient states, but sidesteps the partial-write and no-durable-
  cursor problems by only ever looking at "settled" file state between scans.
- **Redirect to a managed sync layer instead** (e.g. treat files synced through S3/GCS/a managed file
  gateway as the actual source, per section 2, rather than watching a raw local filesystem directly)
  — turns "file system" into "just another object store," inheriting the streaming-hash gap from
  section 2, but at least getting a real event/commit model for free instead of inventing one.
- **Require an application-level convention** (e.g. write-to-temp-then-atomic-rename, which many
  systems already do) so the watcher can distinguish "in-progress write" from "committed file" —
  pushes the correctness burden onto whatever writes the files, which may not be under this project's
  control for a "zero data migration" integration.

None of these is a small addition to `RecordSource`; each is a real design choice with a genuine
trust-model tradeoff, which is why file systems are called out as the weakest fit rather than "just
another connector to write."

## Bottom Line

| Target | Trait-level fit | Concrete gap | Size of gap |
|---|---|---|---|
| Databases (new vendors) | ✅ Sound | None in the trait itself | Large but well-understood — each vendor is its own protocol-level effort, no shortcut |
| Object/blob storage | ✅ Sound | ~~Streaming/chunked hashing in `proof-core`~~ **closed** — only the connector (event-notification wiring) is left to build | Small — one connector, no protocol to hand-roll |
| File systems | ❌ Weak | No durable commit/cursor signal to ack against; partial-write visibility is a correctness risk, not just a performance one | Requires a real design decision, not an implementation task |

If the near-term goal is "prove this product works for a third data-source class beyond Postgres,"
**object/blob storage is the right next target**: `proof-core::hash` now exposes `HashAlgorithm::hasher()`
(incremental) and `hash_reader()` (a `Read`-based convenience) alongside the original one-shot
`hash()`, closing the one real gap this class of source had. What's left is building the actual
connector — wiring S3/GCS/Azure event notifications into a `RecordSource` impl — which should be
genuinely simpler to build and verify than `connector-postgres` was (no wire protocol to hand-roll —
a well-documented, mature-client-library pub/sub mechanism instead). File systems should be treated
as a deferred, explicitly-scoped design decision, not queued up as "just another connector."
