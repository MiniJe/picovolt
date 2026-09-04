# Migration and compaction

PicoVolt 1.9 separates two jobs that are easy to confuse:

- `pv migrate` upgrades a baked image without discarding any MVCC history.
- `pv compact` transposes eligible pages in a writable development workspace.

Neither command modifies its input unexpectedly. Migration is always
out-of-place; compaction is an explicit in-place workspace maintenance step.

## Upgrade a baked image

Start with a dry run. It fully opens the image, walks every table and record
version, resolves CAS values, decodes persisted indexes, and computes a
deterministic verification hash. It creates no files.

```sh
pv migrate app-v4.pvdb app-v5.pvdb --dry-run
```

Run the migration with an exact backup when the source is important:

```sh
pv migrate app-v4.pvdb app-v5.pvdb --backup app-v4.backup.pvdb
```

The command performs these operations in order:

1. validate that the source is a baked file and that destination and backup do
   not already exist;
2. copy and sync the optional byte-for-byte backup;
3. write a same-directory temporary image at the latest format version;
4. reopen the temporary image and compare its transaction clock, catalog,
   record-version count, indexes, and full-history verification hash;
5. publish with a no-clobber rename.

If any step fails, the source stays untouched and no destination is published.
An already-created backup is retained. Roll back by deleting the new destination
and continuing to open the original source (or its exact backup).

Migration accepts baked `.pvdb` files. To publish a development workspace, use
`pv bake <workspace> <image.pvdb>` first. The Rust API exposes the same workflow
as `plan_file_migration` and `migrate_file`.

## Compact a development workspace

```sh
pv compact ./app.pv --max-pages 64
```

Each call attempts at most the requested number of non-tail page positions after
its persisted resume cursor. This bounds the encoding and rewrite slice, not all
I/O: chain-integrity validation may inspect additional pages, and the 1.x
transaction protocol copies the complete workspace. A page is converted only
when all of the following hold:

- it is not the mutable tail;
- the columnar representation fits in one physical page;
- the encoded representation is smaller than the used row representation.

The pass is one crash-recoverable transaction: its format marker, page
replacements, and catalog counts commit together, or the previous workspace is
restored on reopen. The current 1.x transaction protocol keeps a complete
rollback image, so budget roughly one additional workspace's worth of temporary
disk during compaction. Compaction cannot run inside another transaction.

The integrated cold layout retains every 24-byte MVCC envelope, page-chain link,
and slot ordinal. Secondary-index addresses therefore remain valid, historical
queries still work, and an update can tombstone a cold record before appending
its replacement to the hot tail. Decimal columns use canonical zig-zag `i128`
LEB128 rather than a fixed 16-byte mantissa.

PicoVolt 1.x does not start a background thread. A service that wants ongoing
maintenance should call `Database::compact_step` from its existing owner or run
bounded `pv compact` jobs through its scheduler. This keeps the single-owner
contract honest; automatic concurrent maintenance belongs to the 2.0 handle and
scheduler design.

Use `pv inspect` to check the result. `compressed_pages` reports the number of
integrated cold pages and `saved_bytes` reports encoded payload savings inside
the fixed 4096-byte pages.

## Version compatibility

A 1.9 reader opens versions 1 through 5. Images remain at their minimum required
version unless they contain cold pages or were explicitly migrated. A version-5
image must be read with PicoVolt 1.9 or newer. Keep the old source until every
deployed reader has been upgraded and has successfully opened the new image.
