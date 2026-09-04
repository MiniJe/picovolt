// Durable browser helper backed by the Origin Private File System (OPFS).
import { Db } from "./picovolt.js";

export class PersistentStatement {
  constructor(database, source) {
    this.database = database;
    this.source = source;
    this._prepared = database.db.prepare(source);
    this.parameterCount = this._prepared.parameterCount;
  }

  query(params = []) {
    if (!this._prepared) throw new Error("PicoVolt prepared statement is closed");
    this.database._assertOpen();
    return JSON.parse(this._prepared.execute(this.database.db, params));
  }

  close() {
    if (!this._prepared) return false;
    const prepared = this._prepared;
    this._prepared = undefined;
    this.database._statements.delete(this);
    prepared.free();
    return true;
  }

  finalize() {
    return this.close();
  }
}

export class PersistentDb {
  constructor(name, db) {
    this.name = name;
    this.db = db;
    this._closed = false;
    this._statements = new Set();
  }

  static async open(name = "picovolt.pvdb") {
    if (!globalThis.navigator?.storage?.getDirectory) {
      throw new Error("PicoVolt OPFS persistence is unavailable in this browser/context");
    }
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(name, { create: true });
    const file = await handle.getFile();
    const bytes = new Uint8Array(await file.arrayBuffer());
    const db = bytes.length ? Db.fromBytes(bytes) : new Db();
    return new PersistentDb(name, db);
  }

  query(sql, params) {
    this._assertOpen();
    const json = params === undefined ? this.db.query(sql) : this.db.query(sql, params);
    return JSON.parse(json);
  }

  prepare(sql) {
    this._assertOpen();
    const statement = new PersistentStatement(this, sql);
    this._statements.add(statement);
    return statement;
  }

  async save() {
    this._assertOpen();
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(this.name, { create: true });
    const writable = await handle.createWritable();
    await writable.write(this.db.export());
    await writable.close();
  }

  async close() {
    if (this._closed) return;
    await this.save();
    for (const statement of [...this._statements]) statement.close();
    this.db.free();
    this._closed = true;
  }

  _assertOpen() {
    if (this._closed) throw new Error("PicoVolt database is closed");
  }
}

export default PersistentDb;
