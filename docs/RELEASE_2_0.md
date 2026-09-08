# PicoVolt 2.0 release ledger

Current candidate: **2.0.0-rc.3**, unpublished. Stable release remains gated.
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

RC3 validation and benchmark results are recorded below once runs complete.
Benchmarks preserve the RC2 binaries and independent evidence. Keyed bulk loads
use each engine's bulk API, enforce identical primary keys, verify all rows after
reopen and report CPU, memory and storage. Ordinary query benchmarks retain the
existing common SQL harness and durability/retention settings.

## Remaining stable-release gates

- Independent security review remains deferred. This correctness/performance
  follow-up does not complete that review.
- Genuine external owner trials and real legacy database migrations still need
  provenance and recorded acceptance. Generated demos and third-party package
  compatibility probes do not establish owner adoption.
- SQLite ecosystem compatibility is partial: the independent probes found
  missing PRAGMA/transaction-state hooks, Python UDF registration and composite
  primary keys. Do not advertise drop-in Datasette/sqlite-utils/beets support.
- Exact RC3 registry installation, release provenance and publication must be
  verified through the release workflow. Published starters remain pinned to
  the genuine 1.9.0 baseline until publication; local RC3 builds are separate.

No stable tag, registry publication or universal competitor-win claim follows
from these local repairs. See [the interface guide](QUICKSTART_2_0.md) and
[format-7 upgrade contract](FORMAT.md#format-7-acknowledged-commit-sequence-anchor).
