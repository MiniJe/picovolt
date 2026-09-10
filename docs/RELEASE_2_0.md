# PicoVolt 2.0 release ledger

Current release: **2.0.0**, authorized for publication on 2026-09-10.
Publication and registry verification are being completed through PR #21 and
the tagged release workflows.

## Maintainer decision â€” 2026-09-10

The maintainer explicitly approved publishing 2.0.0 with independent review and
external migration/trial acceptance deferred. Those activities remain
uncompleted follow-up work; this decision does not claim review or external
acceptance evidence. Engineering checks, exact registry installs, native
downloads, checksums, SBOMs and provenance remain required release checks.

## Stable preparation â€” 2026-09-09

- Correct the Linux CI lock-lifetime regression found on RC3 documentation
  revision `8720192`. Transaction guards now explicitly unlock their file before
  closing it, so a duplicate inherited during concurrent process creation cannot
  delay release. A Unix regression test keeps a duplicate descriptor alive,
  verifies exclusion while held, and verifies reacquisition after guard drop.
- Align Cargo and Python metadata and every maintained starter with 2.0.0.
  Go starters migrate to the `/v2` module path. Go checksums are calculated from
  canonical Git source bytes including the inherited root LICENSE; the method
  is checked against the published 1.9.0 checksum before generating 2.0.0 pins.
- Build the npm package with release-pinned Rust 1.98.1 and wasm-pack 0.15.0;
  verify its exact tarball integrity before publication.
- Preserve historical RC3 benchmark evidence below. The lock-lifetime repair
  does not establish new independent-review or external-trial evidence.

### Validation of engine revision `fe1aea1`

