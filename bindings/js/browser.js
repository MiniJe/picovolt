// Durable browser helper backed by the Origin Private File System (OPFS).
import { Db } from "./picovolt.js";

export class PersistentDb {
  constructor(name, db) {
    this.name = name;
    this.db = db;
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
    const json = params === undefined ? this.db.query(sql) : this.db.query(sql, params);
    return JSON.parse(json);
  }

  async save() {
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(this.name, { create: true });
    const writable = await handle.createWritable();
    await writable.write(this.db.export());
    await writable.close();
  }

  async close() {
    await this.save();
  }
}

export default PersistentDb;
