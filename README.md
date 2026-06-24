# PicoVolt (PVDB)

[![CI](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml/badge.svg)](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.10.1-blue.svg)](CHANGELOG.md)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Status: experimental](https://img.shields.io/badge/status-experimental-orange.svg)

PicoVolt is an embedded database engine written from scratch in Rust. It is
experimental software. It has not been audited or hardened for production, so use
it to learn from and prototype with rather than to store data you cannot lose.

The engine decouples query logic from storage representation through a
Virtualization Layer Engine (VLE) that shifts between two on-disk shapes:

- **Development mode:** a `.pv/` workspace of mutable, append-only chunk files
  plus a content-addressed blob store, friendly to git and code review.
- **Production mode:** a single contiguous, memory-mappable `.pvdb` file produced
  by `pv_bake()`.

Pages are chameleon. Hot data lands in a slotted row layout for O(1) appends, and
idle pages can be transposed into a packed columnar layout for compression and
cache efficiency.

## Status

The engine is built out across four phases, all implemented, with 103 unit and
integration tests plus doctests passing and a clean `cargo clippy -D warnings` on
Linux and Windows. Changes are tracked in [CHANGELOG.md](CHANGELOG.md).

| Phase | Scope | Status |
|-------|-------|--------|
| 1 | Core memory layouts and error taxonomy | Done |
| 2 | Page engine, CAS dedup, compression, VLE router | Done |
| 3 | MVCC and snapshot isolation, WASM runtime | Done |
| 4 | Public surface (`pv_open_dev` / `pv_open_prod` / `query` / `pv_bake`) | Done |

### Module map

| Module | Responsibility |
|--------|----------------|
| [`core/types.rs`](src/core/types.rs) | constants, ids, `PageType`, `RecordEnvelope`, page and file headers (explicit little-endian codecs) |
| [`core/errors.rs`](src/core/errors.rs) | unified `PvError` and `ComplianceError` |
| [`core/value.rs`](src/core/value.rs) | dynamically-typed `Value` and `Row` |
| [`storage/page.rs`](src/storage/page.rs) | slotted row page (O(1) append), chain links, columnar transposition |
| [`storage/cache.rs`](src/storage/cache.rs) | bounded LRU buffer pool (enables larger-than-RAM reads) |
| [`storage/cas.rs`](src/storage/cas.rs) | BLAKE3 content-addressable dedup (memory, dev-files, mmap) |
| [`storage/compress.rs`](src/storage/compress.rs) | Delta-Z, LEB128 varints, dictionary bit-packing |
| [`storage/index.rs`](src/storage/index.rs) | in-memory ordered secondary index (value to record addresses; point and range) |
| [`storage/record.rs`](src/storage/record.rs) | row and record-body serialization with CAS interception |
| [`storage/vle.rs`](src/storage/vle.rs) | dev directory store, prod mmap monolith, `bake` |
| [`engine/mvcc.rs`](src/engine/mvcc.rs) | transaction clock and snapshot visibility |
| [`engine/wasm.rs`](src/engine/wasm.rs) | sandboxed `wasmi` extension runtime and the `WasmExec` backend trait |
| [`engine/interp.rs`](src/engine/interp.rs) | `pv-wasm`: a from-scratch WASM interpreter (integer subset) |
| [`engine/query.rs`](src/engine/query.rs) | SQL front-end (CREATE/INSERT/UPDATE/DELETE/DROP, `SELECT` with projection and aggregates, `WHERE` predicates, `BEFORE`, `ORDER BY`, `LIMIT`) |
| [`engine/compliance.rs`](src/engine/compliance.rs) | optional, app-driven usage-policy hook (not a license requirement) |
| [`db.rs`](src/db.rs) | the `Database` surface that ties it together |
| [`ffi.rs`](src/ffi.rs) | C ABI (the `capi` feature): a panic-safe, C-callable surface wrapping the engine for Go, Python, and C bindings |

### Engineering notes

- **Explicit little-endian codecs** for every on-disk structure, instead of
  casting `#[repr(C)]` structs, so the file format stays portable and its byte
  offsets are exact.
- **Two interchangeable WASM backends.** The default is the `wasmi` interpreter
  (vetted, full WASM). Alongside it, `pv-wasm`
  ([`engine/interp.rs`](src/engine/interp.rs)) is a from-scratch interpreter: a
  hand-written binary decoder and structured-control stack machine covering the
  `i32` and `i64` integer subset. Both implement the `WasmExec` trait, and a
  differential test checks `pv-wasm` against `wasmi` to keep it honest. Floats,
  tables, globals, imports, SIMD, and `br_table` are out of scope for `pv-wasm`
  and are rejected rather than mis-run.
- **Page-backed engine.** Tables are append-only chains of row pages, each header
  linking to the next. Inserts append to a tail page and write only that page
  plus an O(tables) manifest, so autocommit is O(1) per insert rather than a
  whole-table rewrite. Reads stream through a bounded buffer pool
  ([`storage/cache.rs`](src/storage/cache.rs)), so datasets need not fit in RAM,
  and opt-in ordered indexes ([`storage/index.rs`](src/storage/index.rs)) turn
  `WHERE col = value` into a point lookup and range comparisons such as
  `WHERE col > v` into an ordered scan rather than a full scan.
- **Selectable durability.** `Database::set_durability(Durability::Sync)` makes
  each flush `fsync` the data and commit the manifest atomically (write to a temp
  file, `fsync`, then rename). The default `Fast` mode uses the OS cache only:
  fast and durable on a clean exit, but not power-loss-safe.
- **Hardened against untrusted input.** Opening a `.pvdb` or workspace, or running
  a WASM module, validates manifest hashes (no path traversal), bounds-checks CAS
  offsets and page chains (no out-of-bounds reads or infinite loops on a crafted
  file), and caps WASM resource counts. The decoders are fuzzed (a cross-platform
  fuzz-lite test and a [`fuzz/`](fuzz) cargo-fuzz crate), and `cargo audit`
  reports no advisories. Both run in CI. See [SECURITY.md](SECURITY.md).

## Build

```sh
cargo build
cargo test
```

## Examples and benchmarks

```sh
cargo run --release --example notes    # a small notes app: CRUD, edit history,
                                       # time-travel, CAS dedup, publish (bake)
cargo run --release --example repl     # interactive SQL shell (pvsql)
cargo run --release --example bench    # evaluation harness across modes and workloads
```

SQL supported: `CREATE TABLE`, `CREATE INDEX ON t (col)`, `INSERT`,
`UPDATE ... SET ... WHERE`, `DELETE ... WHERE`, `DROP TABLE`, and
`SELECT {* | col, ... | COUNT/SUM/MIN/MAX/AVG(...)} FROM t [WHERE <pred>]
[GROUP BY cols] [BEFORE tx] [ORDER BY col [ASC|DESC]] [LIMIT n]`, where `<pred>`
combines `col <op> value` (`=`, `!=`, `<`, `<=`, `>`, `>=`, `LIKE`) with `AND`,
`OR`, and parentheses.
Durability is selectable via `Database::set_durability` (`Fast` OS-cache default,
or crash-safe `Sync` with fsync and an atomic manifest).

Measured results and the methodology are in [BENCHMARKS.md](BENCHMARKS.md). In
short, PicoVolt is a page-backed engine with O(1) durable appends (autocommit
around 33k rows/s, linear), larger-than-RAM reads through a bounded buffer pool (a
667-page dataset serves from a 16-page pool), ordered secondary indexes (point
lookups roughly 11,000 times faster than a scan, plus range predicates), MVCC
time-travel, opt-in crash-safe durability (`Durability::Sync`), and a fast
compile-and-publish path (CAS dedup, columnar compression, single-file mmap
artifacts). Current limits: indexes are in-memory (rebuilt on open) and there is
no concurrency.

## Install and distribution

| Target | How |
|--------|-----|
| **Rust** (crates.io) | `cargo add picovolt` |
| **JavaScript / npm** (WebAssembly, browser and Node) | `npm install picovolt` |
| **C / Go / Python** (native, via the C ABI) | `cargo build --release --features capi`, then see [`bindings/`](bindings) |
| **In-memory** (native, no filesystem) | `Database::open_memory()`, export with `bake_to_bytes()` |

PicoVolt runs in the browser through its in-memory backend. Build the WebAssembly
package with `wasm-pack build --target bundler --release -- --features wasm`, then
`import { Db } from "picovolt"` and run SQL with `db.query(...)`. See
[src/wasm_api.rs](src/wasm_api.rs) for the JavaScript surface.

For native languages, the `capi` feature builds a shared library exposing a C ABI
([include/picovolt.h](include/picovolt.h), [src/ffi.rs](src/ffi.rs)). The
[`bindings/`](bindings) directory wraps it for **Go** (cgo) and **Python**
(ctypes); both return query results as the same JSON shape as the JavaScript
binding. The bindings suit embedded use, not a concurrent server's primary store.

All bindings accept positional `?` parameters
(`db.query("... WHERE id = ?", [1])`), bound as safely-escaped SQL literals. For
a familiar surface, drop-in adapters are provided: a `better-sqlite3`-style
JavaScript API (`import Database from "picovolt/sqlite"`), a Python DB-API 2.0
module (`import picovolt.dbapi2 as sqlite`), and the Go `database/sql` driver
([`bindings/go/pvsql`](bindings/go/pvsql)). Shared limits across all of them:
positional `?` only, no SQL transactions, no JOINs, and `CREATE TABLE` takes
column names only.

## Server mode

An optional HTTP and JSON server reaches the engine over a socket. One dedicated
thread owns the database and runs statements serially, while a pool of HTTP
worker threads accepts concurrent connections and hands each request to that
thread over a channel, so the single-threaded core is unchanged.

```sh
cargo build --release --features server
./target/release/picovolt-server --memory --addr 127.0.0.1:8080
curl -s localhost:8080/v1/query -d '{"sql":"SELECT 1 + 1","params":[]}'
```

Endpoints are `POST /v1/query`, `GET /v1/tx`, and `GET /v1/health`. There is no
authentication or TLS, so run it behind a reverse proxy. See
[src/bin/server.rs](src/bin/server.rs).

## Extending PicoVolt

There are two extension paths: sandboxed WebAssembly user-defined functions, and
native modules built on the public API. Both are documented in
[docs/EXTENDING.md](docs/EXTENDING.md).

## Project

| | |
|--|--|
| Roadmap | [ROADMAP.md](ROADMAP.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Code of conduct | [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) |
| Changelog | [CHANGELOG.md](CHANGELOG.md) |
| Security policy | [SECURITY.md](SECURITY.md) |

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Third-party
dependencies are under MIT or Apache-2.0 licenses, and their notices apply to
redistributions (see [`NOTICE`](NOTICE)).

The optional [`compliance`](src/engine/compliance.rs) module is not a license
requirement. It is an opt-in helper for applications that want to enforce their
own usage policy. Apache-2.0 places no usage restrictions on PicoVolt itself.
