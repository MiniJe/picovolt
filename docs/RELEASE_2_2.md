# PicoVolt 2.2.0 release verification

Published 12 September 2026 at 10:37 UTC as
[v2.2.0](https://github.com/MiniJe/picovolt/releases/tag/v2.2.0), commit
`bf9038536c84f8903f8bf25b555299ac8583e83b`. Public source and free registry downloads
use PicoVolt Public-Source License 1.1. Production Hub and browser runtime upgrades
remain separate deployments.

## Public verification

- [Cross-platform CI](https://github.com/MiniJe/picovolt/actions/runs/34688185848): passed.
- [Fuzz and model/recovery shards](https://github.com/MiniJe/picovolt/actions/runs/34687960709): passed.
- [Performance budgets and reproducible npm archive](https://github.com/MiniJe/picovolt/actions/runs/34687974209): passed.
- [Tagged release](https://github.com/MiniJe/picovolt/actions/runs/34688473775): passed, including exact Cargo/npm registry installs and all native builds.
- [Tagged Python/Go workflow](https://github.com/MiniJe/picovolt/actions/runs/34688473827): passed, including installed wheel tests and clean Python/Go registry installs.
- All 18 native assets match SHA256SUMS and have verified signed provenance for
  the exact release workflow, tag and source commit. Published Windows/Linux CLI
  version probes and Windows CLI/libsodium interoperability passed.
- Public npm bytes match the locally verified and independently hosted build.
  Public crate, Python wheel and Go module hashes are recorded in the
  [machine-readable verification record](RELEASE_2_2_VERIFICATION.json).

Public Python wheels cover Windows x86-64, manylinux 2.28 x86-64 and macOS
universal2 (Intel macOS 10.12+; Apple Silicon macOS 11+). Native CLI/server/C ABI
bundles cover Linux x86-64, macOS arm64 and Windows x86-64. The corrected macOS
wheel tag is checked against both Mach-O slices. No Linux ARM binary is supplied.

Some regional Go `.info` responses retained a negative cache while `@latest`,
the exact module archive and hosted clean installs already succeeded. An observed
regional cache expires at 10:57:36 UTC on publication day; tags were not moved.

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

## Earlier private qualification record

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

## Public distribution controls

`legal/PUBLIC-RELEASE.json` binds this version to the exact license digest, zero
fee, covered components and preservation of existing licenses. Public registry
publication checks that notice and the license copies in each binding. No account,
clickwrap service, activation or customer receipt is required for this release.

The public release passed the hosted matrix and exact registry install checks
before GitHub Release creation. The private artifact hashes are
historical qualification evidence; public packages are rebuilt with the revised
terms, notices and reproducible compiler. Platform claims for public artifacts
will be recorded from those builds, without extending the private results above.

Independent cryptographic/legal review has not been performed. No automated
verification result is described as an independent audit. Prior Apache grants
and dependency licenses remain intact.
