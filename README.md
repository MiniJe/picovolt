# PicoVolt (PVDB)

[![CI](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml/badge.svg)](https://github.com/MiniJe/picovolt/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)](CHANGELOG.md)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Status: experimental](https://img.shields.io/badge/status-experimental-orange.svg)

> ⚠️ **Experimental / educational project.** PicoVolt is a from-scratch
> exploration of embedded-database internals. It is not production-hardened, has
> not been audited, and the marketing-style claims in its design spec are
> aspirational. Use it to learn from, not to store data you can't lose.

A polymorphic embedded data engine. PicoVolt decouples query logic from storage
representation through a **Virtualization Layer Engine (VLE)** that shifts
between two on-disk shapes:

- **Development Mode** — a `.pv/` workspace of mutable, append-only chunk files
  plus a content-addressed blob store, friendly to git and code review.
- **Production Mode** — a single, contiguous, memory-mappable `.pvdb` monolith
  produced by `pv_bake()`.

Pages are *chameleon*: hot data lands in a slotted **row** layout for O(1)
appends, then a background worker transposes idle pages into a packed
**columnar** layout for compression and cache efficiency.

## Status

Built out in four phases — **all four are implemented**, with 80 unit/integration
tests + doctests passing and a clean `cargo clippy -D warnings` on Linux and
Windows. Versioning and the release process are documented in
[RELEASING.md](RELEASING.md); changes are tracked in [CHANGELOG.md](CHANGELOG.md).

| Phase | Scope                                                     | Status |
|-------|-----------------------------------------------------------|--------|
| 1     | Core memory layouts & error taxonomy                      | ✅     |
| 2     | Page engine, CAS dedup, compression, VLE router           | ✅     |
| 3     | MVCC / snapshot isolation, WASM runtime                   | ✅     |
| 4     | Public surface (`pv_open_dev`/`pv_open_prod`/`query`/`pv_bake`) + license hook | ✅ |

### Module map

| Module | Responsibility |
|--------|----------------|
| [`core/types.rs`](src/core/types.rs) | constants, ids, `PageType`, `RecordEnvelope`, page & file headers (explicit little-endian codecs) |
| [`core/errors.rs`](src/core/errors.rs) | unified `PvError` + `ComplianceError` |
| [`core/value.rs`](src/core/value.rs) | dynamically-typed `Value` / `Row` |
| [`storage/page.rs`](src/storage/page.rs) | slotted row page (O(1) append) + chain links + columnar transposition |
| [`storage/cache.rs`](src/storage/cache.rs) | bounded LRU buffer pool (enables larger-than-RAM reads) |
| [`storage/cas.rs`](src/storage/cas.rs) | BLAKE3 content-addressable dedup (memory / dev-files / mmap) |
| [`storage/compress.rs`](src/storage/compress.rs) | Delta-Z, LEB128 varints, dictionary bit-packing |
| [`storage/index.rs`](src/storage/index.rs) | in-memory ordered secondary index (value → record addresses; point + range) |
| [`storage/record.rs`](src/storage/record.rs) | row ⇄ record-body serialization with CAS interception |
| [`storage/vle.rs`](src/storage/vle.rs) | dev directory store, prod mmap monolith, `bake` |
| [`engine/mvcc.rs`](src/engine/mvcc.rs) | transaction clock + snapshot visibility |
| [`engine/wasm.rs`](src/engine/wasm.rs) | sandboxed `wasmi` extension runtime + the `WasmExec` backend trait |
| [`engine/interp.rs`](src/engine/interp.rs) | `pv-wasm`: a from-scratch WASM interpreter (integer subset) |
| [`engine/query.rs`](src/engine/query.rs) | small SQL front-end (CREATE/INSERT/UPDATE/DELETE/DROP, `SELECT` with projection/aggregates, `WHERE` predicates `<op>`/`AND`/`OR`/`LIKE`, `BEFORE`, `ORDER BY`, `LIMIT`) |
| [`engine/compliance.rs`](src/engine/compliance.rs) | optional, app-driven usage-policy hook (not a license requirement) |
| [`db.rs`](src/db.rs) | `Database` surface tying it all together |

### Notable engineering decisions & deviations from the spec

- **Explicit little-endian codecs** for every on-disk structure instead of casting
  `#[repr(C)]` structs — the file format stays portable and matches the spec's
  byte offsets exactly.
- **WASM — two interchangeable backends.** The default is the `wasmi` interpreter
  (vetted, full WASM). Alongside it, `pv-wasm` ([`engine/interp.rs`](src/engine/interp.rs))
  is a from-scratch interpreter — a hand-written binary decoder + structured-control
  stack machine covering the `i32`/`i64` integer subset. Both implement the
  `WasmExec` trait, and a differential test checks `pv-wasm`'s output against
  `wasmi` to keep it honest. (Floats, tables, globals, imports, SIMD, and
  `br_table` are deliberately out of scope for `pv-wasm` and rejected rather than
  mis-run.)
- **Page-backed engine.** Tables are append-only chains of row pages (each header
  links to the next). Inserts append to a *tail page* and write only that page
  plus an O(tables) manifest, so autocommit is O(1)/insert (linear), not the old
  whole-table rewrite. Reads stream through a bounded buffer pool
  ([`storage/cache.rs`](src/storage/cache.rs)) so datasets need not fit in RAM, and
  opt-in ordered indexes ([`storage/index.rs`](src/storage/index.rs)) turn
  `WHERE col = value` into a point lookup and `WHERE col > v` (and other range
  comparisons) into an ordered scan instead of a full scan.
- **Durability is selectable.** `Database::set_durability(Durability::Sync)` makes
  each flush `fsync` the data and commit the manifest atomically (write-temp →
  `fsync` → rename); the default `Fast` mode is OS-cache only (fast, durable on
  clean exit, not power-loss-safe).
- **Hardened against untrusted input.** Opening a `.pvdb`/workspace or running a
  WASM module validates manifest hashes (no path traversal), CAS offsets and page
  chains (no OOB / infinite-loop on a crafted file), and caps WASM resource counts.
  The decoders are **fuzzed** (cross-platform fuzz-lite test + a [`fuzz/`](fuzz)
  cargo-fuzz crate) and `cargo audit` reports **no advisories** — both run in CI.
  See [SECURITY.md](SECURITY.md).
- **Columnar `u48` reserved field**: not a native Rust type; the 24-byte header
  reserves the full 13 trailing bytes (the spec's 8+1+2+6 only sums to 17). The
  cold-columnar conversion is implemented and tested but invoked on demand rather
  than by a background timer.

## Build

```sh
cargo build
cargo test
```

## Examples & benchmarks

```sh
cargo run --release --example notes    # a small notes app: CRUD, edit history,
                                        # time-travel, CAS dedup, publish (bake)
cargo run --release --example repl      # interactive SQL shell (pvsql)
cargo run --release --example bench     # evaluation harness across modes/workloads
```

SQL supported: `CREATE TABLE`, `CREATE INDEX ON t (col)`, `INSERT`, `UPDATE … SET …
WHERE`, `DELETE … WHERE`, `DROP TABLE`, and
`SELECT {* | col, … | COUNT/SUM/MIN/MAX(…)} FROM t [WHERE <pred>] [BEFORE tx]
[ORDER BY col [ASC|DESC]] [LIMIT n]`, where `<pred>` combines `col <op> value`
(`=`, `!=`, `<`, `<=`, `>`, `>=`, `LIKE`) with `AND` / `OR` and parentheses.
Durability is selectable via `Database::set_durability` (`Fast` OS-cache default,
or crash-safe `Sync` with fsync + atomic manifest).

Measured results and an honest writeup live in [BENCHMARKS.md](BENCHMARKS.md).
Short version: PicoVolt is a page-backed engine with O(1) durable appends
(autocommit ~33k rows/s, *linear*), larger-than-RAM reads via a bounded buffer
pool (a 667-page dataset serves from a 16-page pool), ordered secondary indexes
(`WHERE col = value` ~11,000× faster than a scan, plus range predicates), MVCC
time-travel, opt-in crash-safe durability (`Durability::Sync`), and a fast
compile-and-publish path (CAS dedup, columnar compression, single-file mmap
artifacts). Remaining limits: indexes are in-memory (rebuilt on open) and there's
no concurrency.

## Install & distribution

| Target | How |
|--------|-----|
| **Rust** (crates.io) | `cargo add picovolt` (once published) |
| **JavaScript / npm** (WebAssembly, browser + Node) | `wasm-pack build --target bundler --release -- --features wasm` (see [RELEASING.md](RELEASING.md)) |
| **In-memory** (native, no filesystem) | `Database::open_memory()`, export with `bake_to_bytes()` |

PicoVolt runs in the browser via an in-memory backend: build the wasm package
with the command above, then `import { Db } from "picovolt"` and run SQL with
`db.query(...)` — see [src/wasm_api.rs](src/wasm_api.rs) for the JS surface.

## Extending PicoVolt

Two extension paths — sandboxed WASM user-defined functions, and native modules
built on the public API — are documented in [docs/EXTENDING.md](docs/EXTENDING.md).
The same public surface is what the planned commercial `picovolt-pro` edition
will build on; the open core stays Apache-2.0. See [ROADMAP.md](ROADMAP.md).

## Project

| | |
|--|--|
| Where it's going | [ROADMAP.md](ROADMAP.md) |
| Contributing (DCO sign-off, the test gate) | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Code of conduct | [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) |
| Versioning & releases | [RELEASING.md](RELEASING.md) · [CHANGELOG.md](CHANGELOG.md) |
| Security policy & reporting | [SECURITY.md](SECURITY.md) |
| Extending the engine | [docs/EXTENDING.md](docs/EXTENDING.md) |

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Third-party
dependencies are MIT/Apache-2.0; their notices apply to redistributions (see
[`NOTICE`](NOTICE)).

The optional [`compliance`](src/engine/compliance.rs) module is **not** a license
requirement — it's an opt-in helper for applications that want to enforce their
*own* usage policy. Apache-2.0 places no usage restrictions on PicoVolt itself.
