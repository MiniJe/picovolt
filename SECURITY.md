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
- **Page slot and record reads are bounds-checked,** so a crafted page returns an
  error rather than reading out of bounds.
- **Header, manifest, and feature versions must agree exactly.** An image is
  rejected if either version understates its index or schema metadata, or if its
  table identities, head/tail page ids, schema references, row arity, or record
  constraints are inconsistent.
- **SQL and persisted predicate trees have explicit complexity limits.** Parser
  depth, node counts, statement boundaries, defaults, and `CHECK` expressions
  are validated before execution so hostile input cannot grow the process stack
  or smuggle an unparsed suffix into a supported statement.
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
- **Bit-pack width is validated at runtime** (`bits` in `1..=8`), fixing a panic
  found by the fuzz-lite test on a malformed dictionary.

The decoders are fuzzed. A deterministic test,
[`tests/fuzz_smoke.rs`](tests/fuzz_smoke.rs), runs in CI on every platform, and a
coverage-guided [`fuzz/`](fuzz) cargo-fuzz crate
(`cargo +nightly fuzz run decode_monolith | decode_wasm | decode_columnar`) runs
on Linux. `cargo audit` reports no vulnerability failures and runs in CI; its
non-failing yanked/transitive warnings are still reviewed during dependency
updates.

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
