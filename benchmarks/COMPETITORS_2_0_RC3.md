# PicoVolt RC3: review fixes and measured performance

RC3 fixes the default primary-key bottleneck: **91.5x faster at 10,000 rows**, with **98.99% less measured CPU time** than the preserved RC2 runtime. It does not beat SQLite or DuckDB on this bulk-loading workload.

## Candidate and method

- RC3 engine: `adf527af35ca7fec6c300bdf24bda9810f0bf7f4`, version 2.0.0-rc.3; format 7. RC2: `eeb45c025b53f1b764d1e50774d6aaf700e17c77`, version 2.0.0-rc.2; format 6.
- Native Windows GNU release artifacts, Rust 1.95.0; exact library and initializer hashes and harness revisions are recorded in raw results. RC3 native build enables `capi,data-tools,enterprise,server`. RC2 uses its preserved original artifact: this is an artifact comparison, not a claim of a controlled identical-feature rebuild.
- Windows 11/NTFS, Xeon W-2125, Python 3.12.10; SQLite 3.53.4 and DuckDB 1.5.5 verified from loaded runtimes. Same previously obtained official SQLite DLL and versioned DuckDB wheel as the RC2 report.
- **85 fresh-process comparison runs**, all completed and correctness-checked: 55 primary-key trials and 30 ordinary-workload trials. Five trials per included engine/size, rotating engine order. Runs are serial; local builds and test suites finished first.
- No OS-cache flushing or control of unrelated desktop applications. Min/max and individual samples remain available. The ordinary RC2 and RC3 series ran separately; modest differences can include host variation and must not be treated as statistically established engine improvements.
- PicoVolt uses its synced incremental log with default limits; SQLite uses WAL and synchronous=FULL; DuckDB uses persistent storage and one execution thread. These do not establish identical power-loss guarantees. All results are materialized through public Python APIs, so timings include bindings and result conversion.

## Default primary-key bulk load

Identical `t(id INTEGER PRIMARY KEY, g INTEGER, amount INTEGER)` data; no manually added index. PicoVolt uses execute_many, SQLite uses executemany within a transaction, and DuckDB uses COPY. CSV/data generation is excluded; conversion, insertion and commit are included. Every row and primary-key rejection are checked after reopen.

Milliseconds, median of five fresh processes.

| Rows | RC2 | RC3 | SQLite 3.53.4 | DuckDB 1.5.5 |
| ---: | ---: | ---: | ---: | ---: |
| 1,000 | 119.201 | 25.788 | 1.817 | 19.066 |
| 10,000 | 10,424.127 | 113.980 | 13.606 | 37.966 |
| 50,000 | Not rerun | 628.666 | 79.220 | 97.029 |

The independent RC2 review recorded five 50,000-row default-load timeouts at its 60-second cap. That result is retained as a timeout, not converted into an invented completed latency. This follow-up reran RC2 at 1,000 and 10,000 rows; every RC3 50,000-row trial completed.

At 10,000 rows, measured CPU time fell from 9,250.00 ms to 93.75 ms. Peak whole-process memory rose from 28.91 MiB to 30.43 MiB (**+5.2%**): the speed gain has an index-memory cost. Windows CPU measurements are quantized; a reported zero means below timer resolution, not zero computation.

Trial ranges (min–max, ms):

| Rows | RC2 | RC3 | SQLite | DuckDB |
| ---: | ---: | ---: | ---: | ---: |
| 1,000 | 112.067–412.385 | 24.411–28.494 | 1.677–2.240 | 15.686–36.545 |
| 10,000 | 10303.197–12270.698 | 105.201–145.998 | 12.402–16.239 | 26.924–45.413 |
| 50,000 | Not rerun | 484.258–737.770 | 72.840–84.814 | 93.378–133.331 |

## Ordinary workloads at 10,000 rows

Same maintained common-SQL harness as RC2, including result checks, rollback and full reopen hashes. Median of five per-trial medians, in milliseconds; read warmups and repetition counts are unchanged. This schema has no primary key, so it separately checks behavior outside the newly fixed keyed path.

| Workload | RC2 rerun | RC3 | SQLite | DuckDB |
| --- | ---: | ---: | ---: | ---: |
| Common SQL load | 278.2043 | 262.8046 | 31.5246 | 4,317.1800 |
| Best bulk API | 184.0876 | 161.1816 | 15.2775 | 51.2925 |
| Create three indexes | 21.4802 | 19.9700 | 8.6782 | 18.2541 |
| Indexed point | 0.0166 | 0.0136 | 0.0072 | 0.3987 |
| Indexed range, 100 rows | 0.1390 | 0.1166 | 0.0643 | 0.7063 |
| Grouped sum | 5.0625 | 4.5974 | 3.7792 | 0.8344 |
| Unindexed top 20 | 7.7108 | 6.8510 | 1.2658 | 0.6407 |
| Indexed join | 0.4088 | 0.4004 | 0.2116 | 1.2827 |
| Single-row synced commit | 17.2406 | 16.0862 | 0.7700 | 1.5749 |
| 100-row commit | 20.6707 | 19.8771 | 1.8881 | 43.9733 |
| Sustained writes including maintenance | 1,962.8514 | 1,801.1901 | 69.9346 | 535.6641 |
| Rollback one delete | 53.2234 | 46.8755 | 0.1170 | 0.7861 |
| Warm OS-cache reopen | 35.0048 | 31.5413 | 1.1066 | 20.0679 |

Every RC3 ordinary-workload median was lower in these runs, by roughly 2–18%, but several trial ranges overlap. SQLite remains faster in every listed RC3 workload. PicoVolt beats DuckDB on point/range/join queries, common per-row SQL loading and 100-row commits; DuckDB wins bulk loading, aggregation, top-N, single commits, sustained writes, rollback and reopen.

CLI pruning before writes and every ten commits remains inside sustained-write timing. The log is not silently disabled or its retention budget enlarged. Storage measurements include retained history; SQLite and DuckDB have different checkpoint/retention behavior, so file sizes do not represent identical historical capabilities.

## Recovery capacity and concurrency

- Default unpruned retention: 10,000 initial rows, three indexes, **200 additional commits**, 204 total retained commits and **3,889,581 bytes**. All 10,200 rows and log status matched after reopen.
- Separate native concurrency envelope: four readers, 2,000 rows, 160 reads and 40 writes. Read p50/p95: 0.0600/0.1403 ms; write p50/p95: 86.4576/104.7347 ms. This is one local envelope observation, not a competitor comparison.

## Raw evidence and limitations

- [Primary-key trials and hashes](rc3-primary-keys.json)
- [Fresh RC2 ordinary-workload baseline](rc2-repeat-10000.json)
- [RC3 ordinary-workload trials](rc3-current-10000.json)
- [Unpruned retention](rc3-log-retention.json) and [concurrency envelope](rc3-concurrency.json)
- [Previous RC2 report](COMPETITORS_2_0_RC2.md) and [release-gate ledger](../docs/RELEASE_2_0.md)

These synthetic local measurements do not complete external owner trials, real legacy-data migrations, an independent RC3 review, a security review, power-cut testing, larger-than-RAM testing or registry publication. There is no universal competitor-win or energy-use claim.
