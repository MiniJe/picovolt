# Changelog

All notable changes to PicoVolt are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org). See
[RELEASING.md](RELEASING.md) for how versions are chosen and cut.

## [Unreleased]

### Added
- **Ordered secondary indexes.** `CREATE INDEX` now builds a range-capable
  ordered index (a `BTreeMap` keyed on `Value`'s total order). Range predicates
  (`col < v`, `<=`, `>`, `>=`) use it for an ordered scan instead of a full table
  scan, in addition to the existing `col = v` point lookups — directly or as an
  `AND` conjunct.
- `Value` now implements `Eq` / `Ord` / `PartialOrd` (total order
  `Null < Int < Text < Blob`).
- **Richer `WHERE` predicates.** Comparison operators (`<`, `<=`, `>`, `>=`,
  `!=` / `<>`), `AND` / `OR` with parentheses, and `LIKE` (`%` / `_` wildcards),
  for `SELECT`, `UPDATE`, and `DELETE`. The equality index is still used when the
  predicate carries a simple `indexed_col = value` (including as an `AND`
  conjunct); everything else is a filtered scan.
- **Whole-table aggregates** in `SELECT`: `COUNT(*)`, `COUNT(col)`, `SUM`, `MIN`,
  `MAX` — over the full or `WHERE`-filtered result.
- `Database::select_filtered`, `update_where`, and `delete_where` — the
  predicate-based programmatic counterparts to the equality `select_where` /
  `update` / `delete` (which now delegate to them).
- `Database::run_wasm_apply` — byte-stream counterpart to `run_wasm_scalar`, for
  WASM extensions that transform their input region in place. Rounds out the
  documented extension seam (see [docs/EXTENDING.md](docs/EXTENDING.md)).

## [0.1.0] - 2026-06-20

First tagged release. PicoVolt is an **experimental, source-available**
(Apache-2.0) embedded data engine; it has not been audited or
production-hardened. The whole engine described below is implemented, tested
(80 unit/integration tests + doctests, `cargo clippy -D warnings` clean on Linux
and Windows), fuzzed, and runs both natively and in the browser via WebAssembly.

### Added
- **Polymorphic storage (VLE).** Three interchangeable backends behind one
  `Database` surface: a `Dev` directory workspace (mutable append-only chunks +
  content-addressed blobs), a read-only `Prod` single-file mmap monolith
  (`.pvdb`, produced by `bake`), and an in-memory backend for native + wasm use.
- **Page-backed engine.** Tables are append-only chains of 4 KiB row pages;
  inserts append to a tail page and write only that page plus an O(tables)
  manifest, so autocommit is O(1) per insert.
- **Bounded buffer pool.** A write-back LRU page cache serves datasets larger
  than RAM ([`storage/cache.rs`](src/storage/cache.rs)).
- **MVCC time-travel.** Snapshot-isolated reads and `SELECT … BEFORE <tx>` to
  query a table as of any past transaction; deletes/updates retain history.
- **Content-addressed dedup (CAS).** BLAKE3-interned payloads over 16 bytes are
  stored once and shared.
- **Columnar compression.** On-demand transposition of cold pages into a packed
  columnar layout (Delta-Z, LEB128 varints, dictionary bit-packing).
- **Secondary indexes.** Opt-in in-memory equality indexes turn `WHERE col = v`
  into a lookup instead of a scan.
- **Selectable durability.** `Durability::Fast` (OS-cache default) or
  `Durability::Sync` (per-flush `fsync` + atomic write-temp→fsync→rename manifest
  commit).
- **WASM extension runtime.** A sandboxed `wasmi` backend plus `pv-wasm`, a
  from-scratch integer-subset WASM interpreter, kept honest by a differential
  test against `wasmi`.
- **SQL front-end.** Hand-written tokenizer + recursive-descent parser for
  `CREATE TABLE`, `CREATE INDEX`, `INSERT`, `UPDATE … SET … WHERE`,
  `DELETE … WHERE`, `DROP TABLE`, and
  `SELECT {* | col, … | COUNT(*)} FROM t [WHERE col = v] [BEFORE tx]
  [ORDER BY col [ASC|DESC]] [LIMIT n]`. String literals support `''` escaping.
  `ORDER BY` uses a total ordering across value types
  (`NULL` < `Int` < `Text` < `Blob`).
- **Query result accessors.** `QueryResult::rows()` and `QueryResult::columns()`.
- **JavaScript / npm bindings.** `wasm-bindgen` `Db` class (`query`, `export`,
  `fromBytes`, `currentTx`) running the in-memory engine in the browser or Node.
- **`.pvdb` round-trip.** `Database::bake_to_bytes` / `Database::import_bytes`
  (and the JS `export` / `fromBytes`) serialize a full database — history and
  writability intact — to and from a portable byte image.
- **Live site.** A picovolt.dev landing page with an in-browser SQL playground
  ([`site/index.html`](site/index.html)) and a task-board app with a
  history-replay time slider ([`site/app.html`](site/app.html)).

### Security
- Untrusted-input hardening across the `.pvdb`/workspace and WASM decoders:
  validated manifest hashes (no path traversal), bounds-checked CAS offsets and
  page-chain links (no OOB / infinite loops on crafted files), and capped WASM
  resource counts. Decoders are fuzzed (a cross-platform fuzz-lite test plus a
  `cargo-fuzz` crate); `cargo audit` reports no advisories. Both run in CI.

[Unreleased]: https://github.com/MiniJe/picovolt/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/MiniJe/picovolt/releases/tag/v0.1.0
