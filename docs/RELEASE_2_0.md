# PicoVolt 2.0 release-candidate ledger

Candidate: **2.0.0-rc.1**. Implementation date: **2026-09-08**.
This is a local candidate build, not a published stable release.

## Implemented contract

- Native shared ownership with explicit concurrent snapshot readers and FIFO
  writers, cooperative cancellation, deadlines and bounded admission.
- Checksummed incremental page undo journals and ordered physical commits with
  byte/count retention limits, explicit pruning and expired-cursor errors.
- Verified, no-clobber checkpoint export with an exact change cursor, plus
  journaled compaction through the same writer queue.
- Versioned host-owned change sinks for encrypted transport and replication;
  a replay test reconstructs and deeply verifies a second database.
- Format 6 prevents 1.x recovery bypass. Migration retains complete history and
  verifies every historical golden image, with a new deterministic v6 fixture.
- CLI change inspection/pruning; maintained C, Python, Go and JS interfaces.
  The Go source module uses `/v2` for semantic import versioning.

See [CONCURRENCY.md](CONCURRENCY.md) for the precise costs, limits, scheduling,
platform durability assumptions and unsupported capabilities.

## Local evidence

| Check | Result |
| --- | --- |
| Full Rust suite, all targets and all features | 321 tests passed on the final implementation |
| Final concurrency/transaction/migration regression run | 30 tests passed, including checkpoint export and compaction |
| Clippy, all targets and all features, warnings denied | Passed |
| Rust 1.86 minimum-version check, all features | Passed |
| wasm32 build and JS prepared/adapter/worker integration | Passed; 4 integration tests |
| Rust documentation tests | 2 passed |
| Registry policy regression suite | 22 passed |
| Python native transaction/DB-API suite | 5 passed |
| Go adapter tests and vet | Passed; v2 import-path verification included |
| Cross-process journal crash injection | Passed at six publication boundaries |
| Corrupt recovery evidence | Rejected before changing live state |
| Golden migration rehearsal | All checked-in historical images and v6 passed |
| Release CLI, server and C ABI build | Built; CLI reports 2.0.0-rc.1 |

The [Windows benchmark](../benchmarks/concurrency-v2-windows.json) records four
snapshot readers and 40 durable writes over 2000 rows. Reader p95 was 0.088 ms,
writer p95 94.8 ms, and four snapshot admissions took 474 ms in total. A final
insert journaled one page of a 24-page image. These are local observations,
not latency promises. Full snapshot construction and retained-log enumeration
remain explicit performance costs.

## Gates before a stable tag

- Independent security review of format/recovery, FFI and network boundaries.
- A passing Linux ThreadSanitizer run. The CI job is implemented; it cannot be
  claimed from a Windows-only local run.
- Migration rehearsals on real external 1.x databases with verified backups.
- At least three external applications completing candidate trials.
- Exact candidate/stable registry installation tests, release checksums, SBOMs,
  provenance and artifact publication through the release workflow.

The maintained runnable starters stay pinned to published **1.9.0** while this
candidate is unpublished. `starters/REGISTRY_VERSION` controls only the default
development check. Tagged releases pass an explicit version and still require
exact matching versions and genuine registry integrity/checksum pins. Stable
versions cannot use the prerelease baseline exception. No candidate checksum
has been invented and no stable-release gate has been marked complete.

Encryption of live storage, automatic replica application, distributed
consensus and new concurrent C/Python/Go handles are not implemented; the
concurrent handle API is native Rust and the existing adapters remain supported.
