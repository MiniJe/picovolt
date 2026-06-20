# Security Policy

## Status

PicoVolt is **experimental software**. The untrusted-input parsing paths have
been hardened, reviewed, and fuzzed (see below), but the fuzzing has not run for
long soak times and the code has **not** been independently audited or certified.
Don't store data you can't lose.

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
- **Bit-pack width is validated at runtime** (`bits ∈ 1..=8`) — a panic found by
  the fuzz-lite test on a malformed dictionary.

The decoders are **fuzzed**: a deterministic [`tests/fuzz_smoke.rs`](tests/fuzz_smoke.rs)
runs in CI on every platform, and a coverage-guided [`fuzz/`](fuzz) cargo-fuzz
crate (`cargo +nightly fuzz run decode_monolith | decode_wasm | decode_columnar`)
runs on Linux. `cargo audit` reports **no advisories** and runs in CI.

## Threat model notes

Still treat these as untrusted unless you produced them yourself:

- **`.pvdb` monolith files** — opening one memory-maps it and parses an internal
  binary format + JSON manifest. The mmap itself is `unsafe`: if the file is
  mutated by another process while mapped, behavior is undefined.
- **WASM extension modules** — sandboxed, but don't run untrusted code with
  access to your secrets without further isolation review.

Remaining sharp edges: the default durability mode is OS-cache (not
power-loss-safe) — use `Durability::Sync` for crash-safe flushes; there is no
authentication/encryption of data at rest; fuzzing has run but not for long
soak times.

## Reporting a vulnerability

For anything sensitive, **report privately before public disclosure** — please
don't open a public issue for an exploitable bug. Use GitHub's
[private vulnerability reporting](https://github.com/MiniJe/picovolt/security/advisories/new)
(the repo's **Security → Report a vulnerability** tab), or email
`security@picovolt.dev`.

For non-sensitive hardening suggestions, a regular GitHub issue is fine. Since
this is an experimental project there is no formal SLA, but reports are
appreciated and addressed on a best-effort basis.
