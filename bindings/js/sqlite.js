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
// supported); blob parameters are unsupported.

import { Db } from "./picovolt.js";

function rowToObject(columns, row) {
  // A null prototype prevents hostile column names such as `__proto__` from
  // changing the shape or prototype of the returned record.
  const obj = Object.create(null);
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
    this._inTransaction = false;
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

  // Wrap a synchronous callback in an atomic unit. A compact PVDB snapshot is
  // restored if the callback throws, matching better-sqlite3's common pattern.
  transaction(fn) {
    if (typeof fn !== "function") throw new TypeError("transaction expects a function");
    const db = this;
    function wrapped(...args) {
      if (db._inTransaction) return fn(...args);
      const snapshot = db.serialize();
      db._inTransaction = true;
      try {
        return fn(...args);
      } catch (error) {
        db._db = Db.fromBytes(snapshot);
        throw error;
      } finally {
        db._inTransaction = false;
      }
    }
    wrapped.deferred = wrapped;
    wrapped.immediate = wrapped;
    wrapped.exclusive = wrapped;
    return wrapped;
  }

  close() {
    /* the WebAssembly instance is reclaimed by the GC */
  }
}

export default Database;
export { Database, Statement };
