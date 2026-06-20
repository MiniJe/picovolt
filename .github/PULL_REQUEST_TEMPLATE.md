<!-- Thanks for contributing! Keep PRs focused; open an issue first for larger design changes. -->

## What and why

<!-- What does this change, and what problem does it solve? Link any related issue. -->

Closes #

## Checklist

- [ ] `cargo test` passes (unit + integration + doc tests)
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all` applied
- [ ] New behavior has tests (for `pv-wasm`, a case in the differential test)
- [ ] On-disk format changes bump a format version and keep the explicit LE codecs
- [ ] Commits are signed off (`git commit -s`, see [CONTRIBUTING.md](../blob/main/CONTRIBUTING.md))

## Notes for reviewers

<!-- Anything tricky, trade-offs you made, or areas you'd like a closer look at. -->
