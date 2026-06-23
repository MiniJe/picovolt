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

## Limitations

PicoVolt is not SQLite, so some better-sqlite3 features are intentionally absent:

- Parameters are positional `?` only. Named parameters (`:id`) are not supported.
- No transactions: `db.transaction(...)` throws.
- No `pragma`.
- Blob parameters are unsupported.
- The SQL subset has no JOINs, and `CREATE TABLE` takes column names only (no
  types or constraints).

The raw engine API (`import { Db } from "picovolt"`) remains available if you want
the JSON-returning `db.query(sql, params)` directly.
