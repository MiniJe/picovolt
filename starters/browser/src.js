import { PersistentDb } from "picovolt/browser";

const db = await PersistentDb.open("starter.pvdb");
db.query("CREATE TABLE IF NOT EXISTS visits (id PRIMARY KEY, path NOT NULL)");
const insert = db.prepare("INSERT INTO visits VALUES (?, ?)");
insert.query([Date.now(), location.pathname]);
insert.close();
const result = db.query("SELECT * FROM visits ORDER BY id DESC LIMIT 10");
document.querySelector("#output").textContent = JSON.stringify(result, null, 2);
await db.close();
