# picovolt.dev — landing page + live playground

A static marketing page for PicoVolt with an **embedded live SQL playground**
powered by the WebAssembly build — it runs entirely in the visitor's browser,
no backend. `index.html` is self-contained (inline CSS/JS) and imports
`./pkg/picovolt.js`.

## Build & serve

From the repo root:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack            # or: cargo install wasm-bindgen-cli

# generate site/pkg/ (the .wasm + JS glue the page imports)
wasm-pack build --target web --release --out-dir site/pkg -- --features wasm

# serve it (any static host works; for local preview:)
python -m http.server 8000 --directory site
# → open http://localhost:8000
```

Deploy `site/` (with the built `pkg/`) to any static host for picovolt.dev.
Replace the `OWNER` placeholders in the GitHub links before shipping.
