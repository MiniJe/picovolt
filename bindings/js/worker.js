// Module-worker RPC endpoint. Use `new Worker(url, { type: "module" })` and
// send `{ id, method, ... }`; every response echoes `id` and carries result/error.
import { PersistentDb } from "./browser.js";

let database;

self.addEventListener("message", async ({ data }) => {
  const { id, method } = data ?? {};
  try {
    let result;
    switch (method) {
      case "open":
        database = await PersistentDb.open(data.name);
        result = true;
        break;
      case "query":
        if (!database) throw new Error("open the database first");
        result = database.query(data.sql, data.params);
        break;
      case "save":
        if (!database) throw new Error("open the database first");
        await database.save();
        result = true;
        break;
      case "close":
        if (database) await database.close();
        database = undefined;
        result = true;
        break;
      default:
        throw new Error(`unknown PicoVolt worker method: ${method}`);
    }
    self.postMessage({ id, result });
  } catch (error) {
    self.postMessage({ id, error: error instanceof Error ? error.message : String(error) });
  }
});
