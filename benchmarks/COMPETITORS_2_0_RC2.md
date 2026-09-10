# PicoVolt 2.0 rc.2: measured performance and remaining gaps

**RC.2 substantially improves durable writes, selective ranges, joins and memory use over RC.1. It does not beat every competitor.** SQLite still leads most measured workloads; DuckDB leads grouped analytics, top-N and bulk import. PicoVolt has strong indexed-query results against DuckDB, with the advantage depending on data size.

At 10,000 rows, single-row commits improved 10.8×, ranges 27.4× and joins 34.7× over the clean RC.1 rerun. Peak process working set fell 45.8%. The table below also preserves regressions.

## Revisions, competitors and method

- Engine: `eeb45c025b53f1b764d1e50774d6aaf700e17c77`, version 2.0.0-rc.2. Harness revision and exact DLL/initializer hashes are recorded in every raw result.
- Baseline: preserved RC.1 runtime at `716417cbcb94325a72a5c02709e5749622f18e33`, measured again with the same CPU/memory instrumentation. Its DuckDB comparator is 1.4.4; only PicoVolt RC.1 is used for before/after conclusions.
- Current competitors: SQLite 3.53.4, from the [official Windows DLL](https://sqlite.org/download.html), and DuckDB 1.5.5, installed into a fresh directory from [the versioned wheel](https://pypi.org/project/duckdb/1.5.5/). Both the DuckDB module version and `SELECT version()` are checked before timing. SQLite reports its loaded library version.
- Windows 11 / NTFS, Xeon W-2125, four physical/eight logical cores; Python 3.12.10, Rust 1.95.0 GNU release build. Hosted MSVC/macOS/Linux wheels are separately installation-tested artifacts, not the binaries timed here.
- Five fresh processes per engine at 1,000, 10,000 and 50,000 initial rows: **45 verified current-version runs**. Engines rotate order within each trial; runs are serial. No local compilation or test suite runs during those measurements. Baseline and RC.2 are separate series, so time-dependent host variation remains possible.
- Same deterministic data, 50 categories, three equivalent indexes, SQL and fully materialized Python results. Five read warmups, then 200 point queries, 30 ranges, 20 groups, 20 top-N queries and 20 joins per trial. All results are checked against a reference outside timing.
- Writes load the initial rows in one transaction, then commit 60 single rows and ten batches of 100. Rollback is checked. Reopen must reproduce every final row and a shared canonical SHA-256; initial/final row counts and hashes are in the raw files.
- This is public Python API latency: PicoVolt ctypes/JSON, SQLite stdlib and DuckDB native extension. It includes parameter binding and result conversion, not just engine execution. No cold OS-cache, larger-than-RAM, power-cut, external-user or universal-market coverage is claimed.

PicoVolt uses its default format-6 synced log and budgets; SQLite uses verified WAL with `synchronous=FULL`; DuckDB uses persistent storage and one execution thread. These request durable transactions, but Windows process-crash validation does not establish identical power-loss guarantees. See [SQLite synchronous](https://sqlite.org/pragma.html#pragma_synchronous), [DuckDB transactions](https://duckdb.org/docs/stable/sql/statements/transactions) and [PicoVolt’s platform contract](../docs/CONCURRENCY.md).

PicoVolt explicitly prunes before writes and every ten commits. The shipped CLI is retained for a consistent RC.1 comparison: process launch, index rebuild on open and pruning are included in sustained-write time. RC.2 also exposes native pruning, which is not timed here. No retention limit or durability mode was relaxed.

## 10,000-row latency

Milliseconds; median of five per-trial medians. Per-trial p95, min/max and every sample remain in the raw results.

| Workload | PicoVolt rc.2 | SQLite 3.53.4 | DuckDB 1.5.5 |
| --- | ---: | ---: | ---: |
| Common SQL load | 282.725 | 30.803 | 4279.786 |
| Create three indexes | 22.193 | 8.981 | 20.188 |
| Indexed point | 0.0253 | 0.0097 | 0.4374 |
| Indexed range, 100 rows | 0.1925 | 0.1217 | 1.039 |
| Grouped sum | 5.895 | 3.882 | 1.045 |
| Unindexed top 20 | 8.127 | 1.537 | 0.6817 |
| Indexed join | 0.5069 | 0.2647 | 1.763 |
| Single-row synced commit | 16.059 | 0.7221 | 1.670 |
| 100-row synced transaction | 17.254 | 1.874 | 46.717 |
| Sustained writes, including pruning | 1807.480 | 67.677 | 594.602 |
| Rollback one delete | 45.961 | 0.0867 | 0.7755 |
| Warm OS-cache reopen | 36.051 | 0.9527 | 23.989 |
| Best bulk API load | 155.619 | 16.114 | 43.206 |

The common SQL load sends one bound INSERT per row inside a transaction. It is **not DuckDB’s best ingestion path**. The separate bulk row uses PicoVolt `execute_many`, SQLite `executemany` in one transaction and DuckDB `COPY` from a prepared CSV, with input preparation outside timing and full verification afterward. DuckDB COPY is much faster than its row-at-a-time loop and faster than PicoVolt’s batch API.

## RC.1 to RC.2, including regressions

| Workload | RC.1 ms | RC.2 ms | RC.1 / RC.2 |
| --- | ---: | ---: | ---: |
| Common SQL load | 314.500 | 282.725 | 1.11× |
| Create three indexes | 118.157 | 22.193 | 5.32× |
| Indexed point | 0.0155 | 0.0253 | 0.61× |
| Indexed range, 100 rows | 5.267 | 0.1925 | 27.35× |
| Grouped sum | 11.023 | 5.895 | 1.87× |
| Unindexed top 20 | 7.009 | 8.127 | 0.86× |
| Indexed join | 17.572 | 0.5069 | 34.67× |
| Single-row synced commit | 173.706 | 16.059 | 10.82× |
| 100-row synced transaction | 179.270 | 17.254 | 10.39× |
| Sustained writes, including pruning | 13446.480 | 1807.480 | 7.44× |
| Rollback one delete | 121.140 | 45.961 | 2.64× |
| Warm OS-cache reopen | 25.741 | 36.051 | 0.71× |

Ratios below 1 are regressions: point lookup, top-N and reopen were slower in the 10,000-row run. Short-query wall times vary across trials and dataset sizes; these are observations, not statistically established universal effects. Reopen has a deliberate cost: logged workspaces now rebuild index definitions from rows instead of rewriting every index entry at each commit.

## CPU and memory

CPU values below are **total CPU milliseconds for the indicated workload per trial**, then the median across trials. Windows process-time samples quantize at 15.625 ms. A recorded zero means below measurement resolution, not zero computation. CLI child CPU is excluded; its wall time is included in sustained writes. These numbers do not measure electrical energy.

| Workload | RC.1 CPU ms | RC.2 CPU ms |
| --- | ---: | ---: |
| Common SQL load | 281.250 | 250.000 |
| Indexed range, 100 rows | 156.250 | 0.000 |
| Grouped sum | 187.500 | 93.750 |
| Unindexed top 20 | 140.625 | 156.250 |
| Indexed join | 312.500 | 15.625 |
| Single-row synced commit | 8109.375 | 656.250 |
| 100-row synced transaction | 1406.250 | 125.000 |
| Warm OS-cache reopen | 31.250 | 31.250 |

| Main workload footprint | RC.1 | RC.2 | SQLite | DuckDB |
| --- | ---: | ---: | ---: | ---: |
| Peak process working set, MiB | 63.285 | 34.273 | 29.941 | 116.398 |
| Files after close/prune, MiB | 2.538 | 0.719 | 0.602 | 1.512 |

Working set includes Python, loaded drivers, engine state and reference verification. It is sampled before the optional bulk database; the raw per-run records also contain a separate peak including bulk. Files after close are not cumulative bytes written or device I/O. Pruned change history is excluded from the final footprint; retained-log capacity is checked separately below.

## Scaling

Each entry is PicoVolt / SQLite / DuckDB, in milliseconds. Full results include all workloads at each scale.

| Initial rows | Point | Range 100 | Group sum | Join | Single commit | Best bulk load |
| ---: | --- | --- | --- | --- | --- | --- |
| 1,000 | 0.0229 / 0.0095 / 0.3717 | 0.1529 / 0.1071 / 0.7534 | 0.4891 / 0.4333 / 0.8412 | 0.0807 / 0.0299 / 0.9659 | 15.079 / 0.7149 / 1.464 | 67.327 / 2.526 / 22.668 |
| 10,000 | 0.0253 / 0.0097 / 0.4374 | 0.1925 / 0.1217 / 1.039 | 5.895 / 3.882 / 1.045 | 0.5069 / 0.2647 / 1.763 | 16.059 / 0.7221 / 1.670 | 155.619 / 16.114 / 43.206 |
| 50,000 | 0.0137 / 0.0147 / 0.3173 | 0.0935 / 0.0799 / 0.7163 | 25.163 / 19.667 / 0.8350 | 1.953 / 1.347 / 1.843 | 14.112 / 0.8198 / 1.615 | 549.809 / 69.154 / 84.687 |

At 50,000 rows PicoVolt’s point median narrowly beats SQLite, while SQLite still leads ranges, aggregates, joins, durable commits and bulk loading. PicoVolt’s join advantage over DuckDB at smaller sizes does not hold at 50,000 rows. Do not extrapolate one favorable cell into overall superiority.

## Retention, integrity and implementation

Compact binary change records, definition-only logged index catalogs and fewer redundant sync/serialization steps reduce write amplification. Selective numeric range intersection and source-filter pushdown reduce scanned candidates; grouped queries retain accumulators. These are normal product paths, not benchmark-only switches. Baked images keep binary indexes; public change JSON remains schema 1.

The [unpruned RC.2 trial](log-retention-v2-rc2.json) starts with 10,000 rows and three indexes, then commits 200 additional rows without pruning or increasing defaults. All 10,200 rows survive reopen; 204 retained commits occupy 3,880,823 bytes, below the unchanged 256 MiB limit. This is a single capacity/correctness observation. The [original RC.1 failure](competitors-v2-unpruned-limit.json) hit that budget after 38 additional-row attempts; the tests do not establish identical hardware power-loss guarantees.

Engine validation: 330 Rust tests, warning-free all-feature Clippy, Python/Go/JS binding tests and [hosted CI including ThreadSanitizer](https://github.com/MiniJe/picovolt/actions/runs/34245030371). The C guide, CLI batch/rollback/status and HTTP query/multi-row/reopen flow also passed local smoke checks. Independent review and external application/migration trials remain pending; use the [standalone review prompt](../docs/INDEPENDENT_REVIEW_PROMPT.md).

The [native concurrency envelope](concurrency-v2-rc2-windows.json) separately checks four pinned readers alongside 40 synced writes over 2,000 rows. Reader p95 was 0.190 ms, writer p95 75.345 ms, and four snapshot admissions totaled 438.451 ms. The final insert changed 1 page. This is one local run, not a five-trial competitor claim.

## Reproduction and evidence corrections

Build the pinned engine in release mode with `--features capi,server --bins --lib --example benchmark_workspace`. Set `PICOVOLT_LIB` to that DLL and `PYTHONPATH` to the matching Python binding plus a **fresh** DuckDB 1.5.5 installation directory. On Windows preload the verified SQLite DLL with `PICOVOLT_BENCH_SQLITE_LIBRARY`; set `PICOVOLT_BENCH_DUCKDB_VERSION=1.5.5`. Then run:

```text
python scripts/benchmark_competitors.py --initializer <release>/examples/benchmark_workspace.exe --rows 10000 --trials 5 --bulk-api --picovolt-source-commit eeb45c025b53f1b764d1e50774d6aaf700e17c77 --output results.json
```

Repeat at 1,000 and 50,000 rows. Run `scripts/benchmark_log_retention.py` with the same initializer/library for the unpruned capacity check. Official SQLite ZIP SHA3-256: `deddee963c810d1eeac3ce5e15c7c41da21a1c54d7a39cf54fbf577d2f50de3a`; DLL SHA-256 is recorded in each current raw result.

- Current raw results: [1,000 rows](competitors-v2-rc2-current-1000.json), [10,000 rows](competitors-v2-rc2-current-10000.json), [50,000 rows](competitors-v2-rc2-current-50000.json).
- Before/after source: [clean RC.1 rerun](competitors-v2-rc1-clean.json). [The earlier CPU attempt](competitors-v2-rc1-cpu-baseline.json) overlapped compilation and is excluded from conclusions.
- [Interim implementation](competitors-v2-rc2-windows.json) predates final optimization. [Superseded-runtime notes](competitors-v2-rc2-superseded.json) preserve complete older-DuckDB runs and the aborted 50,000-row series.
- An earlier report incorrectly labeled DuckDB 1.4.4 as 1.5.5 after a mixed installation left newer metadata beside the old native module. The [RC.1 report](COMPETITORS_2_0.md) is corrected; its raw timings/version were not changed. Final current-version conclusions use the fresh runtime-verified reruns.

Further priorities are small-commit overhead, top-N selection, rollback/reopen costs and broader independent workloads. The measured gains justify this candidate’s improvements; they do not justify a universal “fastest database” claim.
