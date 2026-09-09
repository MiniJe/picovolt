# PicoVolt 2.0: faster writes and simpler operation

This guide describes **2.0.0**. Use matching 2.0 libraries and native binaries;
the maintained starters pin this exact version. See the
[release ledger](RELEASE_2_0.md) for publication status. Go imports use
`github.com/MiniJe/picovolt/bindings/go/v2`.

## Choose storage

- **Memory**: temporary data and tests; nothing survives process exit unless exported.
- **Development workspace**: a writable directory. Enable the commit log for
  synced, atomic SQL mutations, recoverability and physical change consumption.
- **Production `.pvdb` image**: a read-only artifact opened with `open_prod`.
  Importing bytes into a memory database creates a writable in-memory copy.
- **Browser**: WASM is in memory; `PersistentDb` explicitly saves a baked image
  to OPFS. Native filesystem commit logs are not browser persistence.

## Batch writes

Use `execute_many` / `ExecuteMany` / `executeMany` when writing multiple rows.
It owns one transaction, validates parameter counts before writing, and rolls
back the whole batch if any row fails. INSERT (including named columns), UPDATE
and DELETE are supported. An active transaction is rejected rather than silently
committing caller-owned work. Empty batches return zero. Bound batches to fit
your memory and transaction-log budgets; inputs are materialized.

### Rust

```rust
use picovolt::{CommitLogOptions, Database, Value};
let mut db = Database::open_dev("notes-workspace")?;
db.enable_commit_log(CommitLogOptions::default())?;
db.query("CREATE TABLE notes (id PRIMARY KEY, body)")?;
let changed = db.execute_many("INSERT INTO notes VALUES (?, ?)", &[
    vec![Value::Int(1), Value::Text("first note".into())],
    vec![Value::Int(2), Value::Text("second note".into())],
])?;
assert_eq!(changed, 2);
let status = db.commit_log_status()?;
println!("{} bytes retained", status.retained_bytes);
Ok::<(), picovolt::PvError>(())
```

For concurrent ownership use `SharedDatabase`: keep a `begin_read()` handle
while writers proceed; use one `begin_write()` transaction for a group of
mutations. See [the concurrency contract](CONCURRENCY.md).

### Python

```python
from picovolt import Database
with Database.open_dev("notes-workspace") as db:
    db.enable_commit_log()
    db.query("CREATE TABLE notes (id PRIMARY KEY, body)")
    changed = db.execute_many("INSERT INTO notes VALUES (?, ?)", [
        (1, "first note"), (2, "second note"),
    ])
    print(changed, db.commit_log_status())
```

The existing DB-API `executemany` keeps its DB-API transaction behavior; the
explicit `Database.execute_many` above is an atomic operation with one FFI call.
Use each example on a fresh workspace; table creation deliberately reports an
error if the table already exists.

### Go

```go
db, err := picovolt.OpenDev("notes-workspace")
if err != nil { return err }
defer db.Close()
if err := db.EnableCommitLog(picovolt.CommitLogOptions{}); err != nil { return err }
if _, err := db.Query("CREATE TABLE notes (id PRIMARY KEY, body)"); err != nil { return err }
changed, err := db.ExecuteMany("INSERT INTO notes VALUES (?, ?)", [][]any{
    {1, "first note"}, {2, "second note"},
})
if err != nil { return err }
fmt.Println(changed)
```

The source module is `github.com/MiniJe/picovolt/bindings/go/v2`. Link the
matching native C ABI library and configure its loader path as described in
[the Go README](../bindings/go/README.md).

### C

```c
#include "picovolt.h"
#include <stdio.h>
int main(void) {
    PvDb *db = pv_open_dev("notes-workspace");
    if (!db) { fprintf(stderr, "%s\n", pv_last_error()); return 1; }
    if (!pv_enable_commit_log(db, 0, 0, 0)) goto failed;
    char *result = pv_query(db, "CREATE TABLE notes (id PRIMARY KEY, body)");
    if (!result) goto failed;
    pv_string_free(result);
    result = pv_execute_many(db, "INSERT INTO notes VALUES (?, ?)",
                             "[[1,\"first note\"],[2,\"second note\"]]");
    if (!result) goto failed;
    puts(result);
    pv_string_free(result);
    pv_close(db);
    return 0;
failed:
    fprintf(stderr, "%s\n", pv_last_error());
    pv_close(db);
    return 1;
}
```

Use [the matching header](../include/picovolt.h). Handles require external
synchronization; returned JSON strings belong to the caller and must be freed.
Copy error text before making another native call on that OS thread.

### JavaScript and browser workers

```javascript
import Database from "picovolt/sqlite";
const db = new Database();
db.exec("CREATE TABLE notes (id PRIMARY KEY, body)");
const changed = db.executeMany("INSERT INTO notes VALUES (?, ?)", [
  [1, "first note"], [2, "second note"],
]);
db.close();
```

