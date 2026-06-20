# Roadmap

This describes the direction of the project, not a commitment. Priorities shift
with what users need, and dates are deliberately omitted. Items are grouped by
horizon. Changes that have landed are recorded in [CHANGELOG.md](CHANGELOG.md).

PicoVolt is experimental and pre-1.0, so the public API and on-disk format may
still change between minor versions.

## Shipped in 0.1.0

The core engine: VLE development, production, and in-memory backends; page-backed
storage with O(1) appends; a bounded buffer pool; MVCC time-travel; CAS dedup;
columnar compression; secondary indexes; selectable durability; the WebAssembly
extension sandbox; an SQL front-end; and the WebAssembly and npm bindings.

## Recently added (on main)

- **Richer WHERE predicates:** comparison operators (`<`, `<=`, `>`, `>=`, `!=`,
  `<>`), `AND` and `OR` with parentheses, and `LIKE` (`%` and `_`) for `SELECT`,
  `UPDATE`, and `DELETE`.
- **Whole-table aggregates:** `COUNT`, `SUM`, `MIN`, and `MAX`, over the full or
  `WHERE`-filtered result.
- **Ordered, range-capable secondary indexes:** `CREATE INDEX` builds a
  `BTreeMap`-backed index. Range predicates such as `col > v` use it for an
  ordered scan instead of a full scan, alongside the existing point lookups.
- **Index-accelerated `ORDER BY`:** a `SELECT ... ORDER BY indexed_col` with no
  `WHERE` reads the index in key order and skips the sort, and a `LIMIT` stops the
  read early.

## Next

- **`GROUP BY` and `AVG`:** grouped aggregation, and `AVG` once there is a value
  type that can hold a fraction (today `Value` is integer, text, or blob only).
- **Persisted indexes:** indexes are currently rebuilt by a scan on open.
  Persisting them in the `.pvdb` file and workspace would let large tables open
  quickly.
- **Background columnar compaction:** promote the on-demand row-to-columnar
  transposition ([`storage/page.rs`](src/storage/page.rs)) to a background worker.
- **Streaming query results:** an iterator API so large `SELECT`s do not have to
  materialize every row up front.
- **Better parse diagnostics:** error messages that point at the offending token
  position rather than only describing the problem.

## Toward 1.0

Version 1.0 is the point at which the public API and the `.pvdb` on-disk format
are considered stable. Before that:

- **Freeze the on-disk format** behind a versioned header with a documented
  migration path, so future readers can open files written by older versions.
- **Stabilize the extension contract,** the crate-root re-exports documented in
  [docs/EXTENDING.md](docs/EXTENDING.md).
- **Specify durability precisely:** document exactly what `Fast` and `Sync`
  guarantee, with crash-injection tests behind the claims.
- **Longer fuzzing and external review:** the decoders are fuzzed in CI. Version
  1.0 calls for sustained fuzzing and an independent security pass (see
  [SECURITY.md](SECURITY.md)).

## Out of scope

These keep the project focused:

- It is not aiming to be a drop-in SQL-92 or PostgreSQL-compatible database.
- No distributed consensus or multi-node clustering.
- `pv-wasm` stays an integer-subset interpreter. Floats, SIMD, and tables remain
  the `wasmi` backend's responsibility rather than a reimplementation.

## Suggesting changes

The ordering above is a starting point. To influence it, open an issue describing
the problem you have rather than only the feature you want; concrete use cases are
what move items up the list. See [CONTRIBUTING.md](CONTRIBUTING.md).
