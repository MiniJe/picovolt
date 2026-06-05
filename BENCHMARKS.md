# PicoVolt — Evaluation & Benchmarks

These numbers come from [`examples/bench.rs`](examples/bench.rs). Reproduce with:

```sh
cargo run --release --example bench
```

> **Caveats.** Wall-clock measurements on a single Windows host (release build).
> They are ballpark, not laboratory-grade — focus on *relative* behavior across
> modes. Numbers will differ on other machines (antivirus, disk, and OS file-I/O
> behavior all matter a lot for the persistence rows). Cross-platform *correctness*
> is covered by CI on Linux and Windows.

## Headline results

| Scenario | Result |
|---|---|
| In-memory append (no flush) | **~7.8M rows/s** |
| Batched durable write (insert + 1 flush) | **~1.1M rows/s** (flush of 20k rows ≈ 16 ms) |
| Autocommit (flush on every insert) | **~120 rows/s** — quadratic, avoid for bulk |
| Full scan (in-memory) | **~5.7M rows/s** |
| Time-travel scan (`BEFORE tx`) | **~10M rows/s** |
| CAS dedup (5k rows, 10 distinct 625 B bodies) | **12× smaller** on disk than naïve |
| Columnar transposition + compression | **10× smaller** than naïve row encoding |
| Bake (compile 20k-row monolith) | **~33 ms** |
| Open prod (mmap + decode, 20k rows) | **~4 ms** |

## The persistence optimization (measure → fix → re-measure)

The first benchmark run exposed a sharp bottleneck: the dev-mode flush opened the
chunk file once **per 4 KB page**. Batching to one open + one bulk write per chunk
([`DevStore::write_pages`](src/storage/vle.rs)) produced:

| Metric | Before | After | Speedup |
|---|---:|---:|---:|
| Flush 20k rows | 2,287 ms | **16 ms** | ~140× |
| Batched insert throughput | 8.7k rows/s | **1.09M rows/s** | ~125× |
| Bake (compile monolith) | 2,603 ms | **33 ms** | ~79× |
| Autocommit, 1k inserts | 59.8 s | 8.4 s | ~7× |

## What this says about PicoVolt

### Where it's strong
- **In-memory engine core is fast.** Appends, scans, and time-travel all run in
  the millions of rows/sec. The MVCC visibility filter is cheap.
- **CAS dedup and compression deliver.** Duplicate-heavy data shrinks ~12×; the
  columnar codecs (Delta-Z + dictionary bit-packing) shrink friendly data ~10×.
- **The "publish an immutable dataset" path is genuinely good.** Baking 20k rows
  takes ~33 ms and the result opens read-only via mmap in ~4 ms as a single file.
  This is the use case the dev→prod split is built for.

### Where it's weak (honest limitations)
- **Autocommit is O(n²).** Each insert re-serializes and rewrites the *whole*
  table. Batching the I/O helped the constant factor a lot, but the algorithm is
  still quadratic — fine for interactive edits, wrong for bulk loads. The real fix
  is incremental/append-only persistence (future work). **Batch and flush** for now.
- **The working set is fully in-memory in both modes.** `open_prod` mmaps the file
  but then decodes every page into RAM (the mmap is used for loading and for CAS
  blob resolution, not for paged querying). The dev/prod scan-parity row confirms
  this. ⇒ PicoVolt is not suited to datasets larger than RAM yet.
- **No secondary indexes.** Every query is a full scan. ~5–10M rows/s makes that
  fine for small/medium tables, but it's linear in table size.
- **Single-threaded.** MVCC envelopes exist, but concurrent writers/readers are
  untested and unsynchronized.
- **Not `wasm32` / `no_std`.** Both backends rely on `std::fs`, and prod on `mmap`,
  so the engine doesn't currently build for the browser/edge.

### Verdict
PicoVolt is a fast in-memory engine with an excellent *compile-and-publish*
story (CAS dedup, columnar compression, single-file mmap artifacts, time-travel)
for **small-to-medium datasets that fit in RAM**. It is **not** a durable OLTP
store or a larger-than-RAM engine today — and the benchmarks make exactly where
those lines are measurable.

## Try it

```sh
cargo run --release --example notes    # a small notes app on PicoVolt
cargo run --release --example bench     # this harness
```
