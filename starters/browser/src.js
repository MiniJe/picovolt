import { PersistentDb } from "picovolt/browser";

const db = await PersistentDb.open("starter.pvdb");
try {
  db.query(`CREATE TABLE IF NOT EXISTS visits (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    source TEXT DEFAULT 'browser' CHECK (source IN ('browser', 'worker'))
  )`);
  const insert = db.prepare("INSERT INTO visits (id, path) VALUES (?, ?)");
  try {
    insert.query([Date.now(), location.pathname]);
  } finally {
    insert.close();
  }
  const result = db.query("SELECT * FROM visits ORDER BY id DESC LIMIT 10");
  document.querySelector("#output").textContent = JSON.stringify(result, null, 2);
} finally {
  await db.close();
}
