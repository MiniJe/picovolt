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

Contributions are accepted under the project's [Apache License 2.0](LICENSE)
(inbound = outbound, per section 5 of the license).

### Sign your work (DCO)

PicoVolt uses the [Developer Certificate of Origin](https://developercertificate.org/)
— a lightweight, one-line alternative to a CLA. It's a statement that you wrote
the patch or otherwise have the right to submit it under the project license. Add
a `Signed-off-by` line to every commit:

```sh
git commit -s -m "your message"     # appends: Signed-off-by: Name <email>
```

Use your real name and an email you can be reached at; the name must match the
DCO text. CI checks that commits in a PR are signed off.

**One thing to be aware of, stated plainly:** because contributions are
Apache-2.0 (which permits proprietary downstream use), code you contribute to the
open core may also appear in the planned commercial **`picovolt-pro`** edition
described in [ROADMAP.md](ROADMAP.md). The open core stays open — see the roadmap
for how that boundary works.
