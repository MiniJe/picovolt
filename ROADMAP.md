# Roadmap

This describes the direction of the project, not a commitment. Priorities shift
with what users need, and dates are deliberately omitted. Items are grouped by
horizon. Changes that have landed are recorded in [CHANGELOG.md](CHANGELOG.md).
The distribution and adoption plan behind the one-million-download goal lives in
[docs/ROADMAP_1M_DOWNLOADS.md](docs/ROADMAP_1M_DOWNLOADS.md).

PicoVolt reached **1.0**: the public API and the `.pvdb` on-disk format are stable
under SemVer. Features arrive in minor releases, and breaking changes wait for a
major. The near-term, versioned plan is under **Planned 1.x releases** below;
bigger, breaking ideas are under **2.0 candidates**.

## Shipped in 0.1.0

The core engine: VLE development, production, and in-memory backends; page-backed
storage with O(1) appends; a bounded buffer pool; MVCC time-travel; CAS dedup;
columnar compression; secondary indexes; selectable durability; the WebAssembly
extension sandbox; an SQL front-end; and the WebAssembly and npm bindings.

## Recently added (on main)

- **Persisted binary secondary indexes:** baked version-2 images store compact
  index regions and reopen without rebuilding every index from table scans.
- **Bounded server execution:** network binds require bearer authentication, and
  server queries now have queue, scan, memory, result, response, and time limits.
- **First-class CLI and distribution:** `pv` imports/exports common text formats,
  queries and inspects databases, and bakes production images; releases attach
  native tools, SBOMs, checksums, and provenance attestations.
- **Compatibility foundations:** reusable prepared statements, column constraints,
  equality `INNER`/`LEFT JOIN`, and atomic in-memory transaction wrappers.
- **Everyday SQL and inspection:** multi-row inserts, `LIMIT`/`OFFSET`, projected
  and distinct equality joins, and `pv history` snapshot inspection.
- **Durable browser path:** an OPFS wrapper and Web Worker RPC entry point keep
  storage durable and queries off the UI thread.

- **Richer WHERE predicates:** comparison operators (`<`, `<=`, `>`, `>=`, `!=`,
  `<>`), `AND` and `OR` with parentheses, and `LIKE` (`%` and `_`) for `SELECT`,
  `UPDATE`, and `DELETE`.
- **Whole-table aggregates:** `COUNT`, `SUM`, `MIN`, and `MAX`, over the full or
  `WHERE`-filtered result.
- **Ordered, range-capable secondary indexes:** `CREATE INDEX` builds a
  `BTreeMap`-backed index. Range predicates such as `col > v` use it for an
  ordered scan instead of a full scan, alongside the existing point lookups.
- **Index-accelerated `ORDER BY`:** a `SELECT ... ORDER BY indexed_col` with no
  `WHERE` reads the index in key order and skips the sort, and a `LIMIT` stops the
  read early.
- **`GROUP BY`:** group rows by one or more columns and evaluate `COUNT`, `SUM`,
  `MIN`, and `MAX` per group.
- **Fixed-point decimal values:** a storable `Value::Decimal` type (exact, totally
  ordered) with SQL literals; `AVG` returns it instead of text. Packed columnar
  pages still fall back to row storage when a decimal is present.
- **`AVG`:** averages an integer column, on its own or under `GROUP BY`, returning
  an exact decimal.
- **Positioned parse errors:** parse and tokenizer errors report the line and
  column of the offending token and draw a caret under the source.
- **Streaming reads:** `Database::for_each_row` visits visible rows one at a time
  instead of materializing the full result, for bounded-memory processing of large
  tables.

## Native language bindings (shipped in 0.4.0)

PicoVolt exposes a C ABI (the `capi` feature, [`src/ffi.rs`](src/ffi.rs), header
[`include/picovolt.h`](include/picovolt.h)) so it can be embedded from any
language with a C FFI. Two bindings ship on top of it in
[`bindings/`](bindings): **Go** (cgo) and **Python** (ctypes). They surface the
engine's strengths, an embedded single-writer engine with SQL, MVCC time-travel,
and a compile-to-`.pvdb` path; they do not add JOINs, transactions, or concurrent
writers, so they suit embedded use rather than a concurrent server's primary
store.

## Planned 1.x releases

Versioned, non-breaking targets: features land in minor releases, and nothing here
changes the public API or breaks 1.x file compatibility (a newer build always reads
an older 1.x file). Order is by impact (informed by where evaluators say the engine
is weakest) and is direction, not a schedule.

### 1.5: Crash-safe filesystem transactions

`BEGIN` / `COMMIT` / `ROLLBACK` for filesystem workspaces and native bindings, built on
the MVCC machinery that already exists: multi-statement atomicity and rollback, not
just per-statement autocommit. The most-requested correctness feature.

