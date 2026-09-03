# Roadmap to one million lifetime downloads

The goal is one million cumulative installs across the official PicoVolt
distribution channels. Downloads are an outcome of useful software that is easy
to discover, evaluate, install, and keep—not a reason to inflate package counts
or manufacture traffic.

## Baseline and definition

Snapshot taken 2026-09-03 from the public registry APIs:

| Channel | Lifetime downloads | State |
|---|---:|---|
| npm `picovolt` | 2,563 | Published at 1.3.0 |
| crates.io `picovolt` | 279 | Published at 1.3.0 |
| PyPI `picovolt` | 0 | Not published yet |
| **Known total** | **2,842** | GitHub release assets excluded |

Sources: [npm download API](https://api.npmjs.org/downloads/point/2026-06-22:2026-09-03/picovolt),
[crates.io API](https://crates.io/api/v1/crates/picovolt), and
[GitHub repository API](https://api.github.com/repos/MiniJe/picovolt).

Count registry package downloads and GitHub binary-asset downloads separately,
then publish both the per-channel values and their sum. Do not count website
visits, source clones, CI artifacts, or repeated mirrors as downloads. Registry
numbers include bots and CI, so dependents and successful onboarding are the
health metrics that stop the headline number becoming vanity.

At the current baseline, the remaining gap is 997,158 downloads. A two-year path
needs roughly 41,550 downloads per month on average; a three-year path needs
roughly 27,700. Because growth starts much lower, the end-of-period run rate must
be higher than either average.

## Product position

PicoVolt should own one memorable sentence:

> Ship a versioned, read-only SQL database as one file, query it locally in Rust,
> Python, Go, or the browser, and rewind every result through MVCC history.

That is more differentiated than “another embedded SQL database.” The best
acquisition loop is a developer discovering a live browser demo, exporting or
downloading the dataset, and installing the same engine in their own language.

## Priority order

### P0 — Make every promised install work

These are release blockers, not optional marketing work.

1. Publish the already-built Python wheels to PyPI using the trusted-publisher
   workflow. Build portable manylinux and musllinux wheels plus macOS and Windows
   x86-64/arm64 variants. Keep one PicoVolt version across Cargo, npm, Python, and
   Git tags.
2. Attach checksummed `pv` and `picovolt-server` binaries to every GitHub release
   for Linux, macOS, and Windows. Add signed build provenance and an SBOM.
3. Turn the REPL example into an installed `pv` CLI with `query`, `inspect`,
   `import`, `export`, `bake`, and `serve` commands. A useful CLI creates direct
   downloads and makes every tutorial copy-pasteable.
4. Add release smoke tests that install from the public registries into clean
   environments. A workflow that silently skipped a registry must fail visibly.
5. Publish a support matrix for OS, CPU, runtime, and file-format compatibility.

Exit criterion: one version tag produces installable Cargo, npm, PyPI, and GitHub
artifacts, and clean-room smoke tests execute a first query through each.

### P1 — Cut time-to-first-value below five minutes

1. Add import/export commands for CSV, newline-delimited JSON, SQLite dumps, and
   Parquet. Data movement unlocks real trials faster than more SQL syntax alone.
2. Ship four maintained starter projects: browser + Vite, Node, Python notebook,
   and Rust CLI. Each should fetch a real `.pvdb`, execute a query, and demonstrate
   `BEFORE tx` time travel.
3. Publish a small, reproducible benchmark suite against SQLite and DuckDB for the
   workloads PicoVolt actually targets. State losing cases as clearly as wins.
4. Make error messages link to concise troubleshooting pages for unsupported SQL,
   native-library loading, WASM MIME types, and HTTP range support.

Exit criterion: a new user can install, load sample data, query it, and export a
result without reading engine internals.

### P2 — Close adoption-blocking compatibility gaps

Implement in this order:

1. Explicit `BEGIN`, `COMMIT`, and `ROLLBACK` across every binding.
2. `INNER JOIN` and `LEFT JOIN`, first with index-assisted nested loops and then a
   hash join where measurement justifies it.
3. Primary-key and unique constraints with clear conflict errors.
4. A prepared-statement object rather than only placeholder substitution.
5. `OFFSET`, `CASE`, common scalar functions, and richer schema declarations.

These features should be driven by failing compatibility tests copied from the
starter applications. Broad SQL surface area that no onboarding path needs stays
behind the items above.

### P3 — Strengthen the browser/local-first moat

1. Add an OPFS-backed durable browser store and a Web Worker API so queries never
   freeze the UI.
2. Make range-streamed `.pvdb` hosting turnkey: a static-hosting guide, header
   checker, service-worker cache, and sample deployment.
3. Add incremental history export and a documented synchronization seam. Avoid a
   full distributed database until a concrete application proves the need.
4. Turn the Rewind demo into a reusable component and an embeddable playground
   that documentation sites can load with their own datasets.

### P4 — Build ecosystem pull

1. Maintain adapters for popular interfaces rather than one-off language wrappers:
   Python DB-API, Go `database/sql`, Node/Bun/Deno, and a documented C ABI.
2. Add at least three end-to-end integrations where PicoVolt is the storage layer:
   a static analytics portal, an offline-first desktop app, and a versioned catalog.
3. Create a “built with PicoVolt” gallery and promote dependent projects. A healthy
   dependent count is a better predictor of durable downloads than release churn.
4. Offer a monthly release train with upgrade notes and file-format compatibility
   tests. Never ship empty versions merely to increase download traffic.

## Milestones

| Horizon | Cumulative target | Leading indicators |
|---|---:|---|
| 30 days | 10,000 | PyPI live; all release channels green; `pv` CLI specification accepted |
| 90 days | 50,000 | Four starters; two import formats; 20 public dependents |
| 6 months | 150,000 | Transactions and basic JOINs; 10,000 weekly downloads |
| 12 months | 400,000 | OPFS/Worker browser path; 25,000 weekly downloads; 100 dependents |
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

1. Configure the `pypi` GitHub environment and PyPI pending trusted publisher for
   `MiniJe/picovolt`, workflow `python-wheels.yml`, environment `pypi`; configure
   npm's trusted publisher for `release.yml` and then revoke its long-lived token.
2. Replace the current Linux Python wheel with manylinux/musllinux builds and add
   arm64 coverage; run public-install smoke tests after publishing.
3. Extend the shipped `pv` CLI with Parquet and binary SQLite import plus
   time-travel diffs.
4. Run the new native-binary, SHA-256, provenance, and CycloneDX release jobs on
   the next version tag and verify every attached artifact.
5. Publish the four shipped starter projects as standalone templates and choose a
   maintained browser demo as the primary homepage call to action.
