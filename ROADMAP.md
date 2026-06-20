# Roadmap

Where PicoVolt is going. This is a direction, not a contract — priorities shift
with what people actually hit. Dates are deliberately omitted; items are grouped
by horizon and mapped to the release tiers in [RELEASING.md](RELEASING.md).

PicoVolt is experimental and pre-1.0. The open core stays Apache-2.0 **forever**;
the commercial edition (below) is additive — new modules in a separate crate, not
a future enclosure of what's already open.

## Shipped (0.1.0)

The engine is built out: VLE dev/prod/in-memory backends, page-backed storage
with O(1) appends, a bounded buffer pool, MVCC time-travel, CAS dedup, columnar
compression, equality indexes, selectable durability, the WASM extension sandbox,
a small SQL front-end, and the wasm/npm bindings. See [CHANGELOG.md](CHANGELOG.md).

## Next — open core (0.2.x and on)

Backward-compatible features that close the gaps the README is honest about
("no range/ordered indexes and no concurrency") and make the SQL front-end less
of a toy. Each is a **Normal** (minor) release.

- **Richer SQL predicates.** Comparison operators (`<`, `>`, `<=`, `>=`, `!=`),
  `AND`/`OR` in `WHERE`, and `LIKE` prefix matching — today `WHERE` is a single
  `col = value`.
- **Aggregates and grouping.** `SUM` / `MIN` / `MAX` / `AVG` alongside the
  existing `COUNT(*)`, then `GROUP BY`.
- **Range / ordered indexes.** An ordered index structure so `WHERE col > v` and
  `ORDER BY col` can use an index instead of a full scan + sort.
- **Background columnar compaction.** Promote the on-demand row→columnar
  transposition ([`storage/page.rs`](src/storage/page.rs)) to a background worker,
  as the original design intended.
- **Streaming query results.** An iterator API so large `SELECT`s don't have to
  materialize every row up front.
- **Better parse diagnostics.** Error messages that point at the offending token
  position, not just a description.

## Toward 1.0 — stability

1.0 is a promise that the public API and the `.pvdb` on-disk format are stable.
Before cutting it:

- **Freeze the on-disk format** behind a versioned header with a documented
  migration path, so future readers can open old files.
- **Stabilize the extension contract** — the crate-root re-exports documented in
  [docs/EXTENDING.md](docs/EXTENDING.md).
- **Durability, precisely specified.** Document exactly what `Fast` vs `Sync`
  guarantee, with crash-injection tests behind the claims.
- **Longer fuzz soak + external review.** The decoders are fuzzed in CI; 1.0 wants
  sustained fuzzing and an independent security pass (see [SECURITY.md](SECURITY.md)).

## picovolt-pro — commercial edition

The plan is an open-core model: a separate, proprietary `picovolt-pro` crate that
depends on the open core through its public API ([docs/EXTENDING.md](docs/EXTENDING.md))
and adds the capabilities teams pay for. It exists only once there's real demand —
it is **not** being built ahead of traction, and nothing here is removed from the
free core to create it. Candidate modules, roughly in order of usefulness:

- **Encryption at rest** — page-level encryption for `.pvdb` and dev workspaces.
- **Replication / change-data-capture** — a commit-stream observer feeding
  followers or an external sink.
- **Server mode** — a network layer and wire protocol for client/server use,
  beyond the embedded library.
- **Advanced indexing & parallel query** — composite and full-text indexes,
  multi-threaded scans.
- **Observability** — metrics, query tracing, and an admin UI.
- **Managed/hosted option** and priority support with an SLA.

## Out of scope (for now)

Saying no keeps the core coherent:

- Not aiming to be a drop-in SQL-92 / PostgreSQL-compatible database.
- No distributed consensus or multi-node clustering in the open core.
- `pv-wasm` stays an integer-subset interpreter; floats/SIMD/tables remain the
  `wasmi` backend's job, not a reimplementation.

## Influencing it

The ordering above is a starting point. Open an issue describing the problem you
have (not just the feature you want) — concrete use cases are what move items up.
Bugs and the "Next" list are the best places to contribute; see
[CONTRIBUTING.md](CONTRIBUTING.md).
