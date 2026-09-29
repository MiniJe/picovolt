# PicoVolt 2.3.0 independent hardening review

**Mandate:** PV-HARDEN-M-001. **Exit:** `RELEASE_DEFECTS_FOUND`.

## Scope and environment

- Released tag `v2.3.0`, starting commit `089090bab62e47d5fde046d608094ebc7e8532dd` (the annotated tag object is `529677f6`). Audit branch: `audit/pv-2.3-hardening`.
- 2026-09-29, x86-64 host: Intel Xeon Gold 6138 (20 cores/40 threads), 128 GiB RAM, Windows with Docker Desktop/WSL2 Linux 6.18.33.2. The container reports about 62.6 GiB available memory. Executable tests use the `rust:1.86` image, `rustc 1.86.0`, and the checked-in lockfile. A Windows native attempt could not link because MSVC `link.exe` is absent; this is an environment limit, not an engine failure.
- This is an independent, bounded audit. It does not certify power-loss durability, guarantee absence of vulnerabilities, or benchmark all deployment platforms.

Executed baseline gates: `cargo test --locked --all-targets --all-features` in the Rust 1.86 container, with test debug symbols disabled to keep linking practical on Docker Desktop: **passed**. `cargo clippy --locked --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` also passed after the audit harness was added. The C smoke compiled with `-Wall -Wextra -Werror`.

Binding environment: the [C smoke](../scripts/audit_23_c_smoke.c) compiled with Debian `cc` against the matching generated `libpicovolt.so`; Python 3.11 ran the repository's ctypes suite; Go 1.24 ran repository tests and vet against that library; Node 24 ran the repository's three integration files (seven tests) against the **published** `picovolt@2.3.0` npm archive. The JS check verifies the released artifact, not a fresh local WASM rebuild.

## Attack matrix

| Area | Probe/evidence | Result and limit |
| --- | --- | --- |
| Physical storage and recovery | All-feature crash/recovery, corruption, format fixture, and journal tests; abrupt child process exit in `audit_23_bench`; bounded tmpfs full-disk probe | Existing suite passed. A 20 KiB insert on a filled 1 MiB tmpfs returned `ENOSPC`, and reopen showed count 0. Power-cut and filesystem fault injection on real hardware were not performed. |
| Malformed `.pvdb` and retrieval envelopes | `format_robustness`, `fuzz_smoke`, `persistent_retrieval_corruption` | Existing deterministic mutation tests passed; checksummed, internally inconsistent retrieval payloads fail closed in the tests. This is not sustained fuzzing. |
| MVCC and transactions | Concurrency, retained-reader, cancellation, deadline, rollback, and mutation-model tests | Existing all-feature suite passed. Long-duration writer starvation was not measured. |
| BM25, vector, hybrid | Existing generated corpus and model tests compare named indexes with full-scan fallback; benchmark harness asserts equality before recording timings | Passed on tested corpora. The 10,000-document persistent index cap limits corpus size. |
| C/Rust FFI and bindings | Rust C ABI interface tests in all-feature suite; compiled C smoke; Python, Go, and JS suites | C smoke passed; Python 12/12; Go `go test ./...` and `go vet ./...` passed; published npm 2.3 package JS/WASM integration 7/7. No panic or ownership failure observed. Raw C pointers remain caller-owned and unsafe after close by contract. |
| HTTP and CLI | Raw socket probes of the 2.3 server; actual `pv` binary against a 131,073-byte retrieval request and a four-byte truncated `.pvdb` | HTTP defect confirmed below. Normal oversized JSON body returns 413 and health remains 200. CLI returned bounded errors for both probes. |
| Encryption, backup, temp files | Code review of authenticated envelope bounds, vault publication, key-file handling, resumable bake, and journal temp files; existing encryption tests | No confirmed cryptographic failure. Secret-file validation and open have a filesystem race under a hostile local writer; this is an unproven hardening risk. |

## Confirmed defect

### HIGH — HTTP request parsing has no header, request-line, or connection bound

PicoVolt caps `/v1/query` bodies at 1 MiB in `src/bin/server.rs`, but `tiny_http` 0.12 parses request lines and headers first. Its `client.rs::read_next_line` appends each byte to an unrestricted `Vec`; its `TaskPool` starts a new thread when all existing threads are busy and has no maximum. Neither step is guarded by the bearer token. A remote client can therefore consume memory with an unfinished header and threads with unfinished requests before PicoVolt sees a `Request`. This is an availability defect on a non-loopback deployment, even with a configured token.

Reproduction against the tag's actual `picovolt-server` binary in a Linux container, bound with `PICOVOLT_SERVER_TOKEN=audit-test`:

