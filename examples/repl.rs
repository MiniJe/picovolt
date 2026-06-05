//! `pvsql` — a tiny interactive SQL shell for PicoVolt.
//!
//! Run: `cargo run --release --example repl [workspace-path]`
//!
//! Opens (or creates) a development workspace and runs SQL you type, printing
//! result sets as a table. Data persists across runs in the workspace directory.

use picovolt::{Database, QueryResult, Value};
use std::error::Error;
use std::io::{self, BufRead, Write};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("pvsql_demo.pv")
            .to_string_lossy()
            .into_owned()
    });
    let mut db = Database::open_dev(&path)?;

    println!("pvsql — PicoVolt interactive shell");
    println!("workspace: {path}");
    println!("Type SQL, or `.help` for commands.\n");

    let mut input = io::stdin().lock();
    let mut out = io::stdout();
    let mut line = String::new();
    loop {
        print!("pv> ");
        out.flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D / Ctrl-Z)
        }
        let sql = line.trim();
        if sql.is_empty() {
            continue;
        }
        match sql {
            ".exit" | ".quit" => break,
            ".help" => print_help(),
            ".tables" => {
                let names = db.table_names();
                println!(
                    "{}",
                    if names.is_empty() {
                        "(no tables)".into()
                    } else {
                        names.join("  ")
                    }
                );
            }
            _ => match db.query(sql) {
                Ok(QueryResult::Rows { columns, rows }) => print_table(&columns, &rows),
                Ok(QueryResult::Mutated(n)) => println!("{n} row(s) affected"),
                Ok(QueryResult::Done) => println!("ok"),
                Err(e) => println!("error: {e}"),
            },
        }
    }
    println!("bye");
    Ok(())
}

fn print_table(columns: &[String], rows: &[Vec<Value>]) {
    let mut widths: Vec<usize> = columns.iter().map(String::len).collect();
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(ToString::to_string).collect())
        .collect();
    for row in &cells {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }

    let render = |fields: &[String]| -> String {
        fields
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{:width$}", f, width = widths.get(i).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    println!("{}", render(columns));
    println!(
        "{}",
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-")
    );
    for row in &cells {
        println!("{}", render(row));
    }
    println!(
        "({} row{})",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
}

fn print_help() {
    println!("Supported SQL:");
    println!("  CREATE TABLE t (c1, c2, ...)");
    println!("  CREATE INDEX ON t (col)");
    println!("  INSERT INTO t VALUES (v1, v2, ...)");
    println!("  SELECT * FROM t [WHERE col = val] [BEFORE tx] [LIMIT n]");
    println!("  UPDATE t SET col = val WHERE col2 = val2");
    println!("  DELETE FROM t WHERE col = val");
    println!("  DROP TABLE t");
    println!("Commands: .tables  .help  .exit");
}
