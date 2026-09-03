use picovolt::{Database, Value};

fn main() -> picovolt::Result<()> {
    let mut db = Database::open_dev("starter.pv")?;
    if !db.table_names().iter().any(|name| name == "visits") {
        db.query("CREATE TABLE visits (id PRIMARY KEY, path NOT NULL)")?;
    }
    let insert = db.prepare("INSERT INTO visits VALUES (?, ?)")?;
    insert.execute(&mut db, &[Value::Int(1), Value::from("/")])?;
    println!("{:?}", db.query("SELECT * FROM visits")?.rows());
    Ok(())
}
