# Changelog

All notable changes to PicoVolt are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org).

## [Unreleased]

## [0.8.0] - 2026-06-23

Drop-in adapters, so code written for other embedded SQL databases can use
PicoVolt with little change.

### Added
- **better-sqlite3-style JavaScript API** ([`bindings/js/sqlite.js`](bindings/js/sqlite.js),
  published as the `picovolt/sqlite` subpath). `new Database()`,
  `prepare(sql).run/get/all/iterate(...params)`, and `exec(sql)`, returning rows
  as objects keyed by column name.
- **Python DB-API 2.0 (PEP 249)** ([`bindings/python/picovolt/dbapi2.py`](bindings/python/picovolt/dbapi2.py)).
  `connect()`, `cursor()`, `execute(sql, params)`, `executemany`,
  `fetchone`/`fetchmany`/`fetchall`, `description`, and `rowcount`; `paramstyle`
  is `qmark` and `commit`/`rollback` are no-ops (the engine autocommits). Go users
  already have the standard `database/sql` driver in `bindings/go/pvsql`.

All adapters share PicoVolt's limits: positional `?` parameters only, no SQL
transactions, no JOINs, and `CREATE TABLE` takes column names only.

## [0.7.0] - 2026-06-23

Parameterized queries now reach every binding, not just JavaScript.

### Added
- **Parameters in the C ABI, Go, and Python.** `pv_query_params(db, sql, params_json)`
  binds a JSON array of parameters through the same safe binder as
  `Database::query_with`. The Go `database/sql` driver accepts bound parameters
  (`db.Exec("INSERT INTO t VALUES (?, ?)", 1, "alice")`), and the Python binding
  takes `db.query(sql, params)`. (JavaScript gained this in 0.6.0.) Values that
  contain quotes or SQL syntax are escaped, never injected; `[]byte` parameters in
  the Go driver are rejected rather than silently coerced.

## [0.6.0] - 2026-06-23

Parameterized queries, so PicoVolt can be used the way other SQL databases are.

### Added
- **Parameterized queries.** `Database::query_with(sql, &[Value])` binds `?`
  placeholders, each rendered as a safely-escaped SQL literal, so values that
  contain quotes or SQL syntax cannot be injected and callers no longer build SQL
  strings by hand. Placeholders inside string literals are left untouched and the
  parameter count is checked. The JavaScript binding exposes this as an optional
  second argument, `db.query("SELECT * FROM t WHERE id = ?", [1])`, mapping JS
  null, boolean, string, and number to PicoVolt values.

## [0.5.0] - 2026-06-23

Storable decimals, plus two more ways to reach the engine from other ecosystems.

### Added
- **Storable decimal values.** `Value::Decimal` can now be inserted and persisted,
  not only produced by `AVG`. A decimal literal such as `12.50` parses to an exact
  fixed-point value (scale 6); extra fractional digits truncate to the scale. It
  round-trips through row storage and a baked `.pvdb` image.
- **Go `database/sql` driver** ([`bindings/go/pvsql`](bindings/go/pvsql)).
  Registers a `picovolt` driver so the engine works through Go's standard
  `database/sql` API: `sql.Open("picovolt", "memory")`, or `"dev:<path>"` /
  `"prod:<path>"`. Query parameters and transactions are not supported.
- **Python wheels.** The Python binding builds platform wheels that bundle the
  compiled C ABI library, so `pip install` works without a separate build step. A
  `python-wheels` CI workflow builds them for Linux, macOS, and Windows and
  publishes to PyPI when a token is configured.

### Changed
- The on-disk record format gained a field tag for decimals. Files that contain
  decimal values are therefore not readable by 0.4.0 and earlier.

## [0.4.0] - 2026-06-22

A C ABI and the first native-language bindings. PicoVolt can now be embedded from
C, Go, and Python, alongside the existing Rust and JavaScript/WebAssembly
surfaces. No engine behavior changed.

### Added
- **C ABI (`capi` feature).** A stable, panic-safe, C-callable surface
  ([`src/ffi.rs`](src/ffi.rs)) over the in-process engine, with a hand-written
  header ([`include/picovolt.h`](include/picovolt.h)). It exposes
  `pv_open_memory`/`pv_open_dev`/`pv_open_prod`, `pv_query` (returning the same
  JSON shape as the JavaScript binding), `pv_current_tx`, `pv_export`/`pv_import`,
  and `pv_version`/`pv_last_error` plus the matching free and close functions.
  Panics are caught at the boundary and reported through `pv_last_error` rather
  than unwinding into the caller. Build a shared library with
  `cargo build --release --features capi`.
