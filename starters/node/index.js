import Database from "picovolt/sqlite";

const db = new Database();
db.exec("CREATE TABLE visits (id PRIMARY KEY, path NOT NULL)");

const insert = db.prepare("INSERT INTO visits VALUES (?, ?)");
insert.run(1, "/");
insert.run(2, "/docs");
insert.run(3, "/download");

const latest = db.prepare("SELECT * FROM visits ORDER BY id LIMIT 2").all();
console.table(latest);
