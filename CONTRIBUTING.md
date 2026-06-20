# Contributing to PicoVolt

PicoVolt is an experimental embedded database engine. Contributions, bug reports,
and questions are all welcome.

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

PicoVolt follows [Semantic Versioning](https://semver.org). While the project is
pre-1.0, breaking changes are released as minor-version bumps.

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
