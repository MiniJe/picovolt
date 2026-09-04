# Data tools

Install the full native CLI with `cargo install picovolt --features data-tools`,
or use a release binary. Parquet, SQLite, and signing are optional native CLI
dependencies. `EXPLAIN`, inspection, diff, migration, compaction, and baking also
work in the default build. Version 5 is introduced by 1.9 cold pages; see
[Migration and compaction](MIGRATION.md).

## Import and export

```sh
pv import ./workspace source.sqlite --format sqlite --source-table customers --table customers
pv import ./workspace measurements.parquet --table measurements
pv export ./workspace customers customers.parquet
pv export ./workspace customers earlier.parquet --before 42
```

Binary imports create a new table in one transaction. Existing destination
tables are rejected; conversion failure rolls back the entire import. The
filesystem transaction backup needs space proportional to the existing
workspace, so use a new staging workspace for large imports. SQLite is opened
read-only in a read transaction. Names are safely quoted for SQLite and passed
directly to PicoVolt's programmatic API.

SQLite import copies ordinary table rows and column names. Constraints, indexes,
triggers, views, virtual tables, and generated behavior are not migrated.
Integer, UTF-8 text, blob, and NULL map directly. REAL values round to six
fractional digits with overflow checks; NaN/infinity are rejected.

Parquet accepts flat Boolean, signed/unsigned integers within i64, Float32/64,
Decimal128, UTF-8, binary/fixed binary, and nullable columns. Boolean becomes
integer 0/1. Floats use the conversion above; Decimal128 rescaling must be exact
to six fractional digits. Unsupported nested, date/time, dictionary Arrow, and
other logical types fail explicitly, including in empty files. Supported
compression readers include uncompressed, Snappy, Zstandard, and gzip.

Export scans once to infer types, then writes batches of 1024 rows. Mixed
integer/decimal columns become Decimal128(38, 6); incompatible mixed types and
oversized decimals fail. All-null columns become nullable UTF-8. Encoding must
succeed before the destination is atomically replaced. Batches and page reads
are bounded, but large individual fields, decoder metadata, CAS, and indexes
still consume memory outside the page cache.

## Explain and inspect

```sh
pv explain ./workspace "SELECT * FROM customers WHERE id = 12"
pv query ./workspace "EXPLAIN SELECT * FROM customers ORDER BY id LIMIT 10"
pv inspect ./workspace --json
pv history ./workspace --table customers
pv diff ./workspace customers --from 42 --to 57 --format jsonl
```

`EXPLAIN` describes the physical path without reading rows. It reports snapshots,
scans, index lookups/ranges, ordered scans, adaptive indexed equality joins,
filtering, aggregates, sorting, projection, DISTINCT, OFFSET, and LIMIT. It
provides no timing estimate. Numeric ranges currently scan to preserve mixed
numeric semantics. Bounded query execution also falls back to scans for ranges;
`query_with_limits("EXPLAIN ...", ...)` reflects that path. Row-dependent
expression errors may still occur during execution. Rust exposes
`Database::explain` and `Database::inspect_stats`.

Inspection reports format requirements, storage mode, page allocation/orphans,
live rows/versions, free/used bytes, CAS size, index entries/keys/encoded bytes,
and cache counters. It scans headers and envelopes. Compression reports
`row-slotted`, `columnar-packed`, or a mixed layout, plus actual cold-page and
encoded-byte savings. Compaction is explicit and cooperative in 1.x.

Diff compares complete rows with duplicate multiplicity. Updates appear as
removals and additions, with all removals first. It is a net snapshot comparison,
not an event or replication log. Both snapshots currently materialize in memory.
CSV starts with `_change`; JSONL nests values under `row`. Use transaction IDs
from inspection/history rather than counting SQL commands. CSV cannot distinguish
NULL from empty text; JSONL represents NULL explicitly.

## Bake and resume

```sh
pv bake ./workspace dataset.pvdb --resume
```

Keep the source quiescent until baking finishes. Pages stream sequentially with
MVCC versions and indexes preserved. Existing row and packed cold-page encodings
are retained.
Normal baking writes a temporary file, syncs it, then replaces the destination.
Rust's `bake_to_writer` lets applications manage their own output publication.

With `--resume`, progress is the byte prefix in `dataset.pvdb.partial`. Rerun the
same command after interruption. Existing bytes are compared with the source
stream before reuse; the remainder is appended, synced, and renamed. A mismatch
retains the partial and preserves the previous destination. Choose a new output
path when the source changes. Resume avoids rewriting verified bytes but still
reads them. Allow disk space for the workspace, partial, existing destination,
and transaction backups. CAS and indexes remain resident; the page region needs
no whole-image allocation.

## Sign and verify

```sh
pv dataset keygen publisher.secret
# Save the printed PUBLIC key in consumer configuration.
pv dataset sign dataset.pvdb --key publisher.secret --name customers-2026-09 --output dataset.manifest.json
pv dataset verify dataset.pvdb dataset.manifest.json --public-key TRUSTED_PUBLIC_KEY_HEX
```

Private keys contain 32 raw bytes from the operating system random generator.
Key generation never overwrites a file. Unix keys use mode 0600; Windows users
should use an access-controlled directory. Keep private keys out of source
control. Signing also refuses to overwrite a manifest; use versioned names.

Schema 1 signs domain-separated canonical JSON with Ed25519. It covers dataset
name, exact file size, BLAKE3 hash, and format version. Verification strictly
checks the signature against an independently configured public key and hashes
the artifact. The embedded public key is informational, never a trust anchor.
No metadata paths or URLs are fetched. Keep files unchanged between verification
and use. Signatures authenticate bytes, not semantic correctness; normal
open/query validation still applies. Consumers must enforce the expected dataset
identity/version to prevent replay of an older signed artifact.

## Validation

```sh
cargo test --locked --features data-tools --test data_movement
cargo test --locked --test data_inspection
```

The [public Iris fixture](../tests/fixtures/DATASETS.md) runs through SQLite
differential checks, Parquet, and `.pvdb` round trips with a two-page cache.
Further tests cover 2500 rows with NULL/text/blob/decimal fields, rollback,
empty schemas, snapshot export, corrupt partials, and signature/key tampering.