1. Idle `/proc/1/status`: `VmRSS: 5460 kB`, `Threads: 23` (later idle pool: 19).
2. Open 128 TCP sockets, each sending `GET /v1/health HTTP/1.1\r\nX-Long: ` with no terminating header line. While held open, `/proc/1/status` showed `Threads: 147` and `VmRSS: 8548 kB`.
3. Send one 4 MiB unfinished `X-Fill` header. While held open, resident memory reached `12796 kB`, about 4 MiB above the preceding measurement. The server had not reached its body-size check.
4. Control: a 1,048,577-byte JSON POST returned `HTTP/1.1 413 Payload Too Large`; a subsequent `GET /v1/health` returned `HTTP/1.1 200 OK`.

The finite 128-socket and 4 MiB probes did not crash the process. The unbounded resource path is confirmed by the measured growth and dependency implementation. Exhaustion at machine scale was deliberately not attempted. A narrow in-application body check cannot reject input before `tiny_http` accepts and parses it. A2 should select a bounded HTTP parser/connection policy or require a proven ingress proxy with header, connection, and timeout limits; add a network-level regression gate for unauthenticated incomplete requests.

## Fixed defects

None. No engine format, architecture, or API was changed in this audit.

## Security observations and unresolved risks

- **HARDENING:** The reviewed binary decoders and retrieval envelope apply explicit length/count limits and checked arithmetic in the tested paths. Existing mutation tests cover truncated and rehashed corrupt data. Sustained coverage-guided fuzzing and sanitizers were not run in this environment.
- The `unsafe` inventory is `src/ffi.rs`, `src/ffi/vault.rs`, `src/storage/vle.rs`, and test-only mappings in `src/storage/cas.rs`. The production storage mapping copies a file to a private, retained temporary file before read-only mapping. C ABI entry points generally use a panic guard. The raw `pv_close`, `pv_stmt_close`, and free APIs require valid, unique pointers by documented contract; double free/use-after-close from a raw C caller is undefined behavior. Python and Go wrappers check closed handles in the reviewed paths.
- **HARDENING:** Encryption uses a fixed Argon2id profile, authenticated XChaCha20-Poly1305, bounded envelope lengths, and `Zeroizing` for owned secrets. Database pages, caller-owned copies, and swap are outside that zeroization guarantee. `read_secret_file` checks `symlink_metadata` before `File::open`, so a hostile process with write access to the secret directory may race the check; no exploit was demonstrated. Restrictive parent-directory permissions remain necessary.
- **HARDENING:** `bake_resumable` validates an existing partial and synchronizes the file before rename. The surrounding directory is not synchronized after rename, so post-power-loss rename durability is not independently established by this audit.
- A read-only workspace opened as an unprivileged user returned `Permission denied` without panic. On a 1 MiB tmpfs nearly filled by a separate file, `pv query` returned `No space left on device` for a 20 KiB insert; after removing the filler, reopening and `SELECT COUNT(*)` returned `0`. This probes a specific failed write, not every disk-full timing window.
- No release, tag, or publication was performed.

## Benchmark method and results

`examples/audit_23_bench.rs` reproduces retrieval and durable-workspace operations at 1,000, 5,000, and 10,000 rows. Its 10,000-row ceiling follows `MAX_RETRIEVAL_DOCUMENTS`. `examples/audit_23_core_bench.rs` covers larger storage/SQL corpora without persistent retrieval. Measurements use one optimized run per size on Docker Desktop's Linux filesystem and are descriptive, not cross-platform SLAs. The harness records elapsed wall time, insert throughput, image/workspace sizes, persisted retrieval-index bytes, and `/proc/self/status` RSS where available. RSS high-water marks are process cumulative across sequential sizes. Recovery timing opens a workspace after a child exits during an explicit transaction; it is not a power-cut simulation. The legacy v1.9 import timing is compatibility-open cost, not a full migration workflow.

Raw measurements: [retrieval/workspace JSON](../benchmarks/pv-2.3-audit-2026-09-29.json), generated by [the reproducible harness](../examples/audit_23_bench.rs). All times below are one observation in **milliseconds**, except the explicitly marked seconds and MiB. Batched inserts exclude the separately timed flush. SQL point uses a secondary index through SQL; indexed lookup calls the direct indexed API. The retrieval query selects the full corpus, including common `apple` and `東京` terms. The harness checks named results equal the corresponding fallback results before accepting each row.

| Rows | Batched insert ms (rows/s) | Flush | Bake | Open `.pvdb` | SQL point | Direct indexed point | Full scan |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 154.55 (6,470) | 2.86 | 33.33 | 30.61 | 0.032 | 0.0027 | 0.64 |
| 5,000 | 390.12 (12,816) | 13.19 | 146.95 | 108.50 | 0.034 | 0.0017 | 2.66 |
| 10,000 | 715.66 (13,973) | 26.98 | 263.45 | 191.32 | 0.038 | 0.0017 | 5.43 |

