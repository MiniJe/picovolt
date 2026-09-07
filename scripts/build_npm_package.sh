#!/usr/bin/env bash
set -euo pipefail

out_dir="${1:-pkg}"
workspace="${GITHUB_WORKSPACE:-$(pwd)}"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

export RUSTFLAGS="--remap-path-prefix=$workspace=/src/picovolt --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$rustup_home=/rustup"

wasm-pack build --target bundler --release --out-dir "$out_dir" -- --locked --features wasm
cp bindings/js/sqlite.js "$out_dir/sqlite.js"
cp bindings/js/browser.js "$out_dir/browser.js"
cp bindings/js/worker.js "$out_dir/worker.js"
node - "$out_dir" <<'NODE'
const fs = require("fs");
const directory = process.argv[2];
const path = `${directory}/package.json`;
const pkg = JSON.parse(fs.readFileSync(path));
pkg.files = Array.from(
  new Set([...(pkg.files || []), "sqlite.js", "browser.js", "worker.js"]),
);
pkg.exports = {
  ".": `./${pkg.module || "picovolt.js"}`,
  "./sqlite": "./sqlite.js",
  "./browser": "./browser.js",
  "./worker": "./worker.js",
};
pkg.repository = {
  type: "git",
  url: "git+https://github.com/MiniJe/picovolt.git",
};
fs.writeFileSync(path, JSON.stringify(pkg, null, 2));
NODE
