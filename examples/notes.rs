//! `pvnote`, a tiny notes store built on PicoVolt.
//!
//! Demonstrates idiomatic usage end to end: schema + inserts, CAS dedup of long
//! bodies, MVCC edit history with time-travel, and "publishing" (baking) to a
//! single read-only file that is reopened via mmap.
//!
//! Run with: `cargo run --release --example notes`

use picovolt::{Database, QueryResult, Value};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("notes.pv");
    let published = tmp.path().join("notes.pvdb");

    println!("== pvnote: a notes app on PicoVolt ==\n");

    // 1. Open (create) a development workspace and define a schema.
    let mut db = Database::open_dev(&workspace)?;
    db.query("CREATE TABLE notes (id, title, body, tag)")?;
    println!("created workspace at {}", workspace.display());

    // 2. Add notes. Bodies longer than 16 bytes are auto-interned into CAS, so
    //    the two identical "Terms" bodies are stored exactly once.
    let terms = "By using pvnote you agree to nothing in particular. ".repeat(3);
    add_note(&mut db, 1, "Welcome", "Thanks for trying PicoVolt!", "info")?;
    add_note(&mut db, 2, "Shopping", "eggs, milk, coffee", "todo")?;
    add_note(&mut db, 3, "Terms", &terms, "legal")?;
    add_note(&mut db, 4, "Terms (copy)", &terms, "legal")?;
    println!("added 4 notes (notes 3 & 4 share a body -> stored once via CAS)\n");

    // 3. List notes for a tag (filtering happens in app code over a scan).
    list_tag(&db, "legal")?;

    // 4. Edit note 2. Updates are append-only: tombstone the old version under a
    //    new transaction, then insert the new one.
    let before_edit = db.current_tx();
    db.delete("notes", "id", &Value::Int(2))?;
    add_note(&mut db, 2, "Shopping", "eggs, milk, coffee, bread", "todo")?;
    println!("edited note 2 (snapshot before edit = tx {before_edit})\n");

    // 5. Time-travel: compare the note now vs. before the edit.
    println!("note 2 body now:    {}", body_of(&db, 2, None)?);
    println!(
        "note 2 body before: {}",
        body_of(&db, 2, Some(before_edit))?
    );
    println!();

    // 6. Publish: compile the workspace into one read-only file, then reopen it.
    db.bake(&published)?;
    let size = std::fs::metadata(&published)?.len();
    println!("published -> {} ({size} bytes)", published.display());

    let mut prod = Database::open_prod(&published)?;
    if let QueryResult::Rows { rows, .. } = prod.query("SELECT * FROM notes")? {
        println!("reopened read-only; {} live note(s) visible", rows.len());
    }
    // Writes are rejected on a published database.
    let write = prod.query("INSERT INTO notes VALUES (9, 'x', 'y', 'z')");
    println!("attempting write on published db -> {}", describe(&write));

    println!("\nDone.");
    Ok(())
}

fn add_note(
    db: &mut Database,
    id: i64,
    title: &str,
    body: &str,
    tag: &str,
) -> Result<(), Box<dyn Error>> {
    db.insert(
        "notes",
        vec![
            Value::Int(id),
            Value::from(title),
            Value::from(body),
            Value::from(tag),
        ],
    )?;
    Ok(())
}

fn list_tag(db: &Database, tag: &str) -> Result<(), Box<dyn Error>> {
    let (_cols, rows) = db.select("notes", None)?;
    println!("notes tagged '{tag}':");
    for row in rows.iter().filter(|r| r[3] == Value::from(tag)) {
        println!("  #{} {}", row[0], row[1]);
    }
    println!();
    Ok(())
}

fn body_of(db: &Database, id: i64, before: Option<u64>) -> Result<String, Box<dyn Error>> {
    let (_cols, rows) = db.select("notes", before)?;
    Ok(rows
        .iter()
        .find(|r| r[0] == Value::Int(id))
        .map(|r| r[2].to_string())
        .unwrap_or_else(|| "<absent>".to_string()))
}

fn describe<T>(result: &picovolt::Result<T>) -> String {
    match result {
        Ok(_) => "accepted".to_string(),
        Err(e) => format!("rejected: {e}"),
    }
}
