#[cfg(feature = "full-text")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use picovolt::search::SearchIndex;
    use picovolt::{Database, Value};
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("Supply the signed guide dataset path")?;
    let query = args.next().unwrap_or_else(|| "transactions".into());
    let mut database = Database::open_prod(path)?;
    let result = database.query("SELECT title, url, searchable FROM guides ORDER BY url")?;
    let rows = result.rows().ok_or("Expected guide rows")?;
    let mut index = SearchIndex::new();
    for (id, row) in rows.iter().enumerate() {
        let Value::Text(text) = &row[2] else {
            return Err("Expected guide text".into());
        };
        index.upsert(id as i64, text)?;
    }
    for hit in index.search(&query, 10)? {
        println!(
            "{:.4}\t{:?}\t{:?}",
            hit.score, rows[hit.id as usize][0], rows[hit.id as usize][1]
        );
    }
    Ok(())
}

#[cfg(not(feature = "full-text"))]
fn main() {
    eprintln!("Enable --features full-text to run this example");
    std::process::exit(1);
}
