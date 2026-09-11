# PicoVolt 2.2.0 private qualification

Prepared 12 September 2026. This version continues the private proprietary line
selected for 2.1.0. It is not a public package or issued customer license offer.
Production Hub and the public website retain their separately qualified runtime.

## Implemented

- Authenticated encrypted snapshots covering schema, pages, indexes, blobs and history.
- Memory-resident, single-writer encrypted vaults with atomic ciphertext commits.
- Raw-key generation, exact-byte password files and fixed-profile Argon2id derivation.
- Key/password rotation, verified encrypted backups and restore to a new vault.
- Envelope inspection and authenticated database verification.
- Native CLI, Rust, C, Python and Go vault interfaces.
- Hybrid full-text/vector retrieval with weighted reciprocal-rank fusion and rank explanations.

Read [the encryption contract](ENCRYPTION_2_2.md) and [hybrid search](HYBRID_2_2.md)
before using these APIs. Full-image vault commits are intended for bounded
databases, not an unmeasured replacement for page-level I/O on large stores.

## Qualification record

The final artifact manifest records the exact source commit, package hashes and
completed checks. Tests cover authentication, invalid inputs, history, failed
batches, writer locking, rotation, backup recovery, native bindings and filtered
hybrid ranking. Interoperability is checked in both directions with libsodium
through PyNaCl. Dependency advisory scanning is separate from security review.

Completed checks for this build:

| Check | Result |
| --- | --- |
| Windows Rust all-feature/all-target tests | Passed |
| Clippy with warnings denied, rustfmt, rustdoc and doctests | Passed |
| Minimal, encryption/C ABI, full-text-only and vector-only feature builds | Passed |
| Windows Python binding tests | 11 passed against the release DLL |
| Windows Go tests and vet | Passed against the release DLL |
| JavaScript/WASM adapter integration | 6 passed against the 2.2 package |
| Linux encryption, retrieval and abrupt process termination tests | Passed |
| Packaged Windows wheel load, commit, backup, rotation and reopen | Passed |
| Linux release C ABI commit, rollback, backup, rotation and reopen | Passed |
| XChaCha20-Poly1305 interoperability with libsodium | Passed in both directions; tampering rejected |
| Dependency advisory scan | No reported vulnerabilities at build time |

Native archives target Windows x86-64 GNU and Linux x86-64, with the Linux build
qualified on Ubuntu 24.04 under WSL. Linux includes the C ABI and default
encryption/retrieval features; optional `data-tools` are included only in the
Windows archive. A Windows Python wheel and a private WASM/npm package are also
included. macOS and ARM binaries were not built or qualified. WASM offers hybrid
retrieval, not native vault encryption.

This integration has not received an independent cryptographic/security audit.
No power-loss guarantee, encrypted browser storage, distributed vault writers,
transparent encryption of legacy development directories, comprehensive memory
erasure or built-in rollback protection across reopen is claimed. Linux/macOS
and platform package availability are distinguished in the artifact manifest;
do not infer support for an unbuilt target.

## Release controls

Cargo remains `publish=false`; npm packages remain private; public release
workflows fail the proprietary publication gate. Existing Apache releases keep
their original grants. Source/artifacts stay outside the website document root.
No production keys, customer offers, telemetry or forced-upgrade enforcement were
created. Customer delivery still requires a reviewed offer identifying component
scope, exact artifact/terms digests, price, acceptance and receipt.
