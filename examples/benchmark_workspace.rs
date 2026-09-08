//! Initialize a logged workspace for the Python competitor benchmark.
//! Initialization/schema creation is outside all reported timings.
use picovolt::{CommitLogOptions, Database};

fn main() -> picovolt::Result<()> {
    let path = std::env::args().nth(1).expect("workspace path argument");
    let mut db = Database::open_dev(path)?;
    db.enable_commit_log(CommitLogOptions::default())?;
    db.query("CREATE TABLE events (id INTEGER, bucket INTEGER, amount INTEGER, payload TEXT)")?;
    db.query("CREATE TABLE categories (bucket INTEGER, label TEXT)")?;
    Ok(())
}
