# Release cadence

PicoVolt targets one stable release during the first full week of every month,
with patch releases as soon as a security or data-integrity fix is ready.

## Stable release checklist

1. Triage open regressions and update `CHANGELOG.md`.
2. Run the complete Rust, WASM, Go, Python, npm, CLI, and benchmark smoke matrix.
3. Confirm the committed root `Cargo.lock` is current and the package version is
   identical in Cargo, Python, npm, the Go module
   tag, and native version output.
4. Tag an annotated `vX.Y.Z` release.
5. Verify crates.io, npm, PyPI, `proxy.golang.org`, native binaries, C ABI
   bundles, SBOMs, checksums, and attestations.
6. Run clean-install starter tests and record download totals after 24 hours.

Release candidates may be published one week earlier for format or API changes.
Published `.pvdb` format compatibility is covered by the golden fixture suite.

The 1.8 native binaries include `data-tools` (Parquet, SQLite, and dataset
signing). Source installs opt in with `cargo install picovolt --features
data-tools`; npm and Python do not bundle these CLI dependencies. Release
verification runs `cargo test --locked --all-targets --all-features`, including
the public-dataset differential gate and corruption/resume/signature checks in
`tests/data_movement.rs` and `tests/data_inspection.rs`.

## Registry-only starter gate

Every pull request checks that the Rust, Node, browser, Python, and Go starters
pin the same version as `Cargo.toml` and contain no Cargo path dependency, npm
`file:`/`link:` dependency, editable Python requirement, or Go `replace`
directive. The policy also requires registry-backed SHA-512 npm lock entries,
Go module checksums, Cargo/Python/module version parity, and a byte-identical Go
copy of the public C header:

```sh
python scripts/check_registry_starters.py policy
python -m unittest discover -s scripts/tests -v
```

On a release tag, CI copies each starter to an isolated temporary directory,
installs that exact version, verifies its resolved package origin, and executes
it. Rust comes from crates.io, Node and browser from npm, and Python from PyPI.
The Go wrapper and ABI header come from `proxy.golang.org`; because it uses cgo,
its matching native library is taken from the PyPI wheel during the clean-room
gate. After PyPI publishing succeeds, the Python wheels workflow idempotently
creates the required `bindings/go/vX.Y.Z` submodule tag and then tests both
registries. The main release workflow waits for that exact tag workflow as well
as its crates.io/npm gate before it creates the GitHub Release. A failed or
missing registry therefore cannot leave a nominally complete GitHub Release.

The smoke runner starts each package manager with an allowlisted environment:
local registry/proxy settings, language search paths, dynamic-loader overrides,
and inherited credentials are removed. Ignored build output is also excluded
when a starter is copied. No test is allowed to reach into this checkout's
`bindings/`, `target/`, or root Cargo package.

Each GitHub Release contains CLI/server executables, SBOMs, checksums, and a
`picovolt-capi-<platform>.tar.gz` bundle with the matching shared library and
`include/picovolt.h`. Release creation is the final gate, after the package
registries and provenance-attested native artifacts have succeeded.
Both workflows serialize runs per tag. Publishing steps detect immutable
artifacts that already exist, so a rerun resumes safely; an unpublished crate
with no Cargo credential is a failure, never a silent skip.
The release compiler and `wasm-pack` version are pinned, Cargo commands use the
committed lockfile, and build paths are remapped out of the npm WebAssembly
binary so a pre-release package checksum can be reproduced on Linux.

After public artifacts exist, the same checks can be reproduced locally (the Go
native test currently requires Linux):

```sh
python scripts/check_registry_starters.py run --starter rust --starter node
python scripts/check_registry_starters.py run --starter browser --starter python
python scripts/check_registry_starters.py run --starter go
```
