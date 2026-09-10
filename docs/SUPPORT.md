# Support matrix

## Maintenance transition — 2026-09-10

Voluntary feature work and routine maintenance for published Apache release
lines through 2.0.0 have ended, subject to existing agreements and mandatory
obligations. No supported proprietary successor is available yet. Future
proprietary support requires the applicable license and a separate agreement
where paid support is offered; accepting terms alone promises no SLA.

A 30-day migration assistance window begins only when that successor and its
migration guide are available. Dates will be announced then. Existing Apache
copies do not expire, require registration, send activity reports or lose data
access. Historical export tools and compatibility fixtures remain available.
See [the complete transition policy](../legal/TRANSITION.md).

The matrix below records the existing release coverage, not an ongoing support
commitment. Internal website runtime maintenance has a separate operational
policy and does not create a public release support promise.

This page states what PicoVolt's automated release and compatibility checks
cover. A check mark means CI builds and tests that combination on every release;
it is not a promise that every downstream environment is identical.

| Distribution | Supported runtime / platform | Release artifact |
|---|---|---|
| Rust crate and `pv` CLI | Rust 1.86+; release binaries for Linux x86-64, macOS arm64, and Windows x86-64 | crates.io plus native GitHub downloads |
| JavaScript | Node.js 22.12+ on a maintained release line; modern evergreen browsers with WebAssembly | npm `picovolt` |
| Python | CPython 3.9+ runtime compatibility floor (use a Python-supported release in production; release tooling builds on 3.10+); Linux x86-64 (manylinux 2.28), macOS universal2, and Windows x86-64 | platform wheels on PyPI |
| Go | Go 1.26 and 1.27 with cgo; the clean-room native gate runs on Linux x86-64 | versioned Go module plus the matching C ABI library |
| C ABI | Release bundles for Linux x86-64, macOS arm64, and Windows x86-64; other Rust-supported targets can build locally | header and shared library in GitHub release bundles |

Browser persistence uses OPFS and therefore needs a secure `http://localhost` or
`https://` origin. Opening the starter through `file://` is unsupported because
browsers isolate local-file origins; run `npm ci && npm run dev` in
`starters/browser` instead.

## File compatibility

- PicoVolt 1.x readers continue to read older 1.x database images unless a
  release note explicitly documents an exceptional migration.
- Images containing packed cold pages, a compaction cursor, or an explicit 1.9
  migration use format version 5. Literal defaults or `CHECK` constraints use
  version 4; images with only `PRIMARY KEY`/`UNIQUE`/`NOT NULL` remain version
  3. Every earlier format remains covered by checked-in golden fixtures.
- Newer-format images fail with a clear unsupported-version error in older
  readers; they are never silently interpreted as an older layout.

## Known limits

- The engine has one writer. The HTTP server serializes queries through that
  writer rather than providing multi-writer transactions.
- Explicit `BEGIN`/`COMMIT`/`ROLLBACK` works for in-memory and filesystem
  databases across the maintained bindings. Filesystem transactions currently
  create a complete rollback image, so their start cost scales with workspace
  size. Compound autocommit statements use that same safety boundary after
  validation. The 2.0 shared Rust API additionally provides snapshot readers,
  writer admission and an incremental commit journal; see
  [the concurrency contract](CONCURRENCY.md). Savepoints are not implemented.
- A mutation-phase I/O failure aborts an explicit transaction so a partially
  applied statement cannot later be committed. Validation and resource-limit
  failures happen before mutation and leave the transaction open.
- A live filesystem transaction holds an OS-level transaction lock. Another
  opener fails instead of treating the live marker as crash residue.
- SQL intentionally covers a practical embedded subset, not full SQL-92.
