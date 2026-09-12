# PicoVolt (PVDB)

[![CI](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml/badge.svg)](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/picovolt.svg)](https://crates.io/crates/picovolt)
[![License: Proprietary](https://img.shields.io/badge/license-Proprietary-blue.svg)](LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/MiniJe/picovolt?style=social)](https://github.com/MiniJe/picovolt)

PicoVolt is an embedded SQL database with queryable history. **2.2.0** adds
native encrypted snapshots and vaults, key/password rotation, verified encrypted
backup/restore and hybrid full-text/vector retrieval. It builds on the snapshot
readers, bounded writer scheduling and durable commit logs introduced in 2.0.

The source and official packages are available without a license fee under the
[PicoVolt Public-Source License 1.1](LICENSE). This proprietary license permits
commercial application embedding and unchanged registry/mirror distribution;
standalone competing database products require a separate agreement. This is
source-available, not open source. Earlier Apache grants remain unchanged.
No account, activation, telemetry or pre-download acceptance is required.

Read [encrypted storage](docs/ENCRYPTION_2_2.md), [hybrid search](docs/HYBRID_2_2.md)
and [release evidence](docs/RELEASE_2_2.md). Native vault encryption is not offered
in WASM. Independent security/cryptographic review and external application
trials have not been completed; automated verification is not such an audit.

Start with the [2.0 guide for every maintained interface](docs/QUICKSTART_2_0.md)
for atomic batches, persistence choices, log diagnostics and error recovery.
The [standalone review prompt](docs/INDEPENDENT_REVIEW_PROMPT.md) defines an
independent assessment and external trials deferred beyond the 2.0 release.

## Quick start

The 2.2.0 release is free to download and use within its [license](LICENSE).
Historical 2.0 documentation below remains useful for unchanged APIs. See the
[transition policy](legal/TRANSITION.md) for continuing Apache rights and support.

```sh
cargo add picovolt@2.2.0
```

```rust
use picovolt::Database;

fn main() -> Result<(), picovolt::PvError> {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE notes (id, body)")?;
    db.query("INSERT INTO notes VALUES (1, 'Hello, PicoVolt')")?;
    println!("{:?}", db.query("SELECT * FROM notes")?);
    Ok(())
}
```

For other languages, see [Install and distribution](#install-and-distribution).

## Storage model

The engine decouples query logic from storage representation through a
Virtualization Layer Engine (VLE) that shifts between two on-disk shapes:

- **Development mode:** a `.pv/` workspace of mutable, append-only chunk files
  plus a content-addressed blob store and inspectable manifest.
- **Production mode:** a single contiguous, memory-mappable `.pvdb` file produced
  by `pv_bake()`.

New records use a slotted row layout for O(1) appends. Applications can run
bounded maintenance steps that transpose immutable non-tail pages into an
MVCC-preserving columnar layout with packed decimal encoding.

## Status

The engine is exercised by Rust unit and integration suites plus doctests
and maintained-binding integration tests. CI also enforces formatting and
warning-free Clippy builds on Linux and Windows. Shipped changes are tracked in
[CHANGELOG.md](CHANGELOG.md), and future work is tracked in
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
| [`upgrade.rs`](src/upgrade.rs) | out-of-place format migration with backup and deep verification |
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
- **Page-backed engine.** Tables are append-only chains of hot row pages and
  optional packed cold pages, each header linking to the next. Inserts append to
  a row tail. Commit cost also includes catalog/index maintenance, retained-log
  accounting and the selected durability protocol; it is not uniformly O(1).
  Logged 2.0 workspaces persist index definitions to reduce catalog rewrites.
  Reads stream through a
  bounded buffer pool ([`storage/cache.rs`](src/storage/cache.rs)), so datasets
  need not fit in RAM, and opt-in ordered indexes
  ([`storage/index.rs`](src/storage/index.rs)) turn `WHERE col = value` into a
  point lookup and range comparisons such as
  `WHERE col > v` into an ordered scan rather than a full scan.
- **Selectable durability.** `Database::set_durability(Durability::Sync)` makes
  each flush `fsync` the data and commit the manifest atomically (write to a temp
  file, `fsync`, then rename). The default `Fast` mode uses the OS cache only:
  fast and durable on a clean exit, but not power-loss-safe.
- **Concurrent transactions.** Native `SharedDatabase` exposes independent
  snapshot readers and bounded FIFO writers. Logged workspaces sync original
  pages before overwriting them and publish an ordered physical change stream.
  Reopening rolls back incomplete writes. Format 6 prevents old binaries from
  bypassing recovery. Existing 1.x images remain readable and migratable.
  See [the concurrency contract](docs/CONCURRENCY.md) for limits and costs.
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
`pv inspect`, `pv history`, `pv diff`, `pv migrate`, `pv compact`, `pv import`,
`pv export`, and `pv bake`.
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

Native Rust applications can use the 2.0 concurrency surface through
`SharedDatabase`. It is a cloneable, bounded worker-thread coordinator with
explicit read and write transaction handles, FIFO admission, cooperative
cancellation, and rollback before failed or abandoned writes release the queue.
Explicit readers execute against independent snapshots while one writer updates
the workspace. Logged workspaces use format 7 with commit-sequence anchoring;
use clones of one coordinator for a shared development workspace.
See [Shared database concurrency](docs/CONCURRENCY.md) for the contract and
current limits.

Measured results and the methodology are in [BENCHMARKS.md](BENCHMARKS.md).
These measurements describe specific workloads and versions, rather than a
general throughput guarantee. PicoVolt supports bounded page-cache reads,
ordered secondary indexes, MVCC time travel and single-file publication.
Shared filesystem transactions use an incremental page journal; the legacy
unlogged surface retains full rollback images. Current limits include snapshot
copy costs, a focused SQL planner and a single writer. The [2.0 benchmark
baseline](benchmarks/COMPETITORS_2_0_RC3.md) records candidate measurements and
their limits; see [the concurrency contract](docs/CONCURRENCY.md) for current
resource costs.

## Maintenance and migration

Cold-page maintenance is explicit so the host retains ownership of
scheduling and threads:

```sh
pv compact ./data.pv --max-pages 64
pv inspect ./data.pv --json
```

`Database::compact_step(max_pages)` preserves record addresses, indexes, and
complete MVCC history; it never compacts the mutable tail and leaves a page in
row form when transposition would not save space. Each pass uses the
crash-recoverable transaction protocol: allow bounded journal space for logged
workspaces, or a full rollback image for unlogged workspaces. Baked-image migration is
out-of-place and deeply verified before publication:

```sh
pv migrate old.pvdb new.pvdb --dry-run
pv migrate old.pvdb new.pvdb --backup old.exact-backup.pvdb
```

The source is never modified and existing destinations are never overwritten.
See [Migration and compaction](docs/MIGRATION.md).

## Install and distribution

| Target | How |
|--------|-----|
| **Rust** (crates.io) | `cargo add picovolt` |
| **JavaScript / npm** (WebAssembly, browser and Node) | `npm install picovolt` |
| **Python** (native wheels) | `python -m pip install picovolt` |
| **Go** (`database/sql` and direct API) | `go get github.com/MiniJe/picovolt/bindings/go/v2@v2.2.0`, then provide the matching native C ABI library described in [`bindings/go/`](bindings/go) |
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
| Monetization thesis | [docs/MONETIZATION.md](docs/MONETIZATION.md) |
| Enterprise integration foundation | [docs/ENTERPRISE.md](docs/ENTERPRISE.md) |
| Platform and file support | [docs/SUPPORT.md](docs/SUPPORT.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Code of conduct | [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) |
| Changelog | [CHANGELOG.md](CHANGELOG.md) |
| Security policy | [SECURITY.md](SECURITY.md) |

## License

The 2.1.0 line is prepared under the [PicoVolt Proprietary Lifetime License](LICENSE).
Earlier Apache-2.0 components retain their existing grants; the original license
is preserved [here](legal/APACHE-2.0-LEGACY.txt). Third-party licenses and notices
continue to apply (see [`NOTICE`](NOTICE)). No automatic public publication is enabled.

The optional [`compliance`](src/engine/compliance.rs) module is not a license
requirement. It is an opt-in helper for applications that want to enforce their
own usage policy. It does not implement license activation or telemetry.
