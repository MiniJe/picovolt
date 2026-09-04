import assert from "node:assert/strict";

// A small OPFS-compatible in-memory store lets the release gate exercise the
// browser adapter and its public WASM artifact without a heavyweight browser.
const files = new Map();
const root = {
  async getFileHandle(name) {
    return {
      async getFile() {
        const bytes = files.get(name) ?? new Uint8Array();
        return {
          async arrayBuffer() {
            return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
          },
        };
      },
      async createWritable() {
        return {
          async write(value) {
            files.set(name, new Uint8Array(value));
          },
          async close() {},
        };
      },
    };
  },
};

Object.defineProperty(globalThis, "navigator", {
  configurable: true,
  value: { storage: { getDirectory: async () => root } },
});

const { PersistentDb } = await import("picovolt/browser");

const first = await PersistentDb.open("registry-smoke.pvdb");
first.query(`CREATE TABLE visits (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL,
  source TEXT DEFAULT 'browser' CHECK (source IN ('browser', 'worker'))
)`);
const insert = first.prepare("INSERT INTO visits (id, path) VALUES (?, ?)");
insert.query([1, "/from-public-npm"]);
insert.close();
await first.close();

assert.ok(files.get("registry-smoke.pvdb")?.byteLength > 0);
const reopened = await PersistentDb.open("registry-smoke.pvdb");
assert.deepEqual(reopened.query("SELECT * FROM visits"), {
  columns: ["id", "path", "source"],
  rows: [[1, "/from-public-npm", "browser"]],
});
await reopened.close();
console.log("browser package persisted and reopened successfully");
