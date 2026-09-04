use picovolt::{Database, Value};

fn main() -> picovolt::Result<()> {
    let mut db = Database::open_dev("starter.pv")?;
    db.query(
        "CREATE TABLE IF NOT EXISTS visits (\
         id INTEGER PRIMARY KEY, \
         path TEXT NOT NULL, \
         source TEXT DEFAULT 'rust' CHECK (source IN ('rust', 'python', 'go')))",
    )?;
    let next_id = db.row_count("visits", None)? as i64 + 1;
    let insert = db.prepare("INSERT INTO visits (id, path) VALUES (?, ?)")?;
    insert.execute(&mut db, &[Value::Int(next_id), Value::from("/")])?;
    println!(
        "{:?}",
        db.query("SELECT * FROM visits ORDER BY id DESC LIMIT 3")?
            .rows()
    );
    Ok(())
}
