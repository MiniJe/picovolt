# Releasing PicoVolt

This is the process and the versioning scheme for every PicoVolt release. It
exists so that cutting a version is mechanical and the meaning of a version
number is unambiguous.

## Versioning scheme

PicoVolt uses [Semantic Versioning](https://semver.org) — `MAJOR.MINOR.PATCH` —
because that is what crates.io and npm require and what `cargo`/`npm` use to
resolve dependency ranges. Our three release *tiers* map onto the three SemVer
positions:

| Tier        | What it contains                                              | Version bump          | Example          |
|-------------|--------------------------------------------------------------|-----------------------|------------------|
| **Minimal** | Bug fixes, security patches, docs, QoL — no API changes      | **PATCH** (3rd digit) | `0.1.0` → `0.1.1`|
| **Normal**  | New, backward-compatible features (new SQL, new APIs)        | **MINOR** (2nd digit) | `0.1.3` → `0.2.0`|
| **Major**   | Breaking changes to the public API or the on-disk format     | **MAJOR** (1st digit) | `0.9.2` → `1.0.0`|

> **Why not `X.Y.001`?** SemVer forbids leading zeros in a version component,
> and crates.io / npm reject `0.1.001` at publish time. So "Minimal" is a plain
> patch increment: `0.1.1`, `0.1.2`, … `0.1.10`, not `0.1.001`. The *tier* is
> exactly what you proposed; only the zero-padding is dropped to satisfy the
> registries.

### The pre-1.0 rule (we are here)

While the major version is `0`, SemVer treats the **minor** as the
breaking-change signal. Cargo enforces this: `0.1.x` and `0.2.x` are considered
incompatible. So until we ship `1.0.0`:

- **Minimal** → patch bump (`0.1.0` → `0.1.1`) — same as above.
- **Normal** *and* **Major** → minor bump (`0.1.x` → `0.2.0`). A new feature and
  a breaking change look the same to Cargo at `0.y.z`.

`1.0.0` is its own deliberate milestone: it is the promise that the public API
and the `.pvdb` on-disk format are stable. We cut it only when we are ready to
make that promise — not automatically. After `1.0.0`, the full table above
applies and the three tiers line up one-to-one with the three digits.

### What counts as "breaking"

- A change to the `.pvdb` / `.pv` on-disk byte format that old readers can't open.
- Removing or renaming a public Rust item (`pub fn`, `pub struct`, variant, …).
- Removing or renaming an exported JS/wasm binding (`Db`, `query`, `fromBytes`, …).
- A SQL change that makes a previously valid query error or change meaning.

Anything that only *adds* surface (new method, new SQL clause, new variant behind
existing matches) is **Normal**, not Major.

## Release checklist

Every release, regardless of tier, runs the same gate. `X.Y.Z` below is the new
version.

1. **Green main.** `git switch main && git pull`. Working tree clean.
2. **Verify locally** (the same gate CI enforces):
   ```sh
   cargo fmt --check
   cargo clippy --all-targets --features wasm -- -D warnings
   cargo test
   cargo build --lib --target wasm32-unknown-unknown --release --features wasm
   cargo audit            # no advisories
   ```
3. **Bump the version** in `Cargo.toml` (`version = "X.Y.Z"`). Run `cargo build`
   once so `Cargo.lock` updates.
4. **Update [`CHANGELOG.md`](CHANGELOG.md):** move everything under `## [Unreleased]`
   into a new `## [X.Y.Z] - <date>` section. Leave a fresh, empty `Unreleased`.
5. **Commit:** `git commit -am "Release X.Y.Z"`.
6. **Tag:** `git tag -a vX.Y.Z -m "PicoVolt X.Y.Z"` (annotated, `v`-prefixed).
7. **Push:** `git push origin main --follow-tags`.
8. The tag push triggers [`.github/workflows/release.yml`](.github/workflows/release.yml),
   which re-runs the gate and (if secrets are configured) publishes — see below.

## Publishing (crates.io + npm)

Publishing is **not** automatic on a normal push — only a `vX.Y.Z` tag triggers
it, and only if the registry tokens are present as repository secrets:

- `CARGO_REGISTRY_TOKEN` — from <https://crates.io/me> → API Tokens.
- `NPM_TOKEN` — an npm automation token with publish rights.

Without those secrets the workflow still runs the full test gate and creates the
GitHub Release; the publish steps are skipped. To publish by hand instead:

```sh
# crates.io  (Cargo.lock is not committed, so no --locked)
cargo publish                                # dry run first: cargo publish --dry-run

# npm (WebAssembly package)
cargo build --lib --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --target bundler --out-dir pkg \
  target/wasm32-unknown-unknown/release/picovolt.wasm
cd pkg && npm publish --access public
```

> The maintainer holds the crates.io / npm credentials. CI publishes only when
> those secrets are set; otherwise these commands are run locally by someone who
> has them.

## After a release

- Confirm the GitHub Release and (if published) the crates.io / npm pages.
- Rebuild the site bindings so the live playground tracks the release:
  ```sh
  cargo build --lib --target wasm32-unknown-unknown --release --features wasm
  wasm-bindgen --target web --out-dir site/pkg \
    target/wasm32-unknown-unknown/release/picovolt.wasm
  ```
- Start the next `## [Unreleased]` notes as work lands.