This is the next engine milestone. It requires a write-ahead or copy-on-write
commit protocol plus power-loss tests; it will not be implemented as a
best-effort rollback wrapper.

### 1.5: Finish richer JOINs

Two-table equality `INNER JOIN` and `LEFT JOIN` now use a hash-style lookup and
support dotted identifiers, projection, `DISTINCT`, filtering, ordering, and
pagination. Next are N-table plans, aliases, and index-assisted planning.

### SQL ergonomics

`OFFSET` and multi-row inserts are now on main. Next are `CASE WHEN`, more scalar
functions (string / number), richer DDL defaults, and simple scalar
subqueries in `WHERE` / `IN`. Incremental polish that closes the gap with everyday
SQL.

### Smaller items, any release

- **Decimals in the columnar layout.** Decimal values are storable in row form;
  encoding them in the packed columnar layout (today such pages stay in row form) is
  the remaining piece.
- **Background columnar compaction.** Promote the on-demand row-to-columnar
  transposition ([`storage/page.rs`](src/storage/page.rs)) to a background worker.
- **Forward format migration.** Read older `FORMAT_VERSION`s in place rather than
  requiring a re-bake.
- **CLI follow-ups.** `pv history` now summarizes recent snapshots. Add row-level
  time-travel diffs, Parquet, and binary SQLite import.

## Bindings and extensions

The C ABI opens two directions that grow independently of the core engine.

- **More bindings.** A Go `database/sql` driver and pip-installable Python wheels
  that bundle the shared library both shipped in 0.5.0. Still open: Go ORM
  adapters and a documented C example. Because the bindings share one C ABI, new
  languages (Ruby, C#, Java, Zig) are wrappers rather than new engine work.
- **Drop-in compatibility.** Parameterized queries (`?` placeholders) shipped in
  0.6.0 and now span the C ABI and language bindings. A Go `database/sql` driver,
  `better-sqlite3`-style JavaScript adapter, and Python DB-API 2.0 module have also
  shipped. Next: prepared-statement objects and compatibility test suites drawn
  from real applications.
- **Functional plugins.** The `WasmExec` trait is an existing extension seam.
  More seams of the same shape could allow:
  - additional index types behind `CREATE INDEX`, such as a full-text index or a
    vector/embedding index for nearest-neighbor search;
  - pluggable storage backends behind the VLE, such as an object-store backend;
    OPFS snapshot persistence and a worker endpoint now ship for WebAssembly;
  - import and export adapters beyond the shipped CSV, JSONL, and SQLite SQL-dump
    support, especially Parquet and direct binary SQLite ingestion;
  - alternative compression codecs.

## 2.0 candidates (breaking)

Bigger pieces that would change the public API or the concurrency model, so they
wait for a major version. (The HTTP/JSON **server mode** that was once the big next
step shipped in 0.10.0.)

- **Concurrent writers.** The engine is single-writer today: one thread owns it,
  and the server serializes requests through that thread. True multi-writer
  concurrency is the prerequisite for a general multi-client store and almost
  certainly an API change.
- **Encryption at rest** and **replication** for confidentiality and a warm copy.
- **A native OPFS VLE backend.** The 1.x JavaScript wrapper persists snapshots in
  OPFS; direct page-level OPFS access belongs here if it changes the open/init API.
- **Local-first sync.** Operation-log or CRDT sync between an in-browser PicoVolt
  and a server.

## Maturity track (runs alongside every version)

Trust in a database is earned over time, not declared at a version bump. These run
in parallel to the feature releases above and are what actually gate production
confidence:

- **External security audit.** None yet; the highest-value trust item.
- **Sustained fuzzing.** The decoders and the SQL parser are fuzzed per commit;
  1.x calls for a long-running soak, not just CI runs (see [SECURITY.md](SECURITY.md)).
- **Crash-injection.** Read-side corruption is covered by injection tests; still
  wanted is true power-loss injection (killing a process mid-flush) behind the
  `Sync` durability claims, plus index crash-consistency once indexes persist (1.1).
- **Extension contract.** Stabilize the crate-root seams documented in
  [docs/EXTENDING.md](docs/EXTENDING.md).

## Out of scope

These keep the project focused:

- It is not aiming to be a drop-in SQL-92 or PostgreSQL-compatible database.
- No distributed consensus or multi-node clustering.
- `pv-wasm` stays an integer-subset interpreter. Floats, SIMD, and tables remain
  the `wasmi` backend's responsibility rather than a reimplementation.

## Suggesting changes

The ordering above is a starting point. To influence it, open an issue describing
the problem you have rather than only the feature you want; concrete use cases are
what move items up the list. See [CONTRIBUTING.md](CONTRIBUTING.md).
