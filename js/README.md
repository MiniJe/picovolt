# PicoVolt for JavaScript / npm (WebAssembly)

PicoVolt compiles to WebAssembly via `wasm-bindgen`, exposing an **in-memory**
database to JS (browser or Node). A browser has no filesystem, so the wasm build
uses the in-memory engine ([`Database::open_memory`]); call `db.export()` to get
a `.pvdb` byte image you can save/download.

## Build the package

Requires [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) and the wasm target:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Browser (ES modules):
wasm-pack build --target web --release --out-dir js/pkg -- --features wasm

# …or Node.js (CommonJS):
wasm-pack build --target nodejs --release --out-dir js/pkg -- --features wasm
```

This produces `js/pkg/` with the `.wasm`, JS glue, TypeScript types, and a
`package.json` ready to `npm publish` (or `wasm-pack publish`).

## Use it

```js
import init, { Db } from "./pkg/picovolt.js"; // --target web

await init();                       // load the wasm module
const db = new Db();
db.query("CREATE TABLE users (id, name, tier)");
db.query("INSERT INTO users VALUES (1, 'alice', 'free')");
db.query("INSERT INTO users VALUES (2, 'bob', 'pro')");
db.query("CREATE INDEX ON users (tier)");

const res = db.query("SELECT * FROM users WHERE tier = 'free'");
console.log(res); // { columns: ["id","name","tier"], rows: [[1,"alice","free"]] }

const bytes = db.export(); // Uint8Array — a .pvdb image
```

Open [`index.html`](index.html) after building for `--target web` to see it run
in the browser, or run `node node-demo.cjs` after building for `--target nodejs`.

## API

| Method | Returns |
|--------|---------|
| `new Db()` | a fresh in-memory database |
| `db.query(sql)` | `{ columns, rows }` (SELECT), `{ mutated: n }` (INSERT/UPDATE/DELETE), or `{ done: true }`; throws the error message on failure |
| `db.export()` | the database as a `.pvdb` `Uint8Array` |

Supported SQL: `CREATE TABLE`, `CREATE INDEX ON t (col)`, `INSERT`,
`UPDATE … SET … WHERE`, `DELETE … WHERE`, `DROP TABLE`,
`SELECT * FROM t [WHERE col = v] [BEFORE tx] [LIMIT n]`.