- **Go binding** ([`bindings/go`](bindings/go)). A `cgo` wrapper over the C ABI
  with an idiomatic `DB` type (`OpenMemory`/`OpenDev`/`OpenProd`, `Query`,
  `CurrentTx`, `Export`/`Import`, `Close`) and a runnable example.
- **Python binding** ([`bindings/python`](bindings/python)). A pure-`ctypes`
  wrapper (no build step on the Python side) exposing a `Database` class that
  returns query results as parsed Python objects, with a runnable example.

### Changed
- Query-result JSON serialization is now shared internally
  ([`src/json.rs`](src/json.rs)) between the WebAssembly and C ABIs, so every
  binding returns byte-for-byte the same shape.

## [0.3.0] - 2026-06-20

More backward-compatible SQL features on top of 0.2.0: `AVG` with a new exact
decimal value type, streaming reads, and positioned parse errors.

### Added
- **Streaming reads.** `Database::for_each_row` visits a table's visible rows one
  at a time (with optional time-travel) instead of materializing the full result,
  so large tables can be processed or exported with bounded memory. A
  `Database::column_names` accessor returns the schema for interpreting the rows.
- **Positioned parse errors.** A SQL parse or tokenizer error now reports the line
  and column of the offending token and draws a caret under the source, instead of
  only describing the problem.
- **Fixed-point decimal values.** A new `Value::Decimal(i128)` variant holds an
  exact fixed-point number (mantissa over `10^6`). It is a real, totally-ordered
  value (comparable, orderable, and `BTreeMap`-key-safe) and renders as
  fixed-point text such as `"1.500000"`. It is currently produced only by `AVG`;
  it is not yet storable on disk or constructible from a SQL literal, and trying
  to store one is rejected with a clear error.
- **`AVG` aggregate.** Averages an integer column, on its own or under `GROUP BY`,
  returning an exact `Value::Decimal` computed in i128 arithmetic and rounded half
  away from zero (so large integers stay exact, unlike an f64 average). NULLs are
  ignored, and an empty or all-null group averages to NULL.

### Changed
- `SUM` of an empty or all-null group now returns `NULL` instead of `0`, matching
  `MIN`, `MAX`, `AVG`, and standard SQL. (`COUNT` still returns `0`.)

## [0.2.0] - 2026-06-20

A set of backward-compatible SQL and indexing features added on top of 0.1.0.

### Added
- **`GROUP BY`.** `SELECT cols, AGG(...) FROM t [WHERE] GROUP BY cols` groups rows
  by one or more columns and evaluates `COUNT`, `SUM`, `MIN`, and `MAX` per group.
  A bare column in the select list must be a grouping column.
- **Index-accelerated `ORDER BY`.** A `SELECT ... ORDER BY indexed_col` with no
  `WHERE` reads the ordered index in key order instead of collecting every row and
  sorting, and a `LIMIT` lets it stop early once enough visible rows are found.
  Falls back to a sort when the column is unindexed or a `WHERE` clause is present.
