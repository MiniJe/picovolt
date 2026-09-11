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
// The proprietary development line is packaged privately, never npm-published.
if (fs.readFileSync('Cargo.toml', 'utf8').includes('publish = false')) {
  pkg.private = true;
  pkg.license = 'LicenseRef-PicoVolt-Proprietary-1.0';
  for (const name of ['LICENSE', 'NOTICE']) fs.copyFileSync(name, `${directory}/${name}`);
  fs.copyFileSync('legal/APACHE-2.0-LEGACY.txt', `${directory}/APACHE-2.0-LEGACY.txt`);
  pkg.files = Array.from(new Set([...pkg.files, 'LICENSE', 'NOTICE', 'APACHE-2.0-LEGACY.txt']));
}
fs.writeFileSync(path, JSON.stringify(pkg, null, 2));
NODE
