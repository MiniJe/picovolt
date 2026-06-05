# PicoVolt (PVDB)

<!-- Replace OWNER with your GitHub org/user after pushing. -->
[![CI](https://github.com/OWNER/picovolt/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/picovolt/actions/workflows/ci.yml)
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

Built out in four phases — **all four are implemented**, with 58 unit/integration
tests + doctests passing and a clean `cargo clippy -D warnings` on Linux and
Windows.

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
| [`storage/page.rs`](src/storage/page.rs) | slotted row page (O(1) append) + columnar transposition |
| [`storage/cas.rs`](src/storage/cas.rs) | BLAKE3 content-addressable dedup (memory / dev-files / mmap) |
| [`storage/compress.rs`](src/storage/compress.rs) | Delta-Z, LEB128 varints, dictionary bit-packing |
| [`storage/record.rs`](src/storage/record.rs) | row ⇄ record-body serialization with CAS interception |
| [`storage/vle.rs`](src/storage/vle.rs) | dev directory store, prod mmap monolith, `bake` |
| [`engine/mvcc.rs`](src/engine/mvcc.rs) | transaction clock + snapshot visibility |
| [`engine/wasm.rs`](src/engine/wasm.rs) | sandboxed `wasmi` extension runtime + the `WasmExec` backend trait |
| [`engine/interp.rs`](src/engine/interp.rs) | `pv-wasm`: a from-scratch WASM interpreter (integer subset) |
| [`engine/query.rs`](src/engine/query.rs) | small SQL front-end (`SELECT … BEFORE tx`, etc.) |
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
- **Columnar `u48` reserved field**: not a native Rust type; the 24-byte header
  reserves the full 13 trailing bytes (the spec's 8+1+2+6 only sums to 17).
- **Persistence**: development mode re-serializes tables to pages on each mutation
  (`autocommit`) for simplicity — a documented write-amplification trade-off, not
  the incremental persistence a production engine would use. The cold-columnar
  conversion is implemented and tested but invoked on demand rather than by a
  background timer.

## Build

```sh
cargo build
cargo test
```

## Examples & benchmarks

```sh
cargo run --release --example notes    # a small notes app: CRUD, edit history,
                                        # time-travel, CAS dedup, publish (bake)
cargo run --release --example bench     # evaluation harness across modes/workloads
```

Measured results and an honest strengths/limitations writeup live in
[BENCHMARKS.md](BENCHMARKS.md). Short version: the in-memory engine and the
*compile-and-publish* path (CAS dedup, columnar compression, single-file mmap
artifacts, time-travel) are fast and effective for datasets that fit in RAM;
per-insert autocommit is quadratic (batch + flush instead), and there are no
secondary indexes or larger-than-RAM support yet.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Third-party
dependencies are MIT/Apache-2.0; their notices apply to redistributions (see
[`NOTICE`](NOTICE)).

The optional [`compliance`](src/engine/compliance.rs) module is **not** a license
requirement — it's an opt-in helper for applications that want to enforce their
*own* usage policy. Apache-2.0 places no usage restrictions on PicoVolt itself.
