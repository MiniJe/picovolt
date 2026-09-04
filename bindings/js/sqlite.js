// A better-sqlite3-inspired synchronous API over PicoVolt's WebAssembly engine.
// It follows the familiar prepare/run/get/all shape while retaining PicoVolt's
// focused SQL surface:
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
    db._assertOpen();
    this._db = db;
    this.source = sql;
    this._prepared = db._db.prepare(sql);
    this.parameterCount = this._prepared.parameterCount;
    db._statements.add(this);
  }

  _exec(args) {
    if (!this._prepared) throw new Error("PicoVolt prepared statement is closed");
    this._db._assertOpen();
    const params = normalizeParams(args);
    const json = this._prepared.execute(this._db._db, params);
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

  close() {
    if (!this._prepared) return false;
    const prepared = this._prepared;
    this._prepared = undefined;
    this._db._statements.delete(this);
    prepared.free();
    return true;
  }

  finalize() {
    return this.close();
  }
}

class Database {
  constructor() {
    this._db = new Db();
    this._inTransaction = false;
    this._closed = false;
    this._statements = new Set();
  }

  prepare(sql) {
    return new Statement(this, sql);
  }

  // Run one or more `;`-separated statements with no bound parameters.
  exec(sql) {
    this._assertOpen();
    for (const stmt of sql.split(";").map((s) => s.trim()).filter(Boolean)) {
      this._db.query(stmt);
    }
    return this;
  }

  // The most recent committed transaction id (upper bound for `... BEFORE tx`).
  get currentTx() {
    this._assertOpen();
    return this._db.currentTx();
  }

  // Export the database as a `.pvdb` byte image (Uint8Array).
  serialize() {
    this._assertOpen();
    return this._db.export();
  }

  pragma() {
    throw new Error("picovolt: pragma is not supported");
  }

  // Wrap a synchronous callback in an engine transaction, matching
  // better-sqlite3's common pattern.
  transaction(fn) {
    if (typeof fn !== "function") throw new TypeError("transaction expects a function");
    this._assertOpen();
    const db = this;
    function wrapped(...args) {
      db._assertOpen();
      if (db._inTransaction) return fn(...args);
      db._db.beginTransaction();
      db._inTransaction = true;
      try {
        const value = fn(...args);
        db._db.commitTransaction();
        return value;
      } catch (error) {
        if (db._db.inTransaction()) db._db.rollbackTransaction();
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
    if (this._closed) return;
    for (const statement of [...this._statements]) statement.close();
    this._db.free();
    this._closed = true;
  }

  _assertOpen() {
    if (this._closed) throw new Error("PicoVolt database is closed");
  }
}

export default Database;
export { Database, Statement };
