// PicoVolt in Node.js. Build first with the nodejs target:
//   wasm-pack build --target nodejs --release --out-dir js/pkg -- --features wasm
// then run:  node js/node-demo.cjs
const { Db } = require("./pkg/picovolt.js");

const db = new Db();
db.query("CREATE TABLE users (id, name, tier)");
db.query("INSERT INTO users VALUES (1, 'alice', 'free')");
db.query("INSERT INTO users VALUES (2, 'bob', 'pro')");
db.query("CREATE INDEX ON users (tier)");

console.log("all:", db.query("SELECT * FROM users"));
console.log("free:", db.query("SELECT * FROM users WHERE tier = 'free'"));
console.log("updated:", db.query("UPDATE users SET tier = 'pro' WHERE id = 1"));
console.log("exported .pvdb bytes:", db.export().length);
