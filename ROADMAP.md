# Roadmap

This describes the direction of the project, not a commitment. Priorities shift
with what users need, and dates are deliberately omitted. Items are grouped by
horizon. Changes that have landed are recorded in [CHANGELOG.md](CHANGELOG.md).

PicoVolt is experimental and pre-1.0, so the public API and on-disk format may
still change between minor versions.

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

## Next

- **Decimals in the columnar layout:** decimal values became storable in row form,
  and as SQL literals such as `12.50`, in 0.5.0. Extending the packed columnar
  layout to encode them (today such pages stay in row form) is the remaining piece.
- **Persisted indexes:** indexes are currently rebuilt by a scan on open.
  Persisting them in the `.pvdb` file and workspace would let large tables open
  quickly.
- **Background columnar compaction:** promote the on-demand row-to-columnar
  transposition ([`storage/page.rs`](src/storage/page.rs)) to a background worker.

## Bindings and extensions

The C ABI opens two directions that grow independently of the core engine.

- **More bindings.** A Go `database/sql` driver and pip-installable Python wheels
  that bundle the shared library both shipped in 0.5.0. Still open: Go ORM
  adapters and a documented C example. Because the bindings share one C ABI, new
  languages (Ruby, C#, Java, Zig) are wrappers rather than new engine work.
- **Functional plugins.** The `WasmExec` trait is an existing extension seam.
  More seams of the same shape could allow:
  - additional index types behind `CREATE INDEX`, such as a full-text index or a
    vector/embedding index for nearest-neighbor search;
  - pluggable storage backends behind the VLE, such as an object-store backend, or
    durable in-browser persistence (OPFS) for the WebAssembly build, which is
    in-memory only today;
  - import and export adapters for CSV, Parquet, JSON, and SQLite;
  - alternative compression codecs.

## Larger directions

Bigger, still-exploratory pieces, listed so the direction is visible.

- **A server mode.** An optional process that speaks a network protocol (HTTP or
  gRPC) so clients in any language connect over a socket, without `cgo`. This is
  also the natural path to serving more than one client.
- **Write concurrency.** The engine is single-writer today; concurrent writers
  are the prerequisite for multi-client use.
- **Encryption at rest** and **replication** for confidentiality and for keeping a
  warm copy of the data.
- **A `pv` command-line tool.** Promote the `repl` example into a real binary for
  import and export, inspection, and time-travel diffs.
- **Local-first sync.** Operation-log or CRDT sync between an in-browser PicoVolt
  and a server.

## Toward 1.0

Version 1.0 is the point at which the public API and the `.pvdb` on-disk format
are considered stable. Before that:

- **Freeze the on-disk format** behind a versioned header with a documented
  migration path, so future readers can open files written by older versions.
- **Stabilize the extension contract,** the crate-root re-exports documented in
  [docs/EXTENDING.md](docs/EXTENDING.md).
- **Specify durability precisely:** document exactly what `Fast` and `Sync`
  guarantee, with crash-injection tests behind the claims.
- **Longer fuzzing and external review:** the decoders are fuzzed in CI. Version
  1.0 calls for sustained fuzzing and an independent security pass (see
  [SECURITY.md](SECURITY.md)).

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
