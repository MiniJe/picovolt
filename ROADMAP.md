# Roadmap to PicoVolt 2.0

This roadmap describes intended outcomes, not release dates. A feature moves into
a release only when its correctness work, documentation, compatibility tests, and
upgrade path are ready. Shipped work is recorded in [CHANGELOG.md](CHANGELOG.md).
The adoption plan lives in
[docs/ROADMAP_1M_DOWNLOADS.md](docs/ROADMAP_1M_DOWNLOADS.md), and the business
model hypothesis lives in [docs/MONETIZATION.md](docs/MONETIZATION.md).

## Where PicoVolt is now

Version **1.5.0** is published on crates.io, npm, and PyPI; **1.6.0** is the next
release built on main. The current engine
includes page-backed storage, MVCC time-travel queries, persisted secondary
indexes, a stable 1.x file format, Rust/JavaScript/Python/Go/C bindings, a CLI,
an optional bounded HTTP server, and a durable browser path using OPFS and a Web
Worker.

PicoVolt is still deliberately narrow:

- the filesystem engine is single-writer;
- filesystem transactions currently take a complete rollback image rather than
  writing an incremental commit log;
- SQL is a practical subset, not a compatibility claim;
- the project has extensive automated hardening but no independent security
  audit yet.

Those constraints determine the order below.

## 1.6 — Crash-safe transactions (built on main)

**Outcome:** an application can group filesystem writes and trust that an
interrupted commit is either fully visible or not visible at all.

Delivered scope:

- explicit `BEGIN`, `COMMIT`, and `ROLLBACK` for filesystem workspaces;
- a write-ahead or copy-on-write commit protocol with deterministic recovery;
- transaction parity across persistent Rust, JavaScript, Python, Go, and C
  handles; sessionless CLI and HTTP calls intentionally remain atomic
  single-statement operations;
- documented behavior for nesting, errors, and read-your-writes;
- an optional `enterprise` feature with data-minimizing transaction audit events
  and honest capability discovery for future fleet/control-plane integrations.

Release validation now includes cross-process randomized crash/recovery cycles
that bypass destructors after dirty-page sync, plus automatic opening of every
checked-in historical `.pvdb` image. A longer soak can be run with
`PICOVOLT_CRASH_CYCLES=1000 cargo test --test crash_recovery -- --nocapture`.

## 1.7 — Application compatibility

**Outcome:** common small applications can switch their storage adapter without
rewriting normal query and schema code.

Planned scope:

- table aliases and N-table equality joins;
- `CASE WHEN` and a focused set of string, numeric, and null-handling functions;
- richer schema declarations, including defaults and check constraints;
- reusable prepared-statement parity in every maintained binding;
- compatibility suites derived from the browser, Node, Python, and Go starters;
- actionable errors that name the unsupported construct and its source position.

Release gate: every maintained starter runs only against packages installed from
the public registries, with no source-tree fallback.

## 1.8 — Data movement and inspection

**Outcome:** a team can evaluate PicoVolt on existing data and understand what the
engine stored and why a query used a particular path.

Planned scope:

- Parquet import/export and direct binary SQLite import;
- `pv diff` for row-level changes between MVCC snapshots;
- `EXPLAIN` output for scans, indexes, joins, sorting, and limits;
- manifest, page, index, and compression statistics in `pv inspect`;
- a documented, resumable pipeline for baking large production images;
- signed dataset manifests for distributing `.pvdb` artifacts.

Release gate: round-trip and differential tests over representative public
datasets, including datasets larger than the configured buffer pool.

## 1.9 — Stabilization

**Outcome:** 2.0 begins from measured behavior and a proven migration path rather
than from an API redesign performed in the dark.

Planned scope:

- index-assisted N-table planning and benchmark-driven optimizer work;
- packed decimal encoding and background columnar compaction;
- `pv migrate` with dry-run, backup, verification, and rollback guidance;
- sustained parser/decoder fuzzing and multi-day stress runs;
- performance regression budgets for open, scan, point lookup, top-N, join, bake,
  and recovery workloads;
- a release-candidate period focused on downstream compatibility.

Release gate: no unresolved critical or high-severity findings, 30 days of
release-candidate soak, and successful migration of the complete golden-file
corpus.

## 2.0 — A production concurrency contract

**Outcome:** PicoVolt can be shared safely by multiple application tasks without
forcing callers to build their own ownership thread around the database.

The 2.0 design may break APIs and advance the on-disk format. Its minimum scope is:

- explicit database, read-transaction, and write-transaction handles;
- concurrent snapshot readers with defined writer scheduling and cancellation;
- a crash-recoverable commit log that exposes an ordered change stream;
- backpressure and resource limits as part of the public contract;
- first-party migration tooling from every 1.x format;
- stable extension points for encryption and replication without making a
  particular cloud service part of the engine.

Full distributed consensus, automatic conflict-free multi-device sync, and a
hosted control plane are **not** required for 2.0. They can build on the ordered
change stream later instead of delaying the core concurrency contract.

Release gates:

1. an independent security review of the format, recovery path, FFI boundary,
   and network surface;
2. crash injection, model-based transaction tests, and thread-sanitizer coverage;
3. documented latency and memory envelopes under contention;
4. migration rehearsals on real 1.x databases with verified backups;
5. at least three external applications completing a release-candidate trial.

## Work that runs alongside every release

- Keep one version number across crates.io, npm, PyPI, native artifacts, and tags.
- Publish checksums, SBOMs, provenance, clean-install smoke tests, and migration
  notes for every release.
- Never add runtime telemetry by default.
- Maintain the stable-format promise: later 1.x builds read all earlier 1.x files.
- Measure download growth together with successful installs, dependents, starter
  completions, and issue resolution—not as an isolated vanity number.
- Keep enterprise integration host-owned and data-minimizing. The engine never
  gains mandatory telemetry, an account requirement, or a proprietary format.

## Beyond 2.0

Candidates include encrypted storage, an object-store backend, managed immutable
dataset distribution, change-stream replication, offline sync, full-text and
vector indexes, and additional language adapters. They remain candidates until a
real application and maintainer capacity justify their cost.

## Non-goals

- Drop-in PostgreSQL or complete SQL-92 compatibility.
- Distributed consensus or multi-node clustering in the core engine.
- Reimplementing advanced WebAssembly execution features already handled by the
  optional `wasmi` backend.

To influence priority, open an issue describing the application, current
workaround, failure mode, and smallest useful outcome. Concrete constraints move
work faster than feature names alone.
