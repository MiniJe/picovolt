// A better-sqlite3-style synchronous API over PicoVolt's WebAssembly engine, so
// code written for better-sqlite3 can use PicoVolt with minimal change:
//
//   import Database from "picovolt/sqlite";
//   const db = new Database();
//   db.exec("CREATE TABLE t (id, name)");
//   db.prepare("INSERT INTO t VALUES (?, ?)").run(1, "alice");
//   const rows = db.prepare("SELECT * FROM t WHERE id = ?").all(1);
//   // [ { id: 1, name: "alice" } ]
//
// Limitations: parameters are positional `?` only (named `:id` params are not
// supported); there are no transactions; blob parameters are unsupported.

import { Db } from "./picovolt.js";

function rowToObject(columns, row) {
  const obj = {};
  for (let i = 0; i < columns.length; i++) obj[columns[i]] = row[i];
  return obj;
}

// better-sqlite3 accepts bind values either positionally (`run(1, "a")`) or as a
// single array (`run([1, "a"])`); normalize both to one array.
function normalizeParams(args) {
  if (args.length === 1 && Array.isArray(args[0])) return args[0];
  return args;
}

class Statement {
  constructor(db, sql) {
    this._db = db;
    this.source = sql;
  }

  _exec(args) {
    const params = normalizeParams(args);
    const json = params.length ? this._db._db.query(this.source, params) : this._db._db.query(this.source);
    return JSON.parse(json);
  }

  run(...args) {
    const r = this._exec(args);
    return { changes: typeof r.mutated === "number" ? r.mutated : 0 };
  }

  get(...args) {
    const r = this._exec(args);
    if (!r.columns || !r.rows.length) return undefined;
    return rowToObject(r.columns, r.rows[0]);
  }

  all(...args) {
    const r = this._exec(args);
    if (!r.columns) return [];
    return r.rows.map((row) => rowToObject(r.columns, row));
  }

  *iterate(...args) {
    yield* this.all(...args);
  }
}

class Database {
  constructor() {
    this._db = new Db();
  }

  prepare(sql) {
    return new Statement(this, sql);
  }

  // Run one or more `;`-separated statements with no bound parameters.
  exec(sql) {
    for (const stmt of sql.split(";").map((s) => s.trim()).filter(Boolean)) {
      this._db.query(stmt);
    }
    return this;
  }

  // The most recent committed transaction id (upper bound for `... BEFORE tx`).
  get currentTx() {
    return this._db.currentTx();
  }

  // Export the database as a `.pvdb` byte image (Uint8Array).
  serialize() {
    return this._db.export();
  }

  pragma() {
    throw new Error("picovolt: pragma is not supported");
  }

  transaction() {
    throw new Error("picovolt: transactions are not supported");
  }

  close() {
    /* the WebAssembly instance is reclaimed by the GC */
  }
}

export default Database;
export { Database, Statement };