- **Ordered secondary indexes.** `CREATE INDEX` now builds a range-capable
  ordered index (a `BTreeMap` keyed on `Value`'s total order). Range predicates
  (`col < v`, `<=`, `>`, `>=`) use it for an ordered scan instead of a full table
  scan, in addition to the existing `col = v` point lookups, directly or as an
  `AND` conjunct.
- `Value` now implements `Eq`, `Ord`, and `PartialOrd` (total order
  `Null < Int < Text < Blob`).
- **Richer `WHERE` predicates.** Comparison operators (`<`, `<=`, `>`, `>=`,
  `!=`, `<>`), `AND` and `OR` with parentheses, and `LIKE` (`%` and `_`
  wildcards), for `SELECT`, `UPDATE`, and `DELETE`. The equality index is still
  used when the predicate carries a simple `indexed_col = value` (including as an
  `AND` conjunct); everything else is a filtered scan.
- **Whole-table aggregates** in `SELECT`: `COUNT(*)`, `COUNT(col)`, `SUM`, `MIN`,
  and `MAX`, over the full or `WHERE`-filtered result.
- `Database::select_filtered`, `update_where`, and `delete_where`: the
  predicate-based programmatic counterparts to the equality `select_where`,
  `update`, and `delete` (which now delegate to them).
- `Database::run_wasm_apply`: the byte-stream counterpart to `run_wasm_scalar`,
  for WASM extensions that transform their input region in place. Rounds out the
  documented extension seam (see [docs/EXTENDING.md](docs/EXTENDING.md)).

## [0.1.0] - 2026-06-20

First tagged release. PicoVolt is an experimental, Apache-2.0 licensed embedded
database engine. It has not been audited or production-hardened. The whole engine
described below is implemented, tested (80 unit and integration tests plus
doctests, `cargo clippy -D warnings` clean on Linux and Windows), fuzzed, and
runs both natively and in the browser through WebAssembly.

### Added
- **Polymorphic storage (VLE).** Three interchangeable backends behind one
  `Database` surface: a `Dev` directory workspace (mutable append-only chunks plus
  content-addressed blobs), a read-only `Prod` single-file mmap monolith (`.pvdb`,
  produced by `bake`), and an in-memory backend for native and wasm use.
- **Page-backed engine.** Tables are append-only chains of 4 KiB row pages.
  Inserts append to a tail page and write only that page plus an O(tables)
  manifest, so autocommit is O(1) per insert.
- **Bounded buffer pool.** A write-back LRU page cache serves datasets larger than
  RAM ([`storage/cache.rs`](src/storage/cache.rs)).
- **MVCC time-travel.** Snapshot-isolated reads and `SELECT ... BEFORE <tx>` query
  a table as of any past transaction; deletes and updates retain history.
- **Content-addressed dedup (CAS).** BLAKE3-interned payloads over 16 bytes are
  stored once and shared.
- **Columnar compression.** On-demand transposition of cold pages into a packed
  columnar layout (Delta-Z, LEB128 varints, dictionary bit-packing).
- **Secondary indexes.** Opt-in in-memory equality indexes turn `WHERE col = v`
  into a lookup instead of a scan.
- **Selectable durability.** `Durability::Fast` (OS-cache default) or
  `Durability::Sync` (per-flush `fsync` plus an atomic manifest commit: write to a
  temp file, `fsync`, then rename).
- **WASM extension runtime.** A sandboxed `wasmi` backend plus `pv-wasm`, a
  from-scratch integer-subset WASM interpreter, kept honest by a differential test
  against `wasmi`.
- **SQL front-end.** Hand-written tokenizer and recursive-descent parser for
  `CREATE TABLE`, `CREATE INDEX`, `INSERT`, `UPDATE ... SET ... WHERE`,
  `DELETE ... WHERE`, `DROP TABLE`, and
  `SELECT {* | col, ... | COUNT(*)} FROM t [WHERE col = v] [BEFORE tx]
  [ORDER BY col [ASC|DESC]] [LIMIT n]`. String literals support `''` escaping, and
  `ORDER BY` uses a total ordering across value types
  (`NULL` < `Int` < `Text` < `Blob`).
- **Query result accessors.** `QueryResult::rows()` and `QueryResult::columns()`.
- **JavaScript and npm bindings.** A `wasm-bindgen` `Db` class (`query`, `export`,
  `fromBytes`, `currentTx`) running the in-memory engine in the browser or Node.
- **`.pvdb` round-trip.** `Database::bake_to_bytes` and `Database::import_bytes`
  (and the JavaScript `export` and `fromBytes`) serialize a full database, with
  history and writability intact, to and from a portable byte image.

### Security
- Untrusted-input hardening across the `.pvdb`, workspace, and WASM decoders:
  validated manifest hashes (no path traversal), bounds-checked CAS offsets and
  page-chain links (no out-of-bounds reads or infinite loops on crafted files),
  and capped WASM resource counts. Decoders are fuzzed (a cross-platform fuzz-lite
  test plus a `cargo-fuzz` crate), and `cargo audit` reports no advisories. Both
  run in CI.

[Unreleased]: https://github.com/MiniJe/picovolt/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/MiniJe/picovolt/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/MiniJe/picovolt/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/MiniJe/picovolt/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/MiniJe/picovolt/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/MiniJe/picovolt/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/MiniJe/picovolt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/MiniJe/picovolt/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/MiniJe/picovolt/releases/tag/v0.1.0
