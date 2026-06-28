# PicoVolt on-disk format (`FORMAT_VERSION = 2`)

This document specifies the byte-level layout of PicoVolt's persisted data. It is
the reference for the **0.11.0 format freeze**: from this version on, a change to
any structure described here is a format change and must bump
[`FORMAT_VERSION`](../src/core/types.rs) (and, where applicable, ship a migration
path and a new golden fixture under `tests/fixtures/`).

Version history:

- **Version 1** (0.11.0): `header | pages | CAS pool | manifest`.
- **Version 2** (1.3.0): adds a binary secondary-index region between the CAS pool
  and the manifest (§6.1). A file is stamped version 2 **only when it carries that
  region**; an index-less monolith and every development workspace stay at version
  1, so version-1 builds can still read them.

All multi-byte integers are **little-endian**. Sizes are in bytes.

Key constants (`src/core/types.rs`):

| Constant              | Value        | Meaning                                  |
|-----------------------|--------------|------------------------------------------|
| `FORMAT_VERSION`      | `2`          | Newest version this build can read.      |
| `FORMAT_VERSION_BASE` | `1`          | Version written when there is no index region. |
| `PAGE_SIZE`           | `4096`       | One physical page.                       |
| `PAGE_HEADER_SIZE`    | `28`         | Fixed header region at the front of every page. |
| `PAGE_CHECKSUM_OFFSET`| `24`         | Byte offset of the per-page checksum.    |
| `FILE_HEADER_SIZE`    | `24`         | Monolith file header.                    |
| `CHUNK_CAP_BYTES`     | `64 MiB`     | Max size of one dev-workspace chunk file.|
| `PAGES_PER_CHUNK`     | `16384`      | Pages per chunk (`CHUNK_CAP_BYTES/PAGE_SIZE`). |
| `MAGIC_BYTES`         | `50 56 44 42`| ASCII `"PVDB"`.                          |

## 1. Two physical forms

A PicoVolt database exists in one of two shapes:

- **Development workspace** — a directory (conventionally `name.pv/`) holding a
  JSON manifest (`pv_manifest.json`) and an append-only `chunks/` subdirectory of
  page files. Mutable; this is what `open_dev` reads and writes.
- **Baked monolith** — a single, immutable, memory-mappable `.pvdb` file produced
  by `bake`. Read-only; this is what `open_prod` mmaps and what `import_bytes`
  consumes as a byte image.

Both share the same page and record encoding; they differ only in how pages, the
CAS pool, and the manifest are arranged on disk.

## 2. Monolith file layout (`.pvdb`)

```
+-------------------+  offset 0
|   File header     |  FILE_HEADER_SIZE (24)
+-------------------+  offset = FILE_HEADER_SIZE
|   Page-data block |  page_count * PAGE_SIZE
+-------------------+  offset = cas_offset
|   CAS blob pool   |  cas_pool length
+-------------------+  offset = cas_offset + cas_pool length   (v2 only)
|   Index region    |  manifest.index_region length            (v2 only)
+-------------------+  offset = manifest_offset
|   Manifest (JSON) |  to EOF
+-------------------+
```

`cas_offset` and `manifest_offset` are recorded in the file header, so
`page_count = (cas_offset - FILE_HEADER_SIZE) / PAGE_SIZE`. The optional **index
region** (version 2, §6.1) sits between the CAS pool and the manifest; its exact
`(absolute offset, length)` is recorded in the manifest's `index_region` field, so
the CAS pool occupies `[cas_offset, index_region.offset)` and the region occupies
`[index_region.offset, manifest_offset)`. A version-1 file has no region and the
CAS pool runs straight up to `manifest_offset`. On open, all offsets are
bounds-checked against the file length and page alignment; an inconsistent set is
rejected as corruption rather than trusted.

### 2.1 File header (24 bytes)

