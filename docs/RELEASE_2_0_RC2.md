# PicoVolt 2.0 release-candidate ledger

Candidate: **2.0.0-rc.2**, implemented 2026-09-08. Engine revision:
`eeb45c025b53f1b764d1e50774d6aaf700e17c77`.
Available for review in [PR #21](https://github.com/MiniJe/picovolt/pull/21).
This is an unpublished candidate; stable 2.0 qualification is incomplete.

## Implemented contract

- Native shared ownership with concurrent snapshot readers, FIFO writers,
  cooperative cancellation, deadlines and bounded admission.
- Checksummed page undo journals, ordered physical changes, byte/count limits,
  explicit pruning and expired-cursor errors. Verified checkpoint export and
  journaled compaction use the same writer queue.
- Versioned host-owned change sinks and a replay test that reconstructs and
  deeply verifies a second database. Format 6 prevents 1.x recovery bypass;
  migration preserves complete history and verifies historical golden images.
- RC.2 reduces physical-log encoding, redundant synchronization, index-catalog
  rewrites and binding serialization allocations. Logged workspaces rebuild
  index definitions on open; baked images retain binary indexes.
- Indexed mixed-numeric range intersection, BETWEEN and source-predicate
  pushdown improve selective queries. Unfiltered grouped aggregates retain
  group accumulators rather than all input rows.
- Atomic batch helpers in Rust, C, Python, Go, JS/WASM and CLI; native log
  configuration/status/pruning helpers and CLI setup/diagnostics. The Go source
  module remains `/v2`.

See [the interface quickstart](QUICKSTART_2_0.md),
[concurrency contract](CONCURRENCY.md) and [format specification](FORMAT.md)
for usage, costs, limits and platform assumptions.

## Validation evidence

| Check | RC.2 result |
| --- | --- |
| Full Rust suite, all targets/features | 330 passed |
| Clippy, all targets/features, warnings denied | Passed |
| Release CLI, HTTP server and C ABI | Built locally |
| Python native batch/transaction/DB-API tests | 6 passed |
| WASM and maintained JS/adapter/worker integration | 5 passed |
| C/CLI/HTTP interface smoke checks | Guide compiled; batch rollback, parameters and reopen passed |
| Unpruned default log capacity | 200 additional commits over 10,000 rows; 3,880,823 retained bytes and full reopen verification |
| Go adapter tests and vet | Passed; also tested on hosted Go 1.26/1.27 |
| Crash recovery | Six process-exit publication boundaries covered by full suite |
| Index rebuild/history/constraints and bounded queries | Regression tests passed |
| Hosted CI | All jobs passed, including Linux/Windows and ThreadSanitizer |
| Windows, universal2 macOS and manylinux wheels | Built and installation-smoke-tested; publication skipped |

[Candidate CI](https://github.com/MiniJe/picovolt/actions/runs/34245030371)
includes Linux ThreadSanitizer, Linux/Windows tests, minimum Rust 1.86,
dependency audit, fuzz build, Go adapters and WASM checks. A fuzz build is a
compile check, not a sustained fuzzing campaign. The
[candidate wheel workflow](https://github.com/MiniJe/picovolt/actions/runs/34245027768)
built and tested installation outside the source checkout. These are CI
artifacts, not registry-published versions. Automated validation and an
implementation review do not constitute an independent security audit.

## Performance evidence

The [RC.2 report](../benchmarks/COMPETITORS_2_0_RC2.md) compares the frozen
candidate with the preserved RC.1 runtime, SQLite 3.53.4 and DuckDB 1.5.5.
It includes repeated correctness-checked workloads at multiple scales,
CPU, peak process memory, storage, maintenance, reopen and best bulk APIs.
The [original RC.1 report](../benchmarks/COMPETITORS_2_0.md) and unsuccessful
observations remain available. No universal superiority claim follows from
these local workloads; the report records both wins and remaining losses.

## Independent review and external gates

The [copyable standalone prompt](INDEPENDENT_REVIEW_PROMPT.md) pins the engine
revision and specifies a fresh independent assessment, adversarial tests,
competitor comparisons and real external trials. Preparing that prompt does
not mean those trials have occurred.

Before a stable tag:

- Obtain an independent security review of recovery, format, FFI and network
  boundaries, with reproducible findings and a candidate-specific verdict.
- Rehearse migrations on real external 1.x databases with provenance and
  verified backups.
- Complete at least three external application trials and retain owner evidence;
  agent-created examples alone do not establish external adoption.
- Verify exact registry installations, release checksums, SBOMs, provenance and
  artifact publication through the release workflow.

Runnable starters retain their real published **1.9.0** pins while the candidate
is unpublished. Tagged releases still require exact versions and genuine
registry integrity/checksum pins; no checksum or completed release gate is
invented. Live-storage encryption, automatic replica application, distributed
consensus and concurrent C/Python/Go handles remain outside this implementation.
