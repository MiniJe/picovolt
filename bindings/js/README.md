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
`all(...params)`, and `iterate(...params)`; preparation validates SQL immediately,
and `parameterCount` reports the exact positional arity. Call `close()` (or its
`finalize()` alias) when a long-lived application is done with a statement;
closing the database also closes every outstanding statement. `exec(sql)` runs
`;`-separated statements without parameters.

`db.transaction(fn)` returns a synchronous atomic wrapper. If `fn` throws, the
database is restored to its pre-transaction snapshot.

For durable browser storage, import `PersistentDb` from `picovolt/browser`; it
loads and saves a `.pvdb` image in OPFS and exposes the same reusable
`prepare(sql)` contract. `picovolt/worker` is a ready-made module worker accepting
`open`, `query`, `prepare`, `execute`, `finalize`, `save`, and `close` RPC
messages. `prepare` returns `{ statementId, parameterCount }`; pass that id to
`execute` and release it with `finalize`. Opening another database or sending
`close` invalidates all outstanding worker statement IDs.

Every JavaScript surface uses PicoVolt's shared SQL subset, including table
aliases, N-table equality `INNER`/`LEFT` joins, searched `CASE`, and focused
scalar functions. See the [SQL compatibility reference](../../docs/SQL.md) for
examples and exact limits.

## Limitations

PicoVolt is not SQLite, so some better-sqlite3 features are intentionally absent:

- Parameters are positional `?` only. Named parameters (`:id`) are not supported.
- No `pragma`.
- Blob parameters are unsupported.
- Joins accept `INNER` and `LEFT` equality clauses. `RIGHT`, `FULL`, `CROSS`,
  arbitrary `ON` predicates, and qualified wildcards such as `u.*` are not
  supported.
- `CASE` and scalar functions currently belong in the select list; they cannot
  yet be used directly in `WHERE`, `JOIN ON`, grouping, or ordering.

The raw engine API (`import { Db } from "picovolt"`) remains available if you want
the JSON-returning `db.query(sql, params)` directly.
