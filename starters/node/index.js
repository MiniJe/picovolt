import Database from "picovolt/sqlite";

const db = new Database();
try {
  db.exec(`CREATE TABLE visits (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    source TEXT DEFAULT 'node' CHECK (source IN ('node', 'browser'))
  )`);

  const insert = db.prepare("INSERT INTO visits (id, path) VALUES (?, ?)");
  try {
    insert.run(1, "/");
    insert.run(2, "/docs");
    insert.run(3, "/download");
  } finally {
    insert.close();
  }

  const latest = db.prepare("SELECT * FROM visits ORDER BY id DESC LIMIT 2");
  try {
    console.table(latest.all());
  } finally {
    latest.close();
  }
} finally {
  db.close();
}
