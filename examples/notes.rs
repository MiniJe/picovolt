//! `pvnote`, a tiny notes store built on PicoVolt.
//!
//! Demonstrates idiomatic usage end to end: constraints, prepared writes, SQL
//! filtering, CAS dedup of long bodies, MVCC edit history with time-travel, and
//! "publishing" (baking) to a single read-only file that is reopened via mmap.
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
    db.query(
        "CREATE TABLE notes (\
         id INTEGER PRIMARY KEY, \
         title TEXT NOT NULL, \
         body TEXT NOT NULL, \
         tag TEXT DEFAULT 'info' CHECK (tag IN ('info', 'todo', 'legal')))",
    )?;
    println!("created workspace at {}", workspace.display());

    // 2. Add notes. Bodies longer than 16 bytes are auto-interned into CAS, so
    //    the two identical "Terms" bodies are stored exactly once.
    let terms = "By using pvnote you agree to nothing in particular. ".repeat(3);
    let insert = db.prepare("INSERT INTO notes VALUES (?, ?, ?, ?)")?;
    insert.execute(
        &mut db,
        &[
            Value::Int(1),
            Value::from("Welcome"),
            Value::from("Thanks for trying PicoVolt!"),
            Value::from("info"),
        ],
    )?;
    for (id, title, body, tag) in [
        (2, "Shopping", "eggs, milk, coffee", "todo"),
        (3, "Terms", terms.as_str(), "legal"),
        (4, "Terms (copy)", terms.as_str(), "legal"),
    ] {
        insert.execute(
            &mut db,
            &[
                Value::Int(id),
                Value::from(title),
                Value::from(body),
                Value::from(tag),
            ],
        )?;
    }
    println!("added 4 notes (notes 3 & 4 share a body -> stored once via CAS)\n");

    // 3. List notes for a tag with a bound SQL value.
    list_tag(&mut db, "legal")?;

    // 4. Edit note 2. The engine preserves its previous MVCC row version.
    let before_edit = db.current_tx();
    let update = db.prepare("UPDATE notes SET body = ? WHERE id = ?")?;
    update.execute(
        &mut db,
        &[Value::from("eggs, milk, coffee, bread"), Value::Int(2)],
    )?;
    println!("edited note 2 (snapshot before edit = tx {before_edit})\n");

    // 5. Time-travel: compare the note now vs. before the edit.
    println!("note 2 body now:    {}", body_of(&mut db, 2, None)?);
    println!(
        "note 2 body before: {}",
        body_of(&mut db, 2, Some(before_edit))?
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

fn list_tag(db: &mut Database, tag: &str) -> Result<(), Box<dyn Error>> {
    let result = db.query_with(
        "SELECT id, title FROM notes WHERE tag = ? ORDER BY id",
        &[Value::from(tag)],
    )?;
    let rows = result.rows().unwrap_or_default();
    println!("notes tagged '{tag}':");
    for row in rows {
        println!("  #{} {}", row[0], row[1]);
    }
    println!();
    Ok(())
}

fn body_of(db: &mut Database, id: i64, before: Option<u64>) -> Result<String, Box<dyn Error>> {
    let sql = match before {
        Some(tx) => format!("SELECT body FROM notes WHERE id = ? BEFORE {tx}"),
        None => "SELECT body FROM notes WHERE id = ?".to_string(),
    };
    let result = db.query_with(&sql, &[Value::Int(id)])?;
    let rows = result.rows().unwrap_or_default();
    Ok(rows
        .first()
        .map(|r| r[0].to_string())
        .unwrap_or_else(|| "<absent>".to_string()))
}

fn describe<T>(result: &picovolt::Result<T>) -> String {
    match result {
        Ok(_) => "accepted".to_string(),
        Err(e) => format!("rejected: {e}"),
    }
}