| Check | Result |
| --- | --- |
| Local Rust all targets/features | 340 Linux tests; 339 Windows tests passed |
| Local Linux lint/docs | All-feature Clippy and two doctests passed |
| Lock/recovery regression | 20 repeated journal suites and 1,000 abrupt-process crash cycles passed |
| Starter policy | 24 tests and explicit 2.0.0 policy passed |
| JavaScript/WASM | Five tests passed against built and isolated installed npm packages |
| Python | Eight tests passed against the installed hosted Windows 2.0.0 wheel |
| Rust distribution | `cargo package --locked` built and verified the packaged crate |
| Hosted CI | [All jobs passed](https://github.com/MiniJe/picovolt/actions/runs/34401341252), including Linux/Windows, Go 1.26/1.27, WASM, MSRV, ThreadSanitizer and dependency audit |
| Platform wheels | [Windows, macOS universal2 and manylinux builds/install smoke tests passed](https://github.com/MiniJe/picovolt/actions/runs/34401525448); publication skipped |
| Performance budgets | [Benchmark smoke and all seven budgets passed](https://github.com/MiniJe/picovolt/actions/runs/34401528348) |
| Extended fuzz/model/recovery | [All six fuzz shards and 5,000-operation/1,000-crash stress checks passed](https://github.com/MiniJe/picovolt/actions/runs/34401531502) |

For the September 9 preparation package, the initial hosted npm-package check correctly rejected the Windows-mounted
tarball's executable permission bits. Packing identical contents on a native
Linux filesystem with regular 0644 files reproduced the hosted checksum exactly:

```text
sha512-oiiQNnCtSPWtKk+9hxmBhwqzXA9ZkZmZT8VABKwOg/D3hIg0mwSR2nBLuKY6F2/yCNHnCmHgpFENZeOt4sZiEg==
```

The [hosted npm-package gate passed with the corrected starter pins](https://github.com/MiniJe/picovolt/actions/runs/34402128551),
and the downloaded hosted tarball matches the local tarball byte for byte
(SHA-256 `332e8e9493b7f57d957cdbc77825e20969a3e9e884611745d43a9c79d2e50325`).
This packaging-only follow-up
does not change engine or binding contents. Exact registry installs, native
release bundles, SBOMs and attestations remain post-tag workflow gates. The
independent review and external-owner activities below were not completed by
these engineering checks; their deferral was explicitly authorized separately.

## Final release package

The authorization README update produces this final 2.0.0 npm package:

```text
sha512-+ZTOep8ANxJcI21WijHIRGlNa7+Dk1eGJhuCjuJVU4cXBM8LZFfGlxNfljDC7jRY0LmusSYkmONFHsrOjRVRQA==
SHA-256 0b975b152f2c47fdc6f1097ba5347b1cf8a490be720442f5c3ce8b181a77f8f4
```

The tagged workflow must reproduce this integrity before publishing.

## RC3 qualification baseline

RC3: **2.0.0-rc.3**, engine `adf527af35ca7fec6c300bdf24bda9810f0bf7f4`, unpublished.
The [RC2 ledger](RELEASE_2_0_RC2.md) preserves the previous candidate's evidence.

## Independent review and corrections

The separate RC2 evaluation tested frozen engine `eeb45c0` and concluded that
it was not ready for release. Its deliverable hashes and all three reproduced
findings were checked locally before this follow-up. Original reports and raw
results remain unchanged in `D:/picovolt-independent-review-eeb45c0`.

| Finding | RC3 correction |
| --- | --- |
| Missing log history reused an acknowledged sequence | Format-7 manifest anchor; complete unpruned continuity checks before open, write, status, consumption and pruning |
| Skipped SQLite trigger changed restored data | Complete compound-statement splitting; comment handling; lexical preflight rejects incomplete dumps |
| Registry starter runner raised NameError | Explicit policy-version argument; all five dispatch paths covered |
| Default primary-key ingestion repeatedly scanned the table | Automatic constraint indexes and rebuild on writable legacy open; numeric-aware batch uniqueness sets |

Regression coverage includes lost tail/middle/whole log, pruning, rollback,
process-crash publication boundaries, intact legacy upgrade and missing legacy
tail, replica cursor publication, nested CASE in triggers, comments, incomplete
dumps, mixed numeric duplicates, nullable UNIQUE values and reopen behavior.

## Validation and performance

| Check | RC3 result |
| --- | --- |
| Rust, all targets/features | 339 passed |
| Clippy and doctests | Passed; 2 doctests |
| Script policy/runner regressions | 24 passed |
| Published 1.9.0 registry starters | Rust, Node, browser, Python and Linux Go passed |
| Python | 8 tests passed against both native GNU build and isolated installed Windows wheel |
| JavaScript/WASM | 5 tests passed against both built and installed npm packages |
| Go | Uncached tests and vet passed locally; hosted Go 1.26/1.27 passed |
| C, CLI and HTTP | Quickstart, batch atomicity/rollback, parameters and reopen passed |
| Candidate hosted CI | All jobs passed on the frozen engine |
| Windows, macOS and Linux wheels | Built and installation-smoke-tested; publication skipped |
| Competitor benchmarks | 85 complete fresh-process comparison runs |
| Retention | 200 additional unpruned commits; 10,200 rows verified on reopen |

[Engine CI](https://github.com/MiniJe/picovolt/actions/runs/34265047478) and
[wheel build](https://github.com/MiniJe/picovolt/actions/runs/34265044654) apply to
the exact engine revision above. Existing CI checks do not constitute completion
of the deferred independent security review.

The [RC3 benchmark report](../benchmarks/COMPETITORS_2_0_RC3.md) preserves raw
measurements, artifact hashes, timing ranges, resource costs and competitor
losses. Default keyed bulk loading improved about **91x at 10,000 rows**, with
about **99% less measured CPU**, while whole-process peak memory rose about 5%.
SQLite and DuckDB still load that workload faster. Smaller ordinary-workload
improvements may include host variation; no universal superiority is claimed.

The [fresh independent follow-up prompt](INDEPENDENT_RC3_FOLLOWUP_PROMPT.md)
pins RC3 and keeps the deferred review gates explicit. That follow-up has not
been performed by the implementation agent.

## Deferred work and publication checks

- Independent security review remains deferred. This correctness/performance
  follow-up does not complete that review.
- Genuine external owner trials and real legacy database migrations are deferred and still need
  provenance and recorded acceptance. Generated demos and third-party package
  compatibility probes do not establish owner adoption.
- SQLite ecosystem compatibility is partial: the independent probes found
  missing PRAGMA/transaction-state hooks, Python UDF registration and composite
  primary keys. Do not advertise drop-in Datasette/sqlite-utils/beets support.
- Exact 2.0.0 registry installation, release provenance and publication must be
  verified through the release workflow. Prepared starters now pin 2.0.0 and
  become installable when those exact packages are published.

The maintainer's explicit decision authorizes stable publication with the
documented deferrals; no universal competitor-win claim is made.
See [the interface guide](QUICKSTART_2_0.md) and
[format-7 upgrade contract](FORMAT.md#format-7-acknowledged-commit-sequence-anchor).