The raw WASM `Db.executeMany(sql, rows)` returns a JSON string containing
`mutated`; the SQLite-style and `PersistentDb` adapters return the count.
For OPFS use `await persistent.save()` after a batch, or `await persistent.close()`.
The module worker accepts `{id, method: "executeMany", sql, rows}` and returns
the count. Send `save` and await its reply when persistence is required.

### CLI

```text
pv log-enable notes-workspace
pv query notes-workspace "CREATE TABLE notes (id PRIMARY KEY, body)"
pv batch notes-workspace "INSERT INTO notes VALUES (?, ?)" rows.json
pv log-status notes-workspace
```

`rows.json` contains `[[1,"first note"],[2,"second note"]]`. `pv batch` executes
one atomic transaction and prints `{"mutated":2}`. `log-status` reports exact
sequence cursors, retained bytes/commits and current per-handle limits.

### HTTP server

Start the matching `picovolt-server --dev notes-workspace` binary. To use durable
logged mutations, first initialize the workspace with `pv log-enable` while the
server is stopped. Send `POST /v1/query` with `Content-Type: application/json`:

```json
{"sql":"SELECT body FROM notes WHERE id = ?","params":[1]}
```

The result uses the same `columns` / `rows` JSON shape as the native bindings.
Use `GET /v1/health` for liveness and `GET /v1/tx` for the current MVCC clock.
The server benefits from the same indexed-query and aggregation optimizations.
Its queries have scan, result, memory and deadline limits. A 503 means the
bounded queue is full or the engine is unavailable; a 504 requires reconciling
the result before retrying a mutation. Read the JSON `error` for invalid SQL or
parameters. Explicit transactions require an embedded session; separate HTTP
requests cannot share a transaction. Use a multi-row INSERT for a single atomic
HTTP write on a logged workspace. For large parameter batches use a maintained
embedded binding. Non-loopback binds require a bearer token; configure
`--token-file` and a TLS reverse proxy. Browser Origin requests are rejected.

Stop the server before CLI maintenance: independent handles do not share caches.

## Manage retention deliberately

Default native limits are 64 MiB per transaction, 256 MiB retained history and
4096 retained commits. A limit rejects a write with an actionable error; it does
not discard unacknowledged changes. Options are per handle: reapply custom limits
after reopening. Rust, C, Python and Go expose configuration and diagnostics.

1. Read `changes_since(after_sequence, limit)`; sequence numbers are **not** MVCC
   transaction IDs. Batches are bounded to 4096 commits and 256 MiB.
2. Persist/apply changes in every consumer that needs them, then record its
   acknowledgement. Use the slowest required consumer's acknowledged sequence.
3. Call `prune_changes(acknowledged_sequence)` or
   `pv log-prune notes-workspace <acknowledged-sequence>`.
4. If no consumer needs physical history, the host may explicitly acknowledge
   the head reported by `commit_log_status` and prune it. This removes change
   history, not the current database or its MVCC row versions.

A pruned cursor returns an error: obtain a verified base checkpoint instead of
guessing a replacement cursor. Native Rust checkpoint export returns both the
image's verification hash and the correct following sequence. Do not delete
`.pv-log` or its `active` directory manually to recover space.

## Diagnose a slow or failed operation

| Symptom | Next action |
| --- | --- |
| Slow small writes | Batch related mutations into one commit; inspect retained log size |
| Log byte/count limit | Acknowledge and prune consumed history, or deliberately configure a larger budget |
| One transaction exceeds its limit | Reduce batch size; the entire rejected batch is rolled back |
| Snapshot admission rejected | Close completed readers; review snapshot count/image limits |
| Unexpected scan | Use `pv explain` / `EXPLAIN SELECT ...`; index selective filter columns |
| Read-only error | Open a writable workspace or writable memory copy |
| Missing native library/symbol | Install/build the matching 2.0.0 wheel/library; do not mix versions |
| Transaction outcome unknown | Close and reopen to recover/inspect; reconcile before retrying the mutation |

See [the independent review prompt](INDEPENDENT_REVIEW_PROMPT.md) and
[the release ledger](RELEASE_2_0.md) for what remains before stable publication.

## Primary keys and imports

PRIMARY KEY and UNIQUE columns receive indexes automatically. Existing writable
workspaces rebuild missing constraint indexes on open. No extra CREATE INDEX is
needed for the key; large batches should still use the bulk helpers above.

SQL dump import skips entire unsupported trigger definitions and reports them.
Comments do not hide subsequent statements. An incomplete quote, block comment
or trigger rejects the dump before any statement runs. Other SQL execution
errors retain the documented best-effort import behavior; inspect the report.

A missing-history error means the database can no longer prove its change cursor.
Restore a verified backup with its history. Do not delete logs to clear the error.
An intact RC2 log upgrades on the next write; a fully pruned RC2 log cannot prove
its original cursor and requires a verified base image. Keep the original backup
before moving an unpublished candidate workspace to RC3.
