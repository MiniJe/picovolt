# PicoVolt 2.0 concurrency contract

`SharedDatabase` is a native Rust `Send + Sync` handle. Clone one coordinator
into application tasks. Writers use bounded FIFO admission. An explicit write
owns the writer worker until commit, rollback, cancellation, expiry or drop.
Each explicit reader owns an independent immutable image and a separate worker,
so existing readers continue while later writes update or drop tables.

```rust,no_run
use picovolt::{SharedDatabase, Value};
let db = SharedDatabase::open_dev("./app.pv")?;
db.query("CREATE TABLE accounts (id, balance)")?;
db.query("INSERT INTO accounts VALUES (1, 100)")?;
let mut before = db.begin_read()?;
let mut write = db.begin_write()?;
write.query_with("UPDATE accounts SET balance = ? WHERE id = 1", &[Value::Int(80)])?;
write.commit()?;
// Still 100: catalog, indexes, blobs, pages and history are pinned.
let original = before.query("SELECT balance FROM accounts WHERE id = 1")?;
before.close()?;
# let _ = original;
# Ok::<(), picovolt::PvError>(())
```

## Scheduling and snapshots

Public admission is FIFO, with 64 waiting slots by default. Submission never
waits for queue space: `Busy` means it did not start. Accepted synchronous calls
wait for terminal responses. A write has a private control channel, so a full
public queue cannot prevent commit/rollback. Dropping a write is non-blocking;
rollback finishes before queued work. Dropping the final shared handle detaches
cleanup; an immediate independent reopen can briefly encounter the writer lock.

Read admission builds a snapshot on the writer worker, then releases it. New
read admissions and top-level queries wait behind an active write. Existing
read handles execute concurrently. Top-level `SharedDatabase::query` reads run
on the coordinator; use `begin_read` for independent execution. A read handle
is `Send`, not `Sync`, and accepts one query at a time through `&mut self`.

Snapshots copy a full committed image, so construction is O(image size).
In-memory database snapshots stay in memory. Filesystem and production-image
snapshots stream into a private temporary file, then open an owned immutable
mapping. This gives stable schema and index semantics across later mutations;
it is not zero-copy MVCC.

## Limits and cancellation

Configure `SharedDatabaseOptions::new(queue_capacity)` with
`with_snapshot_limits(max_readers, max_image_bytes)` and
`with_commit_log(CommitLogOptions { ... })`. Defaults:

| Resource | Limit |
| --- | --- |
| Waiting public operations | 64 |
| Live reader workers | 16 |
| Each snapshot image | 256 MiB |
| Each transaction journal | 64 MiB |
| Retained log files | 256 MiB |
| Retained commits | 4096 |
| Change batch | 4096 commits and 256 MiB serialized bytes |

A full reader pool returns `Busy`; byte/count limits return `ResourceLimit`.
Failed writes roll back. Readers and history are never silently evicted. The
journal budget includes undo pages, metadata and compact binary physical changes.
The public JSON change API can expand binary payloads. These limits do not bound total database size,
transient SQL input, or unreferenced CAS blobs. Rolled-back appends and blobs
may remain unreachable. Admission scans retained log files for accounting;
prune regularly to bound that cost as well as disk use.

During filesystem snapshot opening, two private copies can briefly coexist.
Memory snapshot opening holds both serialized and decoded images. Each reader
also has its own page cache (16 MiB default), catalog and decoded indexes; OS
mapping caches add memory use. The image-byte cap is not a total-RSS cap.
Use `RequestOptions`/`QueryLimits` to bound query scanning, materialization and
results. Their defaults remain unbounded for compatibility.

`RequestOptions` also accepts a clonable `CancellationToken`. A transaction's
limits are ceilings; individual queries may tighten them. Checks occur before
execution, between snapshot-output writes, during session idle waits, between
statements and at bounded-query checkpoints. Filesystem I/O, snapshot decoding,
and commit are non-interruptible while in progress. Deadlines are therefore
not hard wall-clock bounds on those operations.

Write errors, observed cancellation, expiry, panic or drop abort the whole
transaction. Once commit starts, cancellation may be followed by success;
the API never returns cancellation while its commit outcome is still changing.
Lifecycle errors are `TransactionClosed`, `DatabaseClosed` and
`TransactionOutcomeUnknown`. Unknown recovery/durability outcomes close the
coordinator; reopen and inspect committed state and sequences before retrying.
A reader error does not abort other readers or the writer. SQL transaction
control is rejected on shared handles; use explicit transaction methods.

## Durable commits and extension points

