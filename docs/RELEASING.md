# Release cadence

PicoVolt targets one stable release during the first full week of every month,
with patch releases as soon as a security or data-integrity fix is ready.

## Stable release checklist

1. Triage open regressions and update `CHANGELOG.md`.
2. Run the complete Rust, WASM, Go, Python, npm, CLI, and benchmark smoke matrix.
3. Confirm the package version is identical in Cargo, Python, and npm outputs.
4. Tag an annotated `vX.Y.Z` release.
5. Verify crates.io, npm, PyPI, native binaries, SBOMs, and attestations.
6. Run clean-install starter tests and record download totals after 24 hours.

Release candidates may be published one week earlier for format or API changes.
Published `.pvdb` format compatibility is covered by the golden fixture suite.
