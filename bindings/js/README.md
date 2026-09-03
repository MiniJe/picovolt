# PicoVolt drop-in adapter for JavaScript

A [better-sqlite3](https://github.com/WiseLibs/better-sqlite3)-style synchronous
API over the PicoVolt npm package, so code written for better-sqlite3 can use
PicoVolt with minimal change.

```js
import Database from "picovolt/sqlite";

const db = new Database();
db.exec("CREATE TABLE users (id, name)");

const insert = db.prepare("INSERT INTO users VALUES (?, ?)");
insert.run(1, "alice");
insert.run(2, "bob");

const user = db.prepare("SELECT * FROM users WHERE id = ?").get(1);
// { id: 1, name: "alice" }

const all = db.prepare("SELECT * FROM users").all();
// [ { id: 1, name: "alice" }, { id: 2, name: "bob" } ]
```

`prepare(sql)` returns a statement with `run(...params)`, `get(...params)`,
`all(...params)`, and `iterate(...params)`; `exec(sql)` runs `;`-separated
statements without parameters.

`db.transaction(fn)` returns a synchronous atomic wrapper. If `fn` throws, the
database is restored to its pre-transaction snapshot.

For durable browser storage, import `PersistentDb` from `picovolt/browser`; it
loads and saves a `.pvdb` image in OPFS. `picovolt/worker` is a ready-made module
worker accepting `open`, `query`, `save`, and `close` RPC messages.

## Limitations

PicoVolt is not SQLite, so some better-sqlite3 features are intentionally absent:

- Parameters are positional `?` only. Named parameters (`:id`) are not supported.
- No `pragma`.
- Blob parameters are unsupported.
- JOIN support covers equality `INNER JOIN` and `LEFT JOIN`, including column
  projection and `DISTINCT`; joined-row filters, ordering, and N-table plans are
  not yet supported.

The raw engine API (`import { Db } from "picovolt"`) remains available if you want
the JSON-returning `db.query(sql, params)` directly.
