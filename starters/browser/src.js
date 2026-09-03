import init from "picovolt";
import { PersistentDb } from "picovolt/browser";

await init();
const db = await PersistentDb.open("starter.pvdb");
let result;
try {
  result = db.query("SELECT * FROM visits");
} catch {
  db.query("CREATE TABLE visits (id PRIMARY KEY, path NOT NULL)");
  result = db.query("SELECT * FROM visits");
}
document.querySelector("#output").textContent = JSON.stringify(result, null, 2);
await db.save();