| Rows | BM25 named / fallback | Vector named / fallback | Hybrid named / fallback |
| ---: | ---: | ---: | ---: |
| 1,000 | 1.08 / 4.76 | 0.93 / 1.11 | 1.37 / 5.06 |
| 5,000 | 5.86 / 27.22 | 4.53 / 5.60 | 7.87 / 31.23 |
| 10,000 | 26.67 / 58.59 | 9.75 / 11.94 | 15.57 / 64.10 |

| Rows | Ordinary index build ms | BM25 build s | Vector build s | Insert s | Update s | Delete s | Commit s | Crash recovery s |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 0.48 | 4.95 | 4.55 | 4.78 | 4.75 | 4.75 | 4.96 | 5.14 |
| 5,000 | 2.47 | 23.44 | 23.00 | 63.99 | 51.19 | 44.19 | 47.95 | 49.82 |
| 10,000 | 4.60 | 46.11 | 49.38 | 96.20 | 94.15 | 95.59 | 95.06 | 65.10 |

| Rows | Workspace before / after writes MiB | Baked image MiB | Persisted retrieval index MiB | Process peak / resident RSS MiB |
| ---: | ---: | ---: | ---: | ---: |
| 1,000 | 0.28 / 0.63 | 0.28 | 0.10 | 9.32 / 9.32 |
| 5,000 | 1.39 / 3.17 | 1.43 | 0.49 | 28.80 / 26.79 |
| 10,000 | 2.78 / 6.35 | 2.85 | 0.99 | 53.84 / 51.79 |

Large storage/SQL measurements: [core JSON](../benchmarks/pv-2.3-audit-core-2026-09-29.json), generated by [the core harness](../examples/audit_23_core_bench.rs). These rows use an **in-memory source** followed by bake and production reopen, so insert/index-build timings are not durable workspace timings. There are no persistent retrieval indexes on these corpora because their 10,000-document admission cap applies. Times are one observation in milliseconds.

| Rows | Insert ms (rows/s) | Ordinary index build | Bake | Production open | Indexed SQL point | Full scan | Image MiB | Peak / resident RSS MiB |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100,000 | 78.35 (1.28M) | 46.74 | 45.91 | 52.77 | 0.052 | 58.76 | 8.11 | 75.49 / 75.49 |
| 1,000,000 | 841.03 (1.19M) | 727.72 | 324.08 | 650.86 | 0.058 | 773.24 | 81.06 | 627.98 / 605.66 |

The old-format v1.9 fixture imported in 0.425 ms in-process. Separately, `pv migrate tests/fixtures/golden_v1_9_0.pvdb /tmp/audit-migrated.pvdb --json` took 76.24 ms including CLI startup, produced 16,726 bytes, moved format 5→8, and reported `verified: true` with 240 row versions. This tiny fixture is a compatibility probe, not a representative bulk migration workload.

**Performance observations.** Named BM25 and hybrid search saved repeated index construction on this corpus; exact vector gained less because the SQL selection still scans/materializes IDs and distance calculations remain exhaustive. Open time grew with corpus size, consistent with the documented full source verification. Workspace logical bytes grew about 2.3× after a delete, insert, update, and explicit commit, reflecting retained immutable index generations. The much larger write and recovery times are from the unlogged development-workspace backup path on Docker Desktop. A 10,000-row in-progress backup held about 10,004 files and 43 MiB of physical storage although the logical workspace counted about 3 MiB. The engine documentation already describes whole-workspace rollback images; this audit qualifies its cost on many small CAS blobs. Single-run timing and container fsync behavior prevent assigning these numbers as a general native-filesystem SLA.

## Suggested gates

1. Add a network ingress test that sends long unfinished request lines/headers and many unauthenticated slow connections; assert fixed byte, thread, and timeout ceilings.
2. Run sustained seeded fuzzing of monolith, record, page, index, SQL, and HTTP input parsing, with sanitizer jobs where supported.
3. Keep named-index versus full-scan differential tests across filtered, historical, Unicode, empty, and high-cardinality corpora; enforce the current 10,000-document admission limit explicitly.
4. Compare release-build benchmark medians on pinned native Linux and Windows filesystems, separately from Docker Desktop. Track open, scan, indexed point, retrieval, mutation, commit, and recovery separately, plus index bytes, blob-file count, and peak RSS. Investigate size-dependent slopes and file-growth regressions before changing baselines.
5. Add fault-injection runs for disk-full, permission loss, and directory-sync failures, with explicit `TransactionOutcomeUnknown` expectations where applicable.
