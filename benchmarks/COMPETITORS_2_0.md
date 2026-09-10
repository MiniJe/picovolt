# PicoVolt 2.0 competitor comparison

This is the preserved **rc.1 baseline**. See the
[rc.2 performance report](COMPETITORS_2_0_RC2.md) for the optimized candidate,
current competitor versions, CPU/memory measurements and bulk-loading results.

Measured on 2026-09-08. **PicoVolt 2.0.0-rc.1 is not yet competitive with
SQLite for this durable embedded SQL workload.** Its indexed point lookups
are faster than DuckDB through these Python APIs, but DuckDB is substantially
faster on the aggregate and top-N queries. The results identify engineering
work; they do not justify an overall “faster database” claim.

Version correction (rc.2 review): the raw results and loaded runtime report
DuckDB 1.4.4. An earlier report label said 1.5.5 because newer metadata coexisted
with the old native module. Timings are unchanged; the label is corrected.

## Environment and method

- Windows 11 build 26200, NTFS on D:, Intel Xeon W-2125 at 4.00 GHz,
  4 physical / 8 logical cores, 34,072,264,704 bytes installed usable RAM.
- PicoVolt release build at source commit `878ea6548d5cd0bbd628e1d9b5479f1c28f0303c`;
  SQLite **3.49.1** from Python's standard library; DuckDB **1.4.4**.
- Compiler: Rust **1.95.0**, `x86_64-pc-windows-gnu`, LLVM 22.1.2.
  Measured `picovolt.dll` SHA-256:
  `b74824c55173d051b64f4317b5568e009dd073397e94f4a94e34cc4c9e2e41a6`.
  Hosted wheels use the release workflow's compiler/platform targets and were
  installation-tested separately; these timings belong to the local GNU build.
- One Python 3.12 process per engine/trial, five trials, serial execution,
  engine order rotated. No local compilation ran during measurements.
- 10,000 deterministic rows, 50 categories, integer IDs/amounts and repeated
  text payloads. Identical non-unique indexes on event ID, event category,
  and category ID. All engines receive the same parameterized row-at-a-time
  INSERTs and equivalent SQL. PicoVolt's index syntax omits the index name.
- Five warmups per read shape, then 200 point queries, 30 ranges, 20 aggregates,
  20 top-N queries and 20 joins per trial. Full results are materialized and
  checked against a Python reference outside timing.
- Writes: one 10,000-row transaction, one index-building transaction,
  60 individually committed inserts, then ten 100-row transactions.
  Rollback is checked. Final reopen must reproduce all 11,060 rows exactly;
  all 15 runs have the same SHA-256 over canonical result JSON.
- Numbers below are the **median of five per-trial median latencies**, in
  milliseconds. Raw observations, trial ranges and per-trial p95 values are
  retained in [the JSON results](competitors-v2-windows.json).

This measures public **Python API end-to-end latency**, including PicoVolt's
ctypes/JSON conversion, SQLite's standard-library adapter and DuckDB's native
extension. It does not isolate engine execution or measure Rust
`SharedDatabase` actor overhead. The separate [concurrency envelope](concurrency-v2-windows.json)
measures PicoVolt's native snapshot readers and writer.

## Durability and retention

