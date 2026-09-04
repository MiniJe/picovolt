# Security Policy

## Status

PicoVolt is young software with a stable 1.x API and file-format contract. The
untrusted-input parsing paths have been hardened, reviewed, and fuzzed (see
below), but the fuzzing has not run for long soak times and the code has not been
independently audited or certified. Keep tested backups for data you cannot
regenerate.

## Hardening done

A security review of the parsing paths made the following fixes, each with a
regression test:

- **CAS hashes from the manifest are validated** as 64 hexadecimal characters
  before being used as file names, which closes path traversal and arbitrary file
  reads. Blob contents are integrity-checked against their claimed BLAKE3 digest
  in development, mmap, in-memory import, and streamed-open paths.
- **CAS and index-region offsets are bounds-checked** against the appropriate
  pool, manifest, and image boundaries before slicing.
- **Page-chain traversal is capped** at the total page count, so a cyclic
  `next_page` link returns an error instead of looping forever.
- **Page-chain ownership is checked before indexed mutations.** A corrupt index
  cannot redirect an update/delete into another table's page; scans also verify
  their declared tail and exact MVCC record-version count.
- **Page slot, record, and cold-column reads are bounds-checked.** Cold pages
  prove their claimed row/column shape could have fit the source row page before
  allocating decoded values, so a tiny crafted page cannot request an
  attacker-sized logical matrix.
- **Header, manifest, and feature versions must agree exactly.** An image is
  rejected if either version understates its index, schema, or cold-page
  metadata, or if its table identities, head/tail page ids, schema references,
  row arity, or record constraints are inconsistent.
- **SQL and persisted predicate trees have explicit complexity limits.** Parser
  depth, node counts, statement boundaries, defaults, and `CHECK` expressions
  are validated before execution so hostile input cannot grow the process stack
  or smuggle an unparsed suffix into a supported statement.
- **SQL dump splitting preserves quoted identifiers.** Apostrophes, semicolons,
  punctuation, and keyword names inside delimiters remain identifier data rather
  than becoming executable syntax during compatibility rewriting.
- **Query budgets cover internal work as well as returned rows.** Uniqueness
  scans, existing-row checks, and equality-index candidate sets are charged
  before large allocations; bounded range predicates use a streaming path.
- **Compound mutations are statement-atomic.** Multi-row inserts, deletes, and
  updates validate record shape and constraints before mutation, then roll back
  on a mutation-phase I/O failure. Without savepoints, that kind of failure
  aborts an enclosing explicit transaction rather than leaving it committable.
- **The `pv-wasm` decoder caps** declared memory pages and all LEB128 vector
  counts, preventing out-of-memory from a crafted module.
- **Both WASM runtimes meter instructions** and cap guest memory and returned
  output, so a looping or malicious extension traps instead of monopolizing the
  process or requesting an unbounded allocation.
- **Streamed readers validate exact range lengths and image sizes** and cap the
  eagerly fetched tail, so a dishonest remote reader cannot trigger a slice panic
  or an attacker-sized allocation.
- **The optional HTTP server is bounded and authenticated for network use.** It
  has a finite request queue and query budgets, rejects browser-origin query
  requests, requires JSON, and refuses a non-loopback bind without a bearer token.
- **Compressed and index encodings are canonical.** Varint overflow and
  overlong forms, invalid bit-pack widths/padding, oversized dictionaries,
  duplicate binary-index keys, and trailing payload bytes are rejected rather
  than decoded ambiguously.
- **Persisted offsets are platform-safe.** File, CAS, and binary-index offsets
  and lengths are checked-converted before they become slice indices, including
  on 32-bit and WebAssembly targets.

The decoders and SQL parser are fuzzed. A deterministic test,
[`tests/fuzz_smoke.rs`](tests/fuzz_smoke.rs), runs in CI on every platform, and a
coverage-guided [`fuzz/`](fuzz) cargo-fuzz crate
(`cargo +nightly fuzz run parse_sql | decode_record | decode_index |
decode_monolith | decode_wasm | decode_columnar`) runs in bounded weekly Linux
jobs. Successful monolith inputs are fully inspected and scanned so lazy page,
record, CAS, and index decoders are exercised. `cargo audit` reports no
vulnerability failures and runs in CI; its non-failing yanked/transitive
warnings are still reviewed during dependency updates.

## Threat model notes

Treat these as untrusted unless you produced them yourself:

- **`.pvdb` files.** Opening one snapshots it into an owned read-only mapping and
  parses an internal binary format and JSON manifest. The source file may change
  after open without racing or altering the snapshot.
- **WASM extension modules.** These are metered and memory-bounded, but the WASM
  host still runs in your process. Keep secrets out of extension inputs and use
  process isolation when extensions come from mutually untrusted tenants.

Known limits: the default durability mode uses the OS cache and is not
power-loss-safe (use
`Durability::Sync` for crash-safe flushes). There is no encryption of data at
rest, TLS termination is external to the optional server, and fuzzing has run but
not for long soak times.

## Reporting a vulnerability

For anything sensitive, report privately before public disclosure. Please do not
open a public issue for an exploitable bug. Use GitHub's
[private vulnerability reporting](https://github.com/MiniJe/picovolt/security/advisories/new)
(the repository's Security tab, "Report a vulnerability"). This is the project's
only documented private security-reporting channel.

For non-sensitive hardening suggestions, a regular GitHub issue is fine. As this
is a small independent project there is no formal response-time commitment, but
reports are appreciated and addressed on a best-effort basis.
