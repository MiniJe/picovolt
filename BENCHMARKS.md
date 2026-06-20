# PicoVolt — Evaluation & Benchmarks

From [`examples/bench.rs`](examples/bench.rs). Reproduce with:

```sh
cargo run --release --example bench
```

> **Caveats.** Wall-clock on a single Windows host (release build). Ballpark, not
> laboratory-grade — focus on *relative* behavior. The persistence rows are
> sensitive to filesystem/antivirus behavior. Cross-platform *correctness* is
> covered by CI on Linux and Windows.

## The three weaknesses — fixed

An earlier evaluation found three real limits, all rooted in one design: the
engine held every row in RAM and rewrote everything on write. The **page-backed
engine** (a buffer pool, append-only page chains, and secondary indexes) fixes
all three:

| Weakness | Before | After |
|---|---|---|
| **Autocommit was O(n²)** | 59.8 s for 1k inserts (quadratic); ~107 rows/s | **~33,000 rows/s, linear** (3.3× time for 4× rows) |
| **Whole dataset lived in RAM** | every open loaded all rows | **667-page (2.6 MiB) dataset served with a 16-page / 64 KiB buffer pool** |
| **Every read was a full scan** | `WHERE col = v` scanned all rows | **secondary index → ~11,000× faster** point lookups |

How: inserts append to a table's *tail page* and write only that page plus a tiny
manifest (the page chain is a linked list in the page headers, so the manifest is
O(tables) not O(pages)); write handles are cached so autocommit doesn't reopen
files per insert. Reads stream through a bounded [`PageCache`](src/storage/cache.rs);
`CREATE INDEX` builds an ordered index used by `WHERE col = value` and range
predicates (`col > v`, …).

## Headline results

| Scenario | Result |
|---|---|
| In-memory append (no flush) | ~1.9M rows/s |
| Batched durable write (insert + 1 flush) | ~1.4M rows/s (20k-row flush ≈ 2 ms) |
| Autocommit (durable per insert) | **~33k rows/s, linear** |
| Full scan (in-memory) | ~3.5M rows/s |
| Time-travel scan (`BEFORE tx`) | ~5.7M rows/s |
| Indexed `WHERE col = value` | **~11,000× faster than scan** |
| Larger-than-RAM scan (16-page pool, 667-page dataset) | 50k rows in ~24 ms, ≤16 pages resident |
| CAS dedup (5k rows, 10 distinct bodies) | **12× smaller** on disk |
| Columnar transposition + compression | **10× smaller** |
| Bake (compile 20k-row monolith) | ~15 ms |
| Open prod (mmap + decode) | ~5 ms |

## Honest trade-offs & remaining limits

Becoming a real storage engine cost some peak in-memory speed — a fair trade:

- **In-memory append 7.8M → 1.9M rows/s**, **scan 5.7M → 3.5M rows/s**: inserts now
  serialize into real pages and scans decode records from the buffer pool, rather
  than pushing to / cloning from an in-RAM `Vec`.
- **Durability is selectable.** The default `Fast` mode is OS-cache (a power-loss
  crash can lose recent writes); `Durability::Sync` `fsync`s data and commits the
  manifest atomically per flush (crash-safe, much slower). A full WAL is still
  future work.
- **Indexes are ordered but in-memory**, rebuilt by a streaming scan on open.
  They serve both point (`col = v`) and range (`col > v`) predicates; persisting
  them as on-disk B-trees is future work. (The ~11,000× figure above is for the
  point lookup; range scans weren't separately benchmarked.)
- **`SELECT *` still materializes the full result set** (by definition). The
  larger-than-RAM win is that the *engine* stays bounded; filtered/indexed queries
  return small results without holding the dataset resident.
- **Single-threaded; not `wasm32`/`no_std`** (`std::fs` + `mmap`).

## Verdict

PicoVolt is now a small but genuine page-backed engine: **O(1) durable appends,
larger-than-RAM reads via a buffer pool, indexed lookups, MVCC time-travel, and a
fast compile-and-publish path** (CAS dedup, columnar compression, single-file mmap
artifacts). Indexes are in-memory (rebuilt on open) and there's no concurrency —
but the benchmarks show exactly where those lines are.

## Try it

```sh
cargo run --release --example notes    # a small notes app on PicoVolt
cargo run --release --example bench     # this harness
```
