# Contributing to PicoVolt

PicoVolt is an embedded database engine. The current stable release is 2.0;
supported APIs, formats and migration boundaries are documented in
[the support guide](docs/SUPPORT.md). Contributions, bug reports, and questions
are welcome.

## Development

```sh
cargo test                          # unit, integration, and doc tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same three checks on Linux and Windows. Please make sure they pass
locally before opening a pull request.

## Guidelines

- **Formatting and lints:** code must be `rustfmt`-clean and pass Clippy with
  `-D warnings`.
- **Tests:** new behavior needs tests. For the WASM interpreter in particular,
  prefer adding a case to the differential test that checks `pv-wasm` against the
  `wasmi` reference engine.
- **On-disk formats:** keep the explicit little-endian encoders. Do not persist
  via `#[repr(C)]` casts, and bump a format version if you change a layout.
- **Scope:** keep pull requests focused, and open an issue first for larger
  design changes.

## Versioning

PicoVolt follows [Semantic Versioning](https://semver.org). Breaking public API
or file-format changes require a major-version bump.
The daily minor and weekly major trains are release windows, not automatic
renumbering: compatible value ships in a minor, breaking changes ship in a major,
and a window is skipped rather than publishing an empty or unqualified release.

## License of contributions

Contributions are accepted under the project's [Apache License 2.0](LICENSE)
(inbound equals outbound, per section 5 of the license).

### Sign your work (DCO)

PicoVolt uses the [Developer Certificate of Origin](https://developercertificate.org/),
a lightweight, one-line alternative to a CLA. It is a statement that you wrote the
patch or otherwise have the right to submit it under the project license. Add a
`Signed-off-by` line to every commit:

```sh
git commit -s -m "your message"     # appends: Signed-off-by: Name <email>
```

Use your real name and an email you can be reached at; the name must match the DCO
text. CI checks that every commit in a pull request is signed off.