PicoVolt uses a format-6 filesystem workspace, incremental undo/change log,
synced transaction commits, and its default 64 MiB transaction / 256 MiB
retention limits. The initializer creates a logged workspace before the
Python binding opens it. SQLite uses verified `journal_mode=WAL` and
`synchronous=FULL`. In this configuration SQLite synchronizes the WAL at each
commit; `NORMAL` would omit those per-commit syncs.
See [SQLite's synchronous documentation](https://sqlite.org/pragma.html#pragma_synchronous).
DuckDB uses a persistent database, its default WAL/checkpoint behavior, and
`threads=1`; its [transaction contract](https://duckdb.org/docs/current/sql/statements/transactions)
provides ACID transactions.

These settings request durable commits; this Windows run does **not** establish
equivalent power-loss guarantees. PicoVolt does not sync directories on
Windows, and its crash tests cover process exits. See
[the platform contract](../docs/CONCURRENCY.md). No durability mode was weakened
to obtain a faster number.

An initial 10,000-row run with **no pruning failed** on the 38th single-row
insert after setup: 42 successful total commits exhausted PicoVolt's default
256 MiB retained-log budget. The large persisted index manifest is included
in both undo metadata and each physical commit; JSON encoding adds overhead.
This is a real operational limit, not a discarded timing outlier.
The [preserved failure observation](competitors-v2-unpruned-limit.json) records
265,169,549 retained bytes and 10,037 committed rows after successful rollback
of the rejected insert, with no active recovery journal remaining.

The completed comparison explicitly acknowledges and prunes PicoVolt history
before the write phase and every ten subsequent commits. It uses the shipped
`pv log-prune` CLI because the Python API does not expose native pruning.
CLI startup, workspace open, enumeration and deletion are measured separately
and included in the sustained-write total. Single-commit rows exclude that
periodic maintenance. Pruning is appropriate here because this benchmark has
no consumer needing historical physical commits; a real consumer must first
acknowledge them. SQLite and DuckDB keep their normal checkpoint policies.

## Results at 10,000 initial rows

Lower is better. Bulk load uses identical row-at-a-time parameterized INSERTs
inside one transaction; it is **not maximum ingestion throughput**. DuckDB's
Appender/COPY/vectorized ingestion paths are not exercised. DuckDB documents
its [bulk-loading and workload tuning options](https://duckdb.org/docs/current/guides/performance/how_to_tune_workloads).

| Workload | PicoVolt | SQLite | DuckDB |
| --- | ---: | ---: | ---: |
| Load 10,000 rows, one transaction | 216.072 | 18.808 | 3003.336 |
| Build three indexes, one transaction | 94.318 | 5.514 | 13.900 |
| Indexed point, one row | 0.0139 | 0.0062 | 0.2783 |
| Indexed range, 100 rows | 2.933 | 0.0623 | 0.5116 |
| Grouped SUM, 50 groups | 6.287 | 2.100 | 0.5745 |
| Top 20 by amount and ID | 4.266 | 0.6233 | 0.4168 |
| Indexed join, 200 rows | 11.494 | 0.1691 | 0.8214 |
| One row, one synced commit | 132.106 | 0.8632 | 1.4372 |
| 100 rows, one synced commit | 131.103 | 1.8699 | 30.978 |
| Sustained write phase, including periodic maintenance | 10000.877 | 75.108 | 399.296 |
| Delete one row then roll back | 83.925 | 0.1168 | 0.6248 |
| Reopen with warm OS cache | 17.855 | 0.6262 | 14.032 |

The sustained phase inserts 1,060 rows in 70 commits: approximately
**106 rows/s PicoVolt, 14,113 rows/s SQLite, and 2,655 rows/s DuckDB**.
PicoVolt's individually committed inserts are about **153x slower than SQLite**
and **92x slower than DuckDB** here. Its point queries are about **2.2x slower
than SQLite** and **20x faster than DuckDB**. DuckDB is about **11x faster on
grouped SUM** and **10x faster on top-N** than PicoVolt.

| Per-trial p95, median across five trials | PicoVolt | SQLite | DuckDB |
| --- | ---: | ---: | ---: |
| Indexed point, ms | 0.0151 | 0.0106 | 0.4384 |
| Single-row commit, ms | 149.878 | 1.2717 | 1.9825 |

Closed database logical file sizes after maintenance: PicoVolt **2,660,779 B**
(including its 40-byte pruning checkpoint), SQLite **630,784 B**, DuckDB
**1,585,152 B**. These are logical file lengths, not allocated disk blocks or
peak storage/RSS. PicoVolt retains MVCC row history; the engines do not have
identical historical-retention features. The failed unpruned run's log grew
to roughly 256 MiB separately from its base files.

## Interpretation and next engineering priorities

A second complete five-trial run at **1,000 initial rows** is recorded in
[the scaling results](competitors-v2-windows-1k.json). With the same commit
counts and transaction sizes, single-row median latency grew as follows:

| Initial rows | PicoVolt commit, ms | SQLite commit, ms | DuckDB commit, ms |
| --- | ---: | ---: | ---: |
| 1,000 | 32.304 | 0.7884 | 1.2838 |
| 10,000 | 132.106 | 0.8632 | 1.4372 |

PicoVolt's point lookup stayed near 0.014 ms, while its 100-row range grew
from 0.351 to 2.933 ms. This suggests size-dependent work in both the write
and range paths; the benchmark does not by itself isolate a profiler-level
cause. Across both sizes, **30 independent engine runs** passed full result
and reopen verification.

1. Reduce full manifest/index serialization and physical-record amplification
   per transaction. Retention fills after a small number of indexed commits.
2. Reduce synchronous filesystem operations and retained-directory enumeration
   while preserving the recovery protocol. Batching already amortizes the
   cost substantially: 100 inserts cost about one single-row commit.
3. Investigate range and join plans/materialization. Point lookup speed does
   not carry over to these result shapes.
4. Expose log consumption/pruning in maintained language bindings so applications
   can manage retention without launching the CLI.

The run is a small local application benchmark, not TPC-H/TPC-C, a multi-client
throughput test, a cold-cache test, or a larger-than-RAM evaluation. It does not
measure competitors' best bulk paths, compressed monolith deployment, CAS
dedup against unique payloads, time travel, or encrypted replication. Windows
filesystem/antivirus behavior and Python client differences can affect the
absolute and relative timings. Repeat on deployment hardware before sizing.

## Reproduce

From the repository, use a release build and the maintained Python binding.
The helper expects `pv` alongside the release `examples` directory.

```powershell
cargo build --locked --release --features capi --example benchmark_workspace --bin pv --lib
python -m pip install --target target/benchmark-python duckdb==1.4.4
$env:PICOVOLT_LIB = "$PWD\target\release\picovolt.dll"
$env:PYTHONPATH = "$PWD\bindings\python;$PWD\target\benchmark-python"
python scripts/benchmark_competitors.py --initializer target/release/examples/benchmark_workspace.exe --rows 10000 --trials 5 --output benchmarks/competitors-v2-windows.json
```

SQLite is the version bundled with the selected Python installation; the
JSON records the actual version. Exact replication of this run needs SQLite
3.49.1 and Python 3.12. Run directories and intermediate results are retained
under ignored `target/competitor-benchmarks/`; failed runs exit nonzero.
