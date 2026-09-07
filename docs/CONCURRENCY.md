# Shared database concurrency

PicoVolt's first 2.0 concurrency slice adds a native Rust coordinator without
changing the version-5 file format or the existing `Database` API.
`SharedDatabase` is cloneable, `Send + Sync`, and owns the single-threaded engine
on a private worker thread. Clone **one coordinator** into every application task
that needs the same database:

```rust,no_run
use picovolt::{SharedDatabase, Value};

let database = SharedDatabase::open_dev("./app.pv")?;
let background = database.clone();

std::thread::spawn(move || -> picovolt::Result<()> {
    background.query_with(
        "INSERT INTO jobs VALUES (?, ?)",
        &[Value::Int(7), Value::Text("queued".into())],
    )?;
    Ok(())
});

let mut write = database.begin_write()?;
write.query("UPDATE accounts SET balance = 40 WHERE id = 1")?;
write.query("UPDATE accounts SET balance = 60 WHERE id = 2")?;
write.commit()?;

let mut read = database.begin_read()?;
let pinned_at = read.snapshot_tx();
let result = read.query("SELECT * FROM accounts ORDER BY id")?;
read.close()?;
# let _ = (pinned_at, result);
# Ok::<(), picovolt::PvError>(())
```

## Scheduling and backpressure

The coordinator accepts top-level operations in FIFO admission order through a
bounded channel. The default waiting capacity is 64; configure it with
`SharedDatabaseOptions`. Submission never waits for queue space: a full queue
returns `PvError::Busy`, and the caller can shed load or retry under its own
policy. Once admitted, a synchronous call waits for its terminal response.

An explicit transaction counts as one admitted operation and owns the worker
until it commits, rolls back, expires, is cancelled, or is dropped. Its private
control channel remains available even when the public queue is full, so queued
callers cannot prevent the owner from finishing. Dropping a write handle is
non-blocking; the worker rolls it back before executing the next queued command.
Explicit `commit`, `rollback`, and read `close` wait for a terminal response.
If the final coordinator-owning handle is dropped, cleanup continues on the
detached worker, so an immediate reopen can briefly observe the transaction lock.

This initial implementation deliberately serializes read transactions as well
as writes. A read handle pins both the catalog and the latest committed MVCC id,
rejects mutations, and excludes writers for its lifetime. Parallel execution of
snapshot readers requires immutable catalog/page state and remains a subsequent
2.0 storage slice.

## Limits, cancellation, and errors

`RequestOptions` combines `QueryLimits` with a clonable `CancellationToken`.
Limits apply to each statement and form a ceiling: a transaction's
`query_with_limits` may tighten them but cannot weaken them. The deadline also
bounds an idle transaction. Cancellation is cooperative and is observed before
queued work starts, while a session is idle, between statements, and at scan and
materialization checkpoints.

A write statement error, observed cancellation, expired deadline, engine panic,
or abandoned write handle rolls back the whole explicit transaction. Commit and
filesystem flushes are intentionally non-interruptible: cancellation arriving
after commit begins may be followed by a successful commit, never by a premature
`Cancelled` response while the outcome is still changing.

The concurrency-specific errors are:

- `Busy`: the bounded queue rejected the operation; it did not start.
- `Cancelled`: cooperative cancellation was observed.
- `TransactionClosed`: an explicit write session already ended or aborted.
- `DatabaseClosed`: the owning worker is no longer available.
- `TransactionOutcomeUnknown`: commit/recovery I/O failed across an ambiguous
  durability boundary. The coordinator closes; reopen and inspect the workspace
  before deciding whether to retry.

SQL `BEGIN`, `COMMIT`, and `ROLLBACK` are rejected on this surface. Use the
explicit handles so transaction ownership cannot leak between queued callers.

## Current boundaries

- The guarantee covers clones of **one** `SharedDatabase`. Do not independently
  open the same development workspace through multiple long-lived
  `SharedDatabase` or `Database` instances. The filesystem lock prevents
  overlapping write transactions, but independent handles retain separate
  catalogs and caches and can become stale after another handle commits.
- The coordinator is native-only. Browser and Node WebAssembly builds retain the
  existing Web Worker ownership model; C, Go, Python, and JavaScript bindings are
  unchanged in this slice.
- `open_streamed` is not exposed yet because the existing `RangeReader` contract
  is not thread-transferable.
- Version-5 files and all earlier 1.x images remain readable. This slice adds no
  commit log, change stream, migration, or format-version bump.
- The current filesystem transaction protocol creates a complete rollback image,
  so beginning any write—including a one-statement mutation submitted directly
  to `SharedDatabase`—is O(database size). The incremental commit log planned for
  2.0 removes that cost.
