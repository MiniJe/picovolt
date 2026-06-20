# Benchmarks

Produced by [`examples/bench.rs`](examples/bench.rs). Reproduce with:

```sh
cargo run --release --example bench
```

> Caveats: these are wall-clock numbers from a single Windows host (release
> build). They are ballpark figures rather than laboratory-grade, so focus on
> relative behavior. The persistence rows are sensitive to filesystem and
> antivirus behavior. Cross-platform correctness is covered by CI on Linux and
> Windows.

## Three weaknesses, fixed

An earlier evaluation found three real limits, all rooted in one design: the
engine held every row in RAM and rewrote everything on each write. The page-backed
engine (a buffer pool, append-only page chains, and secondary indexes) fixes all
three.

| Weakness | Before | After |
|---|---|---|
| Autocommit was O(n^2) | 59.8 s for 1k inserts (quadratic), about 107 rows/s | About 33,000 rows/s, linear (3.3x the time for 4x the rows) |
| Whole dataset lived in RAM | every open loaded all rows | A 667-page (2.6 MiB) dataset served from a 16-page (64 KiB) buffer pool |
| Every read was a full scan | `WHERE col = v` scanned all rows | A secondary index makes point lookups about 11,000 times faster |

Inserts append to a table's tail page and write only that page plus a small
manifest. The page chain is a linked list in the page headers, so the manifest is
O(tables) rather than O(pages), and write handles are cached so autocommit does not
reopen files per insert. Reads stream through a bounded
[`PageCache`](src/storage/cache.rs). `CREATE INDEX` builds an ordered index used
by `WHERE col = value` and range predicates such as `col > v`.

## Headline results

| Scenario | Result |
|---|---|
| In-memory append (no flush) | about 1.9M rows/s |
| Batched durable write (insert plus one flush) | about 1.4M rows/s (a 20k-row flush is about 2 ms) |
| Autocommit (durable per insert) | about 33k rows/s, linear |
| Full scan (in-memory) | about 3.5M rows/s |
| Time-travel scan (`BEFORE tx`) | about 5.7M rows/s |
| Indexed `WHERE col = value` | about 11,000 times faster than a scan |
| Larger-than-RAM scan (16-page pool, 667-page dataset) | 50k rows in about 24 ms, at most 16 pages resident |
| CAS dedup (5k rows, 10 distinct bodies) | 12x smaller on disk |
| Columnar transposition and compression | 10x smaller |
| Bake (compile a 20k-row monolith) | about 15 ms |
| Open production file (mmap and decode) | about 5 ms |

## Trade-offs and limits

Becoming a real storage engine cost some peak in-memory speed, which is a fair
trade:

- **In-memory throughput dropped** (append from about 7.8M to 1.9M rows/s, scan
  from about 5.7M to 3.5M rows/s): inserts now serialize into real pages and scans
  decode records from the buffer pool, rather than pushing to and cloning from an
  in-RAM `Vec`.
- **Durability is selectable.** The default `Fast` mode uses the OS cache, so a
  power-loss crash can lose recent writes. `Durability::Sync` calls `fsync` on the
  data and commits the manifest atomically per flush (crash-safe, much slower). A
  full write-ahead log is future work.
- **Indexes are ordered but in-memory,** rebuilt by a streaming scan on open. They
  serve both point (`col = v`) and range (`col > v`) predicates. Persisting them
  as on-disk B-trees is future work. The 11,000-times figure above is for the
  point lookup; range scans were not separately benchmarked.
- **`SELECT *` still materializes the full result set,** by definition. The
  larger-than-RAM benefit is that the engine stays bounded; filtered or indexed
  queries return small results without holding the dataset resident.
- **Single-threaded,** and not `wasm32` or `no_std` (it uses `std::fs` and mmap).

## Summary

PicoVolt is a small but genuine page-backed engine: O(1) durable appends,
larger-than-RAM reads through a buffer pool, indexed lookups, MVCC time-travel,
and a fast compile-and-publish path (CAS dedup, columnar compression, single-file
mmap artifacts). Indexes are in-memory (rebuilt on open) and there is no
concurrency. The benchmarks show where those lines are.

## Try it

```sh
cargo run --release --example notes    # a small notes app on PicoVolt
cargo run --release --example bench    # this harness
```