| Offset | Size | Field            | Notes                                            |
|-------:|-----:|------------------|--------------------------------------------------|
| 0      | 4    | `magic`          | `MAGIC_BYTES` = `"PVDB"`. Mismatch ⇒ `SignatureMismatch`. |
| 4      | 2    | `format_version` | Must be `1..=FORMAT_VERSION` (1 or 2). `0` or newer ⇒ `Corruption`. |
| 6      | 2    | flags            | Reserved, written as zero.                       |
| 8      | 8    | `manifest_offset`| Absolute offset of the JSON manifest.            |
| 16     | 8    | `cas_offset`     | Absolute offset of the CAS blob pool.            |

## 3. Pages

Every page is exactly `PAGE_SIZE` (4096) bytes: a `PAGE_HEADER_SIZE` (28) byte
header region followed by the body. The page kind is the 1-byte discriminant at
offset 8 (`PageType`: `0x01` row, `0x02` columnar); a decoder for one kind
rejects the other.

The **last 4 bytes of the header region** (`[24..28]`) are the per-page integrity
checksum and are common to both kinds (see §3.3). The body begins at offset 28.

### 3.1 Row page header (`Page_Type = 0x01`)

The hot, mutable, slotted page.

| Offset | Size | Field            | Notes                                          |
|-------:|-----:|------------------|------------------------------------------------|
| 0      | 8    | `page_id`        |                                                |
| 8      | 1    | type = `0x01`    | `PageType::Row`.                               |
| 9      | 2    | `slot_count`     | Occupied slot-array entries.                   |
| 11     | 2    | `free_space_ptr` | Top of the downward-growing record store; starts at `PAGE_SIZE`. |
| 13     | 8    | `next_page`      | Next page in the table chain, or `NO_PAGE`.    |
| 21     | 3    | reserved         | Zero.                                          |
| 24     | 4    | `checksum`       | Per-page checksum (§3.3).                       |

### 3.2 Columnar page header (`Page_Type = 0x02`)

The cold, packed, transposed page (column blocks with §4 compression).

| Offset | Size | Field          | Notes                              |
|-------:|-----:|----------------|------------------------------------|
| 0      | 8    | `page_id`      |                                    |
| 8      | 1    | type = `0x02`  | `PageType::Columnar`.              |
| 9      | 2    | `row_count`    | Logical rows across the columns.   |
| 11     | 13   | reserved       | Zero.                              |
| 24     | 4    | `checksum`     | Per-page checksum (§3.3).           |

### 3.3 Per-page integrity checksum

A `u32` at offset `PAGE_CHECKSUM_OFFSET` (24), little-endian. It is a 32-bit
truncation of `BLAKE3` over **every byte of the page except its own 4-byte
field** — i.e. `BLAKE3(page[0..24] ++ page[28..4096])`, first four digest bytes
as a little-endian `u32`.

- The value is **never `0`**: a computed `0` is stored as `1`. `0` is reserved to
  mean *unstamped* — a blank (allocated-but-never-written) page, or a page written
  outside the buffer pool. A reader **accepts** a stored `0` without verifying.
