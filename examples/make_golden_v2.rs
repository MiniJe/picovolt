//! Produce the v6 fixture without regenerating historical golden files.
use picovolt::{CommitLogOptions, Database, Value};

fn main() -> picovolt::Result<()> {
    let output = std::path::Path::new("tests/fixtures/golden_v2_0_0.pvdb");
    if output.exists() {
        return Err(picovolt::PvError::Transaction(
            "v2 golden already exists".into(),
        ));
    }
    let workspace = tempfile::tempdir()?;
    let mut db = Database::open_dev(workspace.path())?;
    db.enable_commit_log(CommitLogOptions::default())?;
    db.transaction(|db| {
        db.query(
            "CREATE TABLE users (id PRIMARY KEY, name TEXT DEFAULT 'anonymous', CHECK (id > 0))",
        )?;
        db.query("CREATE INDEX ON users (name)")?;
        db.query_with(
            "INSERT INTO users VALUES (?, ?)",
            &[
                Value::Int(1),
                Value::Text("a content addressed name".into()),
            ],
        )?;
        Ok(())
    })?;
    db.query("UPDATE users SET name = 'updated content addressed name' WHERE id = 1")?;
    db.bake(output)?;
    println!("{}", output.display());
    Ok(())
}
