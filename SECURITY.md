# Security Policy

## Status

PicoVolt is **experimental software** and has **not** been security-hardened or
audited. Do not use it to store sensitive data in production.

## Threat model notes

The engine parses untrusted-by-design binary input and runs guest code. Treat
the following as untrusted unless you produced them yourself:

- **`.pvdb` monolith files.** Opening one memory-maps it and parses an internal
  binary format and a JSON manifest. A maliciously crafted file could trigger a
  panic. Do not open `.pvdb` files from untrusted sources.
- **WASM extension modules.** The `wasmi` backend is a sandboxed interpreter and
  the built-in `pv-wasm` interpreter bounds-checks memory, but neither should be
  used to run untrusted code that has access to your secrets without further
  isolation review.

Known sharp edges: decoders aim to return `PvError` rather than panic, but they
have not been fuzzed; the development-mode persistence rewrites pages on every
mutation and is not crash-atomic.

## Reporting a vulnerability

Please open a GitHub issue describing the problem, or — for anything sensitive —
contact the maintainers privately before public disclosure. Since this is an
experimental project, there is no formal SLA, but reports are appreciated and
will be addressed on a best-effort basis.
