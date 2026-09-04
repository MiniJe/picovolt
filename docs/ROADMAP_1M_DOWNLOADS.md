# Roadmap to one million lifetime downloads

The goal is one million cumulative installs across the official PicoVolt
distribution channels. Downloads are an outcome of useful software that is easy
to discover, evaluate, install, and keep—not a reason to inflate package counts
or manufacture traffic.

## Baseline and definition

Snapshot taken 2026-09-04 from the public registry APIs:

| Channel | Lifetime downloads | State |
|---|---:|---|
| npm `picovolt` | 2,563 | Current release: 1.7.0 |
| crates.io `picovolt` | 315 | Current release: 1.7.0 |
| PyPI `picovolt` | Not publicly reported | Current release: 1.7.0 |
| **Known registry total** | **At least 2,878** | PyPI and GitHub release assets excluded |

Sources: [npm download API](https://api.npmjs.org/downloads/point/2026-06-22:2026-09-04/picovolt),
[crates.io API](https://crates.io/api/v1/crates/picovolt), and
[GitHub release API](https://api.github.com/repos/MiniJe/picovolt/releases/tags/v1.7.0).
The v1.7.0 GitHub Release contained 13 downloadable assets and recorded zero
asset downloads at this snapshot; release-asset counts remain a separate series.

Count registry package downloads and GitHub binary-asset downloads separately,
then publish both the per-channel values and their sum. Do not count website
visits, source clones, CI artifacts, or repeated mirrors as downloads. Registry
numbers include bots and CI, so dependents and successful onboarding are the
health metrics that stop the headline number becoming vanity.

At the current confirmed baseline, the remaining measured gap is no more than
997,122 downloads because PyPI installs are not represented in the total. A
two-year path needs roughly 41,550 measured downloads per month on average; a
three-year path needs roughly 27,700. Because growth starts much lower, the
end-of-period run rate must be higher than either average.

## Product position

PicoVolt should own one memorable sentence:

> Ship a versioned, read-only SQL database as one file, query it locally in Rust,
> Python, Go, or the browser, and rewind every result through MVCC history.

That is more differentiated than “another embedded SQL database.” The best
acquisition loop is a developer discovering a live browser demo, exporting or
downloading the dataset, and installing the same engine in their own language.

## Priority order

### P0 — Make every promised install work (complete for 1.7)

These are release blockers, not optional marketing work.

Delivered in 1.7.0:

1. One version is published through crates.io, npm, PyPI, the Go module tag, and
   GitHub Releases; npm and PyPI use trusted publishers.
2. GitHub Releases contain checksummed `pv` and `picovolt-server` binaries plus C
   ABI bundles, SBOMs, and build provenance for Linux, macOS, and Windows.
3. The installed `pv` CLI provides `query`, `inspect`, `history`, `import`,
   `export`, and `bake`; server mode remains the separate
   `picovolt-server` executable.
4. Clean-room release gates install the exact Cargo, npm, PyPI, and Go versions
   from public registries and fail when a package or matching native library is
   absent.
5. [`SUPPORT.md`](SUPPORT.md) records the tested runtime, platform, browser, and
   file-format combinations.

Exit criterion: **met by v1.7.0**. One version tag produced installable Cargo,
npm, PyPI, Go, C ABI, and GitHub artifacts, and clean-room smoke tests executed a
first query through every maintained package surface.

### P1 — Cut time-to-first-value below five minutes

1. CSV and newline-delimited JSON import/export plus SQLite SQL-dump import are
   shipped. Version 1.8 adds Parquet and direct binary SQLite movement, with
   [documented conversions and limits](DATA_TOOLS.md), to support real-data trials.
2. Five maintained starter projects—browser + Vite, Node, Python, Go, and
   Rust CLI—now run clean first queries against public packages. Dataset-backed
   `BEFORE tx` walkthroughs remain an adoption task.
3. Publish a small, reproducible benchmark suite against SQLite and DuckDB for the
   workloads PicoVolt actually targets. State losing cases as clearly as wins.
4. Make error messages link to concise troubleshooting pages for unsupported SQL,
   native-library loading, WASM MIME types, and HTTP range support.

Exit criterion: a new user can install, load sample data, query it, and export a
result without reading engine internals.

### P2 — Close adoption-blocking compatibility gaps

Delivered in this order:

1. Explicit `BEGIN`, `COMMIT`, and `ROLLBACK` across every binding. **Shipped in
   1.6.0.**
2. N-table equality `INNER JOIN` and `LEFT JOIN`, aliases, self-joins, and joined
   grouping. **Shipped in 1.7.0; adaptive indexed right-side probes are complete
   for 1.9.0.**
3. Primary-key, unique, not-null, default, and check constraints with atomic,
   actionable failures. **Shipped through 1.7.0.**
4. Reusable prepared-statement objects in every maintained binding. **Shipped
   in 1.7.0.**
5. `OFFSET`, searched `CASE`, common scalar functions, named-column inserts, and
   schema-light type declarations. **Shipped through 1.7.0.**

These features should be driven by failing compatibility tests copied from the
starter applications. Broad SQL surface area that no onboarding path needs stays
behind the items above.

### P3 — Strengthen the browser/local-first moat

1. The OPFS-backed durable browser store and Web Worker API shipped in 1.4.0 and
   are covered by the registry-only browser starter gate.
2. Make range-streamed `.pvdb` hosting turnkey: a static-hosting guide, header
   checker, service-worker cache, and sample deployment.
3. Add incremental history export and a documented synchronization seam. Avoid a
   full distributed database until a concrete application proves the need.
4. Turn the Rewind demo into a reusable component and an embeddable playground
   that documentation sites can load with their own datasets.

### P4 — Build ecosystem pull

1. Maintain adapters for popular interfaces rather than one-off language wrappers:
   Python DB-API, Go `database/sql`, Node, and a documented C ABI are shipped;
   explicit Bun/Deno compatibility remains open.
2. Add at least three end-to-end integrations where PicoVolt is the storage layer:
   a static analytics portal, an offline-first desktop app, and a versioned catalog.
3. Create a “built with PicoVolt” gallery and promote dependent projects. A healthy
   dependent count is a better predictor of durable downloads than release churn.
4. Offer a monthly release train with upgrade notes and file-format compatibility
   tests. Never ship empty versions merely to increase download traffic.

## Milestones

| Horizon | Cumulative target | Leading indicators |
|---|---:|---|
| 30 days | 10,000 | All release channels green; three external starter completions |
| 90 days | 50,000 | Five standalone starter templates; 20 public dependents |
| 6 months | 150,000 | Three external 1.7 applications; 10,000 weekly downloads |
| 12 months | 400,000 | 1.8 data tooling in regular use; 25,000 weekly downloads; 100 dependents |
| 24 months | 1,000,000 | 50,000+ weekly downloads sustained across multiple channels |

Review the targets monthly. If downloads rise but dependents and issue engagement
do not, investigate CI/bot traffic rather than treating it as product growth.

## Measurement

Create a public dashboard updated weekly from registry and GitHub APIs. Track:

- cumulative and weekly downloads per channel;
- new public dependent repositories and repeat package consumers;
- starter-project completion checks and documentation search failures;
- release success rate, install smoke-test failures, and time to patch advisories;
- issue-to-fix time and the percentage of releases with migration notes.

Do not add runtime telemetry to the database by default. Public registry data,
opt-in surveys, documentation analytics, and repository activity are sufficient
until users explicitly ask for product telemetry.

## Immediate next sprint

1. Run the 1.9 release-candidate migration and compatibility trial against real
   1.x databases, retaining verification hashes and rollback results.
2. Recruit real-data trials for the 1.8 Parquet, SQLite, and snapshot-diff tools
   plus the 1.9 compaction path; prioritize missing conversions from concrete
   datasets.
3. Publish the 1.9 benchmark JSON and compare it with SQLite and DuckDB only on
   workloads the projects genuinely share.
4. Publish the starters as standalone templates and measure clean installs and
   completed first queries.
5. Start the public weekly adoption dashboard and recruit the first three
   applications for a 1.7 compatibility trial.
6. Validate the commercial-support and PicoVolt Hub hypotheses described in
   [MONETIZATION.md](MONETIZATION.md) using the host-owned integration seams in
   [ENTERPRISE.md](ENTERPRISE.md), without gating the open-source engine.
