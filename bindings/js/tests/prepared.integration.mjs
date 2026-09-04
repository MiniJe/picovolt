import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const packageDirectory = resolve(
  process.env.PICOVOLT_JS_PACKAGE_DIR ??
    fileURLToPath(new URL("../../../pkg/", import.meta.url)),
);

function packageModule(name) {
  return pathToFileURL(resolve(packageDirectory, name)).href;
}

function captureThrow(fn) {
  let thrown;
  try {
    fn();
  } catch (error) {
    thrown = error;
  }
  assert.notEqual(thrown, undefined, "expected the operation to throw");
  return String(thrown);
}

function installMemoryOpfs() {
  const files = new Map();
  const storage = {
    async getDirectory() {
      return {
        async getFileHandle(name, options = {}) {
          if (!files.has(name)) {
            if (!options.create) throw new Error(`file not found: ${name}`);
            files.set(name, new Uint8Array());
          }
          return {
            async getFile() {
              const bytes = files.get(name);
              return {
                async arrayBuffer() {
                  return bytes.buffer.slice(
                    bytes.byteOffset,
                    bytes.byteOffset + bytes.byteLength,
                  );
                },
              };
            },
            async createWritable() {
              return {
                async write(value) {
                  files.set(name, Uint8Array.from(value));
                },
                async close() {},
              };
            },
          };
        },
      };
    },
  };

  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: { storage },
  });
  return files;
}

test("raw Wasm prepared statements validate once and execute repeatedly", async () => {
  const { Db } = await import(packageModule("picovolt.js"));
  const db = new Db();
  try {
    db.query("CREATE TABLE contacts (id PRIMARY KEY, label)");
    const insert = db.prepare("INSERT INTO contacts VALUES (?, ?)");
    try {
      assert.equal(insert.parameterCount, 2);
      assert.deepEqual(JSON.parse(insert.execute(db, [1, "one"])), { mutated: 1 });
      assert.deepEqual(JSON.parse(insert.execute(db, [2, null])), { mutated: 1 });
      assert.match(captureThrow(() => insert.execute(db, [3])), /expects 2.*got 1/i);
      assert.match(captureThrow(() => insert.execute(db, "not-an-array")), /must be an array/i);
    } finally {
      insert.free();
    }

    const select = db.prepare("SELECT * FROM contacts ORDER BY id");
    try {
      const rows = JSON.parse(select.execute(db, []));
      assert.deepEqual(rows.rows, [
        [1, "one"],
        [2, null],
      ]);
    } finally {
      select.free();
    }
  } finally {
    db.free();
  }
});

test("sqlite adapter reuses statements and closes native resources", async () => {
  const { default: Database } = await import(packageModule("sqlite.js"));
  const db = new Database();
  db.exec("CREATE TABLE contacts (id PRIMARY KEY, label)");

  const insert = db.prepare("INSERT INTO contacts VALUES (?, ?)");
  assert.equal(insert.parameterCount, 2);
  assert.deepEqual(insert.run(1, "one"), { changes: 1 });
  assert.deepEqual(insert.run([2, "two"]), { changes: 1 });
  assert.match(captureThrow(() => insert.run(3)), /expects 2.*got 1/i);
  assert.equal(insert.finalize(), true);
  assert.equal(insert.close(), false);
  assert.match(captureThrow(() => insert.run(4, "four")), /statement is closed/i);

  const select = db.prepare("SELECT * FROM contacts ORDER BY id");
  assert.deepEqual(
    select.all().map((row) => ({ ...row })),
    [
      { id: 1, label: "one" },
      { id: 2, label: "two" },
    ],
  );
  db.close();
  assert.match(captureThrow(() => select.all()), /statement is closed/i);
  assert.match(captureThrow(() => db.prepare("SELECT * FROM contacts")), /database is closed/i);
  db.close();
});

