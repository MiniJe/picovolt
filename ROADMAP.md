# Roadmap

This describes the direction of the project, not a commitment. Priorities shift
with what users need, and dates are deliberately omitted. Items are grouped by
horizon. Changes that have landed are recorded in [CHANGELOG.md](CHANGELOG.md).

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
- **Fixed-point decimal values:** a `Value::Decimal` type (exact, totally ordered)
  that `AVG` now returns instead of text. It is not yet storable on disk or
  constructible from a literal.
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

### 1.1: Persistent secondary indexes

Indexes are rebuilt by a full scan on every open today. Persist them in the
workspace and the baked `.pvdb` (a new `FORMAT_VERSION` that newer builds read
alongside v1), so a large table opens in roughly constant time instead of scanning
every row. This is the single biggest "is this production-real?" gap.

### 1.2: Explicit transactions

`BEGIN` / `COMMIT` / `ROLLBACK` across the engine API and every binding, built on
the MVCC machinery that already exists: multi-statement atomicity and rollback, not
just per-statement autocommit. The most-requested correctness feature.

### 1.3: JOINs

`INNER JOIN` and `LEFT JOIN` in `SELECT`: nested-loop first, an index/hash join
when a join key is indexed. The largest single piece and the most-cited missing SQL
feature; two-table joins first, then N-table.

### 1.4: Server hardening

Bearer-token authentication and optional TLS for `picovolt-server` (it already caps
body size and times out slow statements). Turns the demonstration server into
something that can safely sit on a network.

### 1.5: SQL ergonomics

`OFFSET`, `CASE WHEN`, more scalar functions (string / number), and simple scalar
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
- **A `pv` CLI.** Promote the `repl` example into a real binary for import/export,
  inspection, and time-travel diffs.

## Bindings and extensions

The C ABI opens two directions that grow independently of the core engine.

- **More bindings.** A Go `database/sql` driver and pip-installable Python wheels
  that bundle the shared library both shipped in 0.5.0. Still open: Go ORM
  adapters and a documented C example. Because the bindings share one C ABI, new
  languages (Ruby, C#, Java, Zig) are wrappers rather than new engine work.
- **Drop-in compatibility.** Parameterized queries (`?` placeholders) shipped in
  0.6.0, the foundation for using PicoVolt the way other SQL databases are used.
  Next: surface them in the Go `database/sql` driver, the C ABI, and Python, then
  offer familiar adapter shapes (a `better-sqlite3`-style JavaScript API and a
  Python DB-API 2.0 interface) so existing apps can swap PicoVolt in with minimal
  change.
- **Functional plugins.** The `WasmExec` trait is an existing extension seam.
  More seams of the same shape could allow:
  - additional index types behind `CREATE INDEX`, such as a full-text index or a
    vector/embedding index for nearest-neighbor search;
  - pluggable storage backends behind the VLE, such as an object-store backend, or
    durable in-browser persistence (OPFS) for the WebAssembly build, which is
    in-memory only today;
  - import and export adapters for CSV, Parquet, JSON, and SQLite;
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
- **A durable in-browser backend (OPFS)** so the WebAssembly build persists instead
  of being in-memory only. A 2.0 item only if it changes the open/init API.
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