- The checksum is **stamped on write-out** (just before a page is handed to a
  backend) and **verified on read-in** (when a page is faulted into the buffer
  pool, and when a table's tail page is loaded). Resident, in-RAM pages may carry
  a stale checksum; that is harmless because resident reads do not re-verify and
  every backend write re-stamps.
- It is an integrity guard against **bit-rot and torn writes**, not an adversary.
  32 bits gives a ~2⁻³² miss rate against random corruption, which is ample for
  detect-and-rebuild. The one corruption pattern it cannot catch is one that
  zeroes exactly its own 4-byte field (indistinguishable from an unstamped page);
  such a page is instead caught by structural decode (a zeroed page type byte is
  an invalid `PageType`).

### 3.4 Row page body

From offset 28 upward grows the **slot array**; from `free_space_ptr` downward
grows the **record store**. Free space is the gap between them.

- Each slot is `SLOT_SIZE` (4) bytes: record `offset` (`u16`) then `len` (`u16`).
- Each record is a 24-byte **MVCC envelope** (§3.5) followed by its encoded
  payload (the row's values, with large values interned into the CAS — see §5).

On load, the invariant `28 + slot_count*4 ≤ free_space_ptr ≤ PAGE_SIZE` is
checked, and every slot/record extent is bounds-checked, so a crafted page yields
an error rather than an out-of-bounds panic.

### 3.5 Record envelope (24 bytes)

Wraps every record version; this is what makes reads MVCC / time-travelling.

| Offset | Size | Field         | Notes                                         |
|-------:|-----:|---------------|-----------------------------------------------|
| 0      | 8    | `tx_inserted` | Transaction that created this version.        |
| 8      | 8    | `tx_deleted`  | Transaction that tombstoned it, or `TX_NULL`. |
| 16     | 8    | `prev_version`| Physical address of the prior version, forming the MVCC chain. |

A version is visible to a snapshot at `target_tx` iff
`tx_inserted ≤ target_tx` and (`tx_deleted == TX_NULL` or `tx_deleted > target_tx`).
`SELECT ... BEFORE <tx>` reads against a past `target_tx`.

## 4. Columnar column blocks

After a columnar page header comes a `u16` `arity`, then one block per column:

```
arity: u16
repeated arity times:
  tag: u8            # 1 = delta-z ints, 2 = dictionary text, 3 = raw tagged
  payload_len: u32
  payload: payload_len bytes
```

Encodings: integer columns use Delta-Z (zig-zag + LEB128 of successive deltas);
low-cardinality text uses a bit-packed dictionary; anything else falls back to a
raw tagged encoding. Decoders bounds-check every length and reject unknown tags.

## 5. CAS blob pool

Large record payloads are content-addressed: each distinct blob is hashed with
`BLAKE3` (full 256-bit digest), stored once, and referenced by id. In a monolith
the blobs are packed contiguously in the CAS pool (between `cas_offset` and
`manifest_offset`); the manifest's `cas_dir` gives each blob's `(offset, len)`
within the pool and `cas_hashes` gives its hex digest. On open, every blob extent
is bounds-checked and (in dev mode) re-hashed against its recorded digest.

## 6. Manifest (JSON)

The catalog. In a monolith it is the trailing JSON payload; in a dev workspace it
is `pv_manifest.json`. Schema:

```jsonc
{
  "format_version": 1,          // u16; absent/0 ⇒ pre-freeze ⇒ rejected
  "clock": 0,                   // u64; the MVCC transaction clock
  "page_count": 0,              // u64; pages in the page-data block
  "tables": [
    {
      "name": "users",
      "columns": ["id", "name", "city"],
      "first_page": 0,          // Option<u64>: head of the page chain
      "tail_id": 0,             // Option<u64>: the resident write page
      "row_versions": 3,        // u64
      "indexed_columns": [],    // columns with a secondary index
      // One of the following persists each index, depending on the form (§6.1):
      "indexes": [              // dev workspaces / v1: JSON (key, addresses) pairs
        { "column": "city", "pairs": [ [<value>, [<addr>, ...]], ... ] }
      ],
      "binary_indexes": [       // v2 monolith: descriptors into the index region
        { "column": "city", "offset": 0, "len": 1234 }
      ]
    }
  ],
  "cas_hashes": ["<hex>", ...], // per-blob BLAKE3 digests
  "cas_dir": [[offset, len], ...], // per-blob (offset,len) in the CAS pool
  "index_region": [offset, len] // v2 only: absolute (offset,len) of the region
}
```

`format_version` is validated on every open. For a development workspace — which
has no file header — this is the **only** version gate, so it is what stops an
older build from mis-reading a newer workspace (and vice versa).

### 6.1 Secondary-index persistence

A secondary index (`SELECT ... ORDER BY col` / range / equality acceleration) is
an ordered map from a column value to the record addresses carrying it. It is
persisted so that opening a database does not have to rebuild it by scanning every
page. There are three forms, tried in this precedence on open:

1. **Binary region** (version-2 monolith). The manifest's `binary_indexes`
   descriptors each give a `column` and a `(offset, len)` slice — relative to the
   region start (`index_region[0]`) — holding that column's index blob. The blob:

   ```
   key_count: u32
   repeated key_count times, in ascending key order:
     key:        tagged value     # tag u8, then payload (see below)
     addr_count: u32
     addrs:      u64 * addr_count  # RecordAddr = page_id<<16 | slot
   ```

   The key codec is self-contained (it never reaches into the CAS pool): tag `0x00`
   Null (no payload), `0x01` Int (`i64`), `0x06` Decimal (`i128`), `0x02` Text and
   `0x03` Blob (`u32` length then that many bytes). Every length and extent is
   bounds-checked on decode.

2. **JSON pairs** (development workspaces, and version-1 files written by 1.2). The
   `indexes` array carries each index inline as `(value, [addresses])` pairs.

3. **Rebuild** (pre-1.2 files). Neither of the above is present but
   `indexed_columns` names the columns, so the index is rebuilt by one scan.

Record addresses are stable across a bake (pages keep their ids), so a persisted
index is valid in the baked file without adjustment.

## 7. Development workspace layout

```
name.pv/
  pv_manifest.json            # the manifest above (+ pv_manifest.json.tmp during a Sync commit)
  chunks/
    chunk_00000.pvd           # pages 0 .. 16383
    chunk_00001.pvd           # pages 16384 .. 32767
    ...
```

Each chunk holds up to `PAGES_PER_CHUNK` (16384) pages written at their
page-aligned offsets. Pages are append-only within the workspace's lifetime.

## 8. Versioning & compatibility policy

- A reader **rejects** any file or workspace whose `format_version` is `0` or
  greater than its own `FORMAT_VERSION`. This is enforced in both the monolith
  file header (`FileHeader::decode`) and the manifest (`check_manifest_version`).
- Files written by **0.10.x and earlier are not readable** by 0.11.0+: they
  predate both the versioned header and the 28-byte (checksummed) page header.
- **Version 2 is read by 1.3.0+ only.** A 1.3 build reads versions 1 and 2; a
  1.1/1.2 build cleanly rejects a version-2 file (it is refused, never mis-parsed).
  Because the version is bumped to 2 only when a binary index region is present,
  index-less files written by 1.3 stay version 1 and remain readable by 1.1/1.2.
- Any future change to the bytes described here **must** bump `FORMAT_VERSION`,
  add a golden fixture for the new version under `tests/fixtures/`, and preserve
  the old fixtures' read tests (`tests/format_robustness.rs`).

## 9. Durability model

`bake`/`import` are whole-image operations and are atomic at the OS level (write a
new file, or hand back a byte image). The development workspace has two policies,
selected by `Database::set_durability`:

- **`Fast` (default).** Flushes land in the OS page cache; data is durable on a
  clean process exit but a power-loss crash can lose writes since the last sync.
  No `fsync`. The manifest is written in place.
- **`Sync`.** Each flush `fsync`s every dirty chunk to stable storage, then
  commits the manifest **atomically**: write `pv_manifest.json.tmp`, `fsync` it,
  and `rename` it over `pv_manifest.json` (an atomic replace on POSIX and
  Windows). Because pages are append-only and the manifest is the single source of
  truth for `page_count` and table heads, a crash leaves either the old manifest
  (older consistent state) or the new one (committed state) — never a torn
  catalog. Pages written but not yet referenced by a committed manifest are simply
  unreachable.

Per-page checksums (§3.3) are orthogonal to the durability policy: they detect a
page that was torn or rotted on the medium regardless of how it got there, turning
silent corruption into a clean `Corruption` error at read time.

## 10. What is verified on open / read

| Check                                   | Where                          | On failure         |
|-----------------------------------------|--------------------------------|--------------------|
| Magic signature                         | `FileHeader::decode`           | `SignatureMismatch`|
| File `format_version` in range          | `FileHeader::decode`           | `Corruption`       |
| Monolith offsets consistent & aligned   | `Monolith::open`               | `Corruption`       |
| Manifest `format_version` in range      | `check_manifest_version`       | `Corruption`       |
| CAS blob extents in bounds              | `import_bytes` / `from_mapped` | `Corruption`       |
| Per-page checksum                       | buffer-pool fault / tail load  | `Corruption`       |
| Row page free-space invariant           | `RowPage::from_bytes`          | `Corruption`       |
| Slot / record extents in bounds         | record access                  | `Corruption`/`OutOfBounds` |

Every one of these is a structured error, never a panic — see
`tests/format_robustness.rs` for the corruption-injection coverage.
