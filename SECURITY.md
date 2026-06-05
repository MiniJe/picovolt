# Security Policy

## Status

PicoVolt is **experimental software**. The untrusted-input parsing paths have
been hardened (see below) and reviewed, but the code has **not** been fuzzed or
independently certified. Don't store data you can't lose.

## Hardening done

A security review of the parsing paths fixed, with regression tests:

- **CAS hashes from the manifest are validated** as 64 hex chars before being
  used as file names (closes path traversal / arbitrary file read), and blob
  contents are integrity-checked against their claimed BLAKE3 digest.
- **CAS directory offsets are bounds-checked** against the mmap length.
- **Page-chain traversal is capped** at the total page count, so a cyclic
  `next_page` link errors instead of looping forever.
- **Page slot/record reads are bounds-checked** — a crafted page errors rather
  than panicking out of bounds.
- **The `pv-wasm` decoder caps** declared memory pages and all LEB128 vector
  counts, preventing OOM from a crafted module.

`cargo audit` reports no vulnerable dependencies (one informational
unmaintained-crate notice: `paste`, transitive via `wasmi`).

## Threat model notes

Still treat these as untrusted unless you produced them yourself:

- **`.pvdb` monolith files** — opening one memory-maps it and parses an internal
  binary format + JSON manifest. The mmap itself is `unsafe`: if the file is
  mutated by another process while mapped, behavior is undefined.
- **WASM extension modules** — sandboxed, but don't run untrusted code with
  access to your secrets without further isolation review.

Remaining sharp edges: parsers are bounds-checked but **not fuzzed**; the default
durability mode is OS-cache (not power-loss-safe) — use `Durability::Sync` for
crash-safe flushes; there is no authentication/encryption of data at rest.

## Reporting a vulnerability

Please open a GitHub issue describing the problem, or — for anything sensitive —
contact the maintainers privately before public disclosure. Since this is an
experimental project, there is no formal SLA, but reports are appreciated and
will be addressed on a best-effort basis.
