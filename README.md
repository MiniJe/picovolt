# PicoVolt (PVDB)

[![CI](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml/badge.svg)](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/picovolt.svg)](https://crates.io/crates/picovolt)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Status: stable 1.x](https://img.shields.io/badge/status-stable%201.x-brightgreen.svg)
[![GitHub stars](https://img.shields.io/github/stars/MiniJe/picovolt?style=social)](https://github.com/MiniJe/picovolt)

PicoVolt is an embedded database engine written in Rust. Its 1.x public API and
on-disk format are stable under Semantic Versioning. It is young software and
has not had an external security audit, so review it and keep backups before
trusting it with data you cannot regenerate.

If PicoVolt is useful to you, consider starring the repository on GitHub. It is
the simplest way to help others discover the project.

The engine decouples query logic from storage representation through a
Virtualization Layer Engine (VLE) that shifts between two on-disk shapes:

- **Development mode:** a `.pv/` workspace of mutable, append-only chunk files
  plus a content-addressed blob store and inspectable manifest.
- **Production mode:** a single contiguous, memory-mappable `.pvdb` file produced
  by `pv_bake()`.

New records use a slotted row layout for O(1) appends. Idle pages can be
transposed into a packed columnar layout for compression and cache efficiency.

## Status

The current stable release is exercised by a 240+ test Rust suite plus doctests
and maintained-binding integration tests. CI also enforces formatting and
warning-free Clippy builds on Linux and Windows. Shipped changes are tracked in
[CHANGELOG.md](CHANGELOG.md), and the remaining work toward 2.0 is tracked in
[ROADMAP.md](ROADMAP.md).

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
| [`storage/index.rs`](src/storage/index.rs) | ordered secondary-index query structure and its persisted value/address encoding (point and range) |
| [`storage/record.rs`](src/storage/record.rs) | row and record-body serialization with CAS interception |
| [`storage/vle.rs`](src/storage/vle.rs) | dev directory store, owned prod snapshot, streamed reads, `bake` |
| [`engine/mvcc.rs`](src/engine/mvcc.rs) | transaction clock and snapshot visibility |
| [`engine/wasm.rs`](src/engine/wasm.rs) | sandboxed `wasmi` extension runtime and the `WasmExec` backend trait |
| [`engine/interp.rs`](src/engine/interp.rs) | `pv-wasm`: a from-scratch WASM interpreter (integer subset) |
| [`engine/query.rs`](src/engine/query.rs) | SQL front-end (CREATE/INSERT/UPDATE/DELETE/DROP, `SELECT` with projection, `AS` aliases, `DISTINCT`, aggregates, `GROUP BY`/`HAVING`, `WHERE` predicates incl. `IN`/`BETWEEN`/`IS NULL`/`LIKE`, `BEFORE`, multi-column `ORDER BY`, `LIMIT`/`OFFSET`) |
| [`engine/compliance.rs`](src/engine/compliance.rs) | optional, app-driven usage-policy hook (not a license requirement) |
| [`enterprise.rs`](src/enterprise.rs) | optional, host-owned audit events and honest capability discovery for fleet integrations |
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
- **Crash-recoverable transactions.** Explicit `BEGIN`, `COMMIT`, and
  `ROLLBACK` group filesystem or in-memory writes. Filesystem transactions keep
  a synced rollback image and recovery marker; reopening after interruption
  restores the last committed state before loading the workspace.
- **Hardened against untrusted input.** Opening a `.pvdb` or workspace, or running
  a WASM module, validates manifest hashes (no path traversal), bounds-checks CAS
  offsets and page chains (no out-of-bounds reads or infinite loops on a crafted
  file), and meters WASM instructions, memory, and output. The decoders are fuzzed (a cross-platform
  fuzz-lite test and a [`fuzz/`](fuzz) cargo-fuzz crate), and `cargo audit`
  currently reports no vulnerability failures. Both run in CI. See
  [SECURITY.md](SECURITY.md).

## Build

Rust 1.86 or newer is required.

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

Install the full CLI with `cargo install picovolt --features data-tools`, then use `pv query`,
`pv inspect`, `pv history`, `pv diff`, `pv import`, `pv export`, and `pv bake`.
Parquet/SQLite conversion, query explanations, inspection, resumable baking,
and dataset signing are documented in [Data tools](docs/DATA_TOOLS.md).

Compare two MVCC snapshots with:

```sh
pv diff ./data users --from 42 --to 57
pv diff ./data users --from 42 --to 57 --format jsonl
```

CSV is the default and identifies each row with an `_change` column. JSONL uses
`{"change":"added|removed","row":{...}}`. The diff is deterministic and
duplicate-aware; an update is represented as a removed row followed by an added
row. Copyable Rust, Python, Go, Node, and browser projects are in
[`starters/`](starters/README.md); supported adapters are catalogued in
[`docs/INTEGRATIONS.md`](docs/INTEGRATIONS.md).

SQL supports the normal PicoVolt CRUD and schema statements plus projection,
filters, aggregates, grouping, time travel, ordering, and pagination. The
current query surface includes `AS`/bare table aliases, N-table equality
`INNER`/`LEFT` joins, searched `CASE WHEN`, and the focused `LOWER`, `UPPER`,
`TRIM`, `LENGTH`, `ABS`, `COALESCE`, and `NULLIF` scalar functions. Schema-light
types, literal defaults, named inserts, and persisted `CHECK` constraints cover
common adapter DDL. See the precise syntax, examples, type behavior, and
deliberate limits in
[`docs/SQL.md`](docs/SQL.md). Rust callers can cache `Database::prepare(...)`
templates; C, WebAssembly, JavaScript, Python, and Go expose the same reusable
prepared-statement lifecycle. Callers can use explicit transactions or atomic
`Database::transaction(...)` closures with both filesystem and in-memory
databases.
Durability is selectable via `Database::set_durability` (`Fast` OS-cache default,
or crash-safe `Sync` with fsync and an atomic manifest).

Measured results and the methodology are in [BENCHMARKS.md](BENCHMARKS.md). In
short, PicoVolt is a page-backed engine with O(1) filesystem appends (autocommit
around 33k rows/s, linear), larger-than-RAM reads through a bounded buffer pool (a
667-page dataset serves from a 16-page pool), ordered secondary indexes (point
lookups roughly 6,100 times faster than a scan, plus range predicates), MVCC
time-travel, opt-in crash-safe durability (`Durability::Sync`), and a fast
compile-and-publish path (CAS dedup, columnar compression, memory-mappable
single-file artifacts). Current limits include full-workspace transaction
backups rather than an incremental WAL, left-deep equality joins rather than a
general SQL planner, and no concurrent writers.

## Install and distribution

| Target | How |
|--------|-----|
| **Rust** (crates.io) | `cargo add picovolt` |
| **JavaScript / npm** (WebAssembly, browser and Node) | `npm install picovolt` |
| **Python** (native wheels) | `python -m pip install picovolt` |
| **Go** (`database/sql` and direct API) | `go get github.com/MiniJe/picovolt/bindings/go@latest`, then provide the matching native C ABI library described in [`bindings/go/`](bindings/go) |
| **C** | Download the matching `picovolt-capi-*` bundle from the [latest release](https://github.com/MiniJe/picovolt/releases/latest), or run `cargo build --release --features capi` |
| **In-memory** (native, no filesystem) | `Database::open_memory()`, export with `bake_to_bytes()` |

PicoVolt runs in the browser through its in-memory backend plus an OPFS persistence
wrapper and Web Worker endpoint. Build the WebAssembly
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
a familiar surface, PicoVolt provides a `better-sqlite3`-inspired JavaScript API
(`import Database from "picovolt/sqlite"`), a Python DB-API 2.0 module
(`import picovolt.dbapi2 as sqlite`), and a Go `database/sql` driver
([`bindings/go/pvsql`](bindings/go/pvsql)). These are interface adapters, not
drop-in compatibility layers: shared limits include positional `?` only and the
intentionally compact SQL grammar. JavaScript and in-memory Rust also expose
rollback-capable transaction wrappers. Native bindings expose the same
transaction lifecycle through the C ABI.

## Server mode

An optional HTTP and JSON server reaches the engine over a socket. One dedicated
thread owns the database and runs statements serially, while a pool of HTTP
worker threads accepts concurrent connections and hands each request to that
thread over a channel, so the single-threaded core is unchanged.

```sh
cargo build --release --features server
./target/release/picovolt-server --memory --addr 127.0.0.1:8080
curl -s localhost:8080/v1/query \
  -H 'Content-Type: application/json' \
  -d '{"sql":"CREATE TABLE demo (value)","params":[]}'
```

Endpoints are `POST /v1/query`, `GET /v1/tx`, and `GET /v1/health`. Loopback use
may omit authentication. A non-loopback bind is refused unless a bearer token is
provided with `--token-file` or `PICOVOLT_SERVER_TOKEN`; send it as
`Authorization: Bearer ...`. Query bodies, queues, execution time, rows scanned,
result rows, and response size are bounded. TLS is not built in, so network
deployments still belong behind a TLS-terminating reverse proxy. See
[src/bin/server.rs](src/bin/server.rs).

The HTTP API is sessionless, so each request is an atomic statement and explicit
transaction-control statements are rejected. Applications needing a
multi-statement transaction should use an embedded language binding, where the
transaction belongs to one database handle.

Applications accepting SQL from users can also call `Database::query_with_limits`
directly and choose their own scan, result, memory, and deadline budgets.

## Extending PicoVolt

There are two extension paths: sandboxed WebAssembly user-defined functions, and
native modules built on the public API. Both are documented in
[docs/EXTENDING.md](docs/EXTENDING.md).

## Project

| | |
|--|--|
| Roadmap | [ROADMAP.md](ROADMAP.md) |
| One-million-download plan | [docs/ROADMAP_1M_DOWNLOADS.md](docs/ROADMAP_1M_DOWNLOADS.md) |
| Monetization thesis | [docs/MONETIZATION.md](docs/MONETIZATION.md) |
| Enterprise integration foundation | [docs/ENTERPRISE.md](docs/ENTERPRISE.md) |
| Platform and file support | [docs/SUPPORT.md](docs/SUPPORT.md) |
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
