// Module-worker RPC endpoint. Use `new Worker(url, { type: "module" })` and
// send `{ id, method, ... }`; every response echoes `id` and carries result/error.
import { PersistentDb } from "./browser.js";

let database;
let nextStatementId = 1;
const statements = new Map();

self.addEventListener("message", async ({ data }) => {
  const { id, method } = data ?? {};
  try {
    let result;
    switch (method) {
      case "open":
        if (database) await database.close();
        database = undefined;
        statements.clear();
        database = await PersistentDb.open(data.name);
        result = true;
        break;
      case "query":
        if (!database) throw new Error("open the database first");
        result = database.query(data.sql, data.params);
        break;
      case "prepare": {
        if (!database) throw new Error("open the database first");
        const statement = database.prepare(data.sql);
        const statementId = nextStatementId++;
        statements.set(statementId, statement);
        result = { statementId, parameterCount: statement.parameterCount };
        break;
      }
      case "execute": {
        const statement = statements.get(data.statementId);
        if (!statement) throw new Error("unknown PicoVolt prepared statement");
        result = statement.query(data.params ?? []);
        break;
      }
      case "finalize": {
        const statement = statements.get(data.statementId);
        result = statement ? statement.close() : false;
        statements.delete(data.statementId);
        break;
      }
      case "save":
        if (!database) throw new Error("open the database first");
        await database.save();
        result = true;
        break;
      case "close":
        if (database) await database.close();
        database = undefined;
        statements.clear();
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