Shared filesystem handles enable the incremental page journal by default.
Begin saves the prior manifest. Before a page's first overwrite, its original
bytes are saved and synced. Commit syncs pages, newly referenced blobs and the
manifest, writes a checksummed change record, then renames the active journal
to its sequence number. That rename publishes the commit. Opening an active
journal rolls it back. See [FORMAT.md](FORMAT.md) for framing, recovery and
Windows directory-sync limitations. Process-crash tests are not power-cut tests.

`changes_since(after, limit)` returns an exclusive-cursor batch. Sequences order
commits separately from MVCC ids: one transaction can contain several mutations
or catalog-only changes. Rollbacks emit no entry. `prune_changes(through)` deletes
history only when called explicitly. Pass the minimum durably acknowledged
sequence across every required consumer. A pruned cursor fails and needs a new
verified base image; it never silently skips data.

A schema-1 `ChangeCommit` contains final page images, new CAS blobs, the complete
final manifest and before/after clocks. Physical replay requires a matching
verified base image and format. `export_checkpoint(destination)` holds writer admission while exporting and
verifying an image with its exact following sequence cursor. Publication is
no-clobber and must be outside the live workspace. Use `changes_since` from
that sequence to continue; automatic replica bootstrap remains host-owned.
`ChangeSink::accept` and `visit_changes` run host code outside the database
worker for encrypted transport, replication, backups and auditing. Sink errors
never reverse commits. Retry delivery is at least once: sinks must deduplicate
sequences and persist their own checkpoints. Live database encryption, key
management, automatic replication and distributed consensus are not implemented.

The CLI provides `pv changes <workspace> <after-sequence> [limit]` as JSONL and
`pv log-prune <workspace> <acknowledged-sequence>`.

RC.2 also exposes `commit_log_status` (head, pruned cursor, retained bytes/count
and current limits), native binding configuration, `pv log-enable` and
`pv log-status`. See [the interface guide](QUICKSTART_2_0.md). Logged workspaces
save index definitions rather than every index entry at commit. Opening rebuilds
all defined indexes in one scan per table, trading startup CPU for smaller
durable writes. Baked production images retain persisted binary indexes.

## Migration and compatibility

Back up a quiescent 1.x workspace with `pv bake` before enabling the shared API.
Its first logged transaction advances the manifest to format 6, so 1.x binaries
reject it instead of bypassing recovery. Copy the whole development workspace,
including `.pv-log`. Do not downgrade a live logged workspace to 1.x.

`pv migrate source.pvdb destination.pvdb --backup backup.pvdb` reads every
historical golden format, verifies full history/content, and refuses to clobber
destination or backup. `--dry-run` writes nothing. Rollback uses the unchanged
source or exact backup. A v6 golden joins the migration/corruption corpus.

`Database::enable_commit_log` enables the same protocol on the sequential API.
Reopened logged workspaces automatically wrap mutating SQL in transactions;
low-level mutation methods require explicit transactions. Unlogged databases
retain the legacy rollback-image protocol. C, Go and Python sequential adapters
and the browser/Node Web Worker model remain supported. New shared handles are
currently a native Rust API. `open_streamed` remains outside this API because
`RangeReader` is not thread-transferable.

The guarantee covers clones of one coordinator. Independent live handles keep
separate caches/catalogs; never write through multiple coordinators or mix raw
handles with a live coordinator. The filesystem transaction lock prevents
overlapping writes, but does not provide cache coherence. No runtime telemetry,
account requirement or network dependency is added.

## Measured envelope

Run `cargo run --release --example concurrency_envelope`: 2000 CAS-backed rows,
four readers, 160 count queries and 40 durable inserts. The
[historical RC.1 Windows run](../benchmarks/concurrency-v2-windows.json) measured reader
p50/p95 of 0.067/0.088 ms, writer p50/p95 of 82.9/94.8 ms, and 474 ms total to
open four snapshots. The final insert journaled one page of a 24-page database.
These are local observations, not service-level guarantees. Snapshot creation
and retained-log accounting remain optimization targets. Size limits using
representative data and the intended filesystem.

The [RC.2 rerun](../benchmarks/concurrency-v2-rc2-windows.json) measured reader
p95 0.190 ms, writer p95 75.3 ms and 438 ms for four snapshot admissions, with
one final changed page. This is a single local run, not a repeated latency
guarantee; its higher reader p95 is retained alongside the write improvement.
See [the repeated competitor report](../benchmarks/COMPETITORS_2_0_RC2.md) for
separate Python API measurements, CPU, memory and remaining regressions.

`SharedDatabase::compact_step` queues journaled page maintenance with the same
writer ordering. Existing read snapshots remain stable through compaction.