test("browser adapter persists prepared writes and rejects local-file-style contexts", async () => {
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {},
  });
  const { PersistentDb } = await import(packageModule("browser.js"));
  await assert.rejects(
    PersistentDb.open("unavailable.pvdb"),
    /OPFS persistence is unavailable/i,
  );

  const files = installMemoryOpfs();
  const first = await PersistentDb.open("prepared.pvdb");
  first.query("CREATE TABLE contacts (id PRIMARY KEY, label)");
  const insert = first.prepare("INSERT INTO contacts VALUES (?, ?)");
  assert.equal(insert.parameterCount, 2);
  assert.deepEqual(insert.query([1, "one"]), { mutated: 1 });
  assert.deepEqual(insert.query([2, "two"]), { mutated: 1 });
  assert.match(captureThrow(() => insert.query([3])), /expects 2.*got 1/i);
  await first.close();
  assert.ok(files.get("prepared.pvdb").byteLength > 0);
  assert.match(captureThrow(() => insert.query([4, "four"])), /statement is closed/i);
  assert.match(captureThrow(() => first.query("SELECT * FROM contacts")), /database is closed/i);

  const reopened = await PersistentDb.open("prepared.pvdb");
  const select = reopened.prepare("SELECT * FROM contacts ORDER BY id");
  assert.deepEqual(select.query().rows, [
    [1, "one"],
    [2, "two"],
  ]);
  assert.equal(select.close(), true);
  assert.equal(select.finalize(), false);
  await reopened.close();
});

test("worker prepare, execute, finalize, reopen, and close have scoped lifetimes", async () => {
  installMemoryOpfs();
  let listener;
  let response;
  Object.defineProperty(globalThis, "self", {
    configurable: true,
    value: {
      addEventListener(type, callback) {
        assert.equal(type, "message");
        listener = callback;
      },
      postMessage(value) {
        response = value;
      },
    },
  });
  await import(`${packageModule("worker.js")}?prepared-test=${Date.now()}`);
  assert.equal(typeof listener, "function");

  async function rpc(method, fields = {}) {
    const id = `${method}-${Math.random()}`;
    response = undefined;
    await listener({ data: { id, method, ...fields } });
    assert.equal(response?.id, id);
    return response;
  }

  assert.equal((await rpc("open", { name: "worker-a.pvdb" })).result, true);
  assert.equal(
    (await rpc("query", { sql: "CREATE TABLE contacts (id PRIMARY KEY, label)" })).result.done,
    true,
  );
  const prepared = await rpc("prepare", {
    sql: "INSERT INTO contacts VALUES (?, ?)",
  });
  assert.equal(prepared.result.parameterCount, 2);
  assert.equal((await rpc("execute", { statementId: prepared.result.statementId, params: [1, "one"] })).result.mutated, 1);
  assert.equal((await rpc("execute", { statementId: prepared.result.statementId, params: [2, "two"] })).result.mutated, 1);
  assert.match(
    (await rpc("execute", { statementId: prepared.result.statementId, params: [3] })).error,
    /expects 2.*got 1/i,
  );
  assert.equal((await rpc("finalize", { statementId: prepared.result.statementId })).result, true);
  assert.match(
    (await rpc("execute", { statementId: prepared.result.statementId, params: [3, "three"] })).error,
    /unknown PicoVolt prepared statement/i,
  );
  assert.equal((await rpc("finalize", { statementId: prepared.result.statementId })).result, false);

  const stale = await rpc("prepare", { sql: "SELECT * FROM contacts" });
  assert.equal((await rpc("open", { name: "worker-b.pvdb" })).result, true);
  assert.match(
    (await rpc("execute", { statementId: stale.result.statementId })).error,
    /unknown PicoVolt prepared statement/i,
  );
  assert.equal((await rpc("close")).result, true);
  assert.match((await rpc("query", { sql: "SELECT * FROM contacts" })).error, /open the database first/i);

  assert.equal((await rpc("open", { name: "worker-a.pvdb" })).result, true);
  assert.deepEqual((await rpc("query", { sql: "SELECT * FROM contacts ORDER BY id" })).result.rows, [
    [1, "one"],
    [2, "two"],
  ]);
  await rpc("close");
});
