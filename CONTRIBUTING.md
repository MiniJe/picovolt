# Contributing to PicoVolt

Thanks for your interest! PicoVolt is an **experimental, educational** embedded
data engine — contributions, bug reports, and questions are all welcome.

## Development

```sh
cargo test                          # unit + integration + doc tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same three checks on Linux and Windows; please make sure they pass
locally before opening a PR.

## Guidelines

- **Formatting & lints:** code must be `rustfmt`-clean and pass Clippy with
  `-D warnings`.
- **Tests:** new behavior needs tests. For the WASM interpreter especially,
  prefer adding a case to the differential test that checks `pv-wasm` against
  the `wasmi` reference engine.
- **On-disk formats:** keep the explicit little-endian encoders — don't persist
  via `#[repr(C)]` casts. Bump a format version if you change a layout.
- **Scope:** keep PRs focused; open an issue first for larger design changes.

## License of contributions

By submitting a contribution you agree that it is licensed under the project's
[Apache License 2.0](LICENSE) (see section 5 of the license).
