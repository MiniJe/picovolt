//! Best-effort import of a SQL dump (such as the output of `sqlite3 mydb .dump`)
//! into PicoVolt.
//!
//! PicoVolt's SQL is a subset, so the importer rewrites what it can and skips
//! the rest rather than aborting: `CREATE TABLE` is reduced to column names
//! (types and constraints are dropped, since PicoVolt tables are untyped),
//! double-quoted identifiers are unquoted, and statement kinds the engine does
//! not support (PRAGMA, transactions, triggers, views, indexes, ALTER, ATTACH)
//! are skipped with a reason. Each statement that does run is reported, and a
//! statement that errors is collected rather than stopping the import.

use crate::Database;

/// The outcome of [`Database::import_sql`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    /// Number of statements executed successfully.
    pub executed: usize,
    /// Statements intentionally skipped, each as "reason: statement preview".
    pub skipped: Vec<String>,
    /// Statements that failed, each as "error: statement preview".
    pub errors: Vec<String>,
}

impl Database {
    /// Import a SQL dump, returning an [`ImportReport`]. See the module docs for
    /// the rewriting and skipping rules.
    pub fn import_sql(&mut self, dump: &str) -> ImportReport {
        let mut report = ImportReport::default();
        for raw in split_statements(dump) {
            let stmt = unquote_double_quotes(&raw);
            let stmt = stmt.trim();
            if stmt.is_empty() || stmt.starts_with("--") {
                continue;
            }
            match rewrite_statement(stmt) {
                Rewrite::Run(sql) => match self.query(&sql) {
                    Ok(_) => report.executed += 1,
                    Err(e) => report.errors.push(format!("{e}: {}", preview(stmt))),
                },
                Rewrite::Skip(reason) => {
                    report.skipped.push(format!("{reason}: {}", preview(stmt)))
                }
            }
        }
        report
    }
}

enum Rewrite {
    Run(String),
    Skip(String),
}

fn preview(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 60 {
        format!("{}...", one.chars().take(60).collect::<String>())
    } else {
        one
    }
}

fn rewrite_statement(stmt: &str) -> Rewrite {
    let upper = stmt.to_ascii_uppercase();
    for (kw, reason) in [
        ("PRAGMA", "pragma not supported"),
        ("BEGIN", "transactions not supported"),
        ("COMMIT", "transactions not supported"),
        ("ROLLBACK", "transactions not supported"),
        ("SAVEPOINT", "transactions not supported"),
        ("RELEASE", "transactions not supported"),
        ("CREATE TRIGGER", "triggers not supported"),
        ("CREATE VIEW", "views not supported"),
        ("CREATE INDEX", "dump index skipped"),
        ("CREATE UNIQUE INDEX", "dump index skipped"),
        ("ALTER", "ALTER not supported"),
        ("ATTACH", "ATTACH not supported"),
        ("DETACH", "DETACH not supported"),
        ("ANALYZE", "ANALYZE not supported"),
    ] {
        if upper.starts_with(kw) {
            return Rewrite::Skip(reason.to_string());
        }
    }
    if upper == "END" {
        // The tail of a trigger body, left behind when its inner `;` split it.
        return Rewrite::Skip("trigger body fragment".to_string());
    }
    if upper.starts_with("CREATE TABLE") {
        return match rewrite_create_table(stmt) {
            Some(sql) => Rewrite::Run(sql),
            None => Rewrite::Skip("could not parse CREATE TABLE".to_string()),
        };
    }
    // INSERT/UPDATE/DELETE/DROP and plain statements pass through (identifiers
    // were already unquoted by the caller).
    Rewrite::Run(stmt.to_string())
}

/// `CREATE TABLE [IF NOT EXISTS] name (col TYPE constraints, ..., table-constraint)`
/// becomes `CREATE TABLE name (col, ...)`.
fn rewrite_create_table(stmt: &str) -> Option<String> {
    let open = stmt.find('(')?;
    let name = stmt[..open].split_whitespace().last()?;
    let close = matching_paren(stmt, open)?;
    let body = &stmt[open + 1..close];
    let cols = column_names(body);
    if cols.is_empty() {
        return None;
    }
    Some(format!("CREATE TABLE {} ({})", name, cols.join(", ")))
}

fn column_names(body: &str) -> Vec<String> {
    let mut cols = Vec::new();
    for item in split_top_level_commas(body) {
        let item = item.trim();
        let upper = item.to_ascii_uppercase();
        if upper.starts_with("PRIMARY KEY")
            || upper.starts_with("FOREIGN KEY")
            || upper.starts_with("UNIQUE")
            || upper.starts_with("CHECK")
            || upper.starts_with("CONSTRAINT")
            || upper.starts_with("KEY ")
        {
            continue; // a table-level constraint, not a column definition
        }
        if let Some(name) = first_identifier(item) {
            cols.push(name);
        }
    }
    cols
}

fn first_identifier(s: &str) -> Option<String> {
    let id: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Replace `"identifier"` (and ``identifier``/`[identifier]`) outside string
/// literals with the bare identifier, since PicoVolt uses unquoted names.
fn unquote_double_quotes(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    in_str = false;
                }
            }
        } else if c == '\'' {
            in_str = true;
            out.push(c);
        } else if c == '"' || c == '`' || c == '[' {
            let close = if c == '[' { ']' } else { c };
            while let Some(n) = chars.next() {
                if n == close {
                    // "" escapes a quote inside a quoted identifier.
                    if close != ']' && chars.peek() == Some(&close) {
                        out.push(close);
                        chars.next();
                    } else {
                        break;
                    }
                } else {
                    out.push(n);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            cur.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    cur.push('\'');
                    chars.next();
                } else {
                    in_str = false;
                }
            }
        } else if c == '\'' {
            in_str = true;
            cur.push(c);
        } else if c == ';' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn split_top_level_commas(body: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            cur.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    cur.push('\'');
                    chars.next();
                } else {
                    in_str = false;
                }
            }
        } else {
            match c {
                '\'' => {
                    in_str = true;
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => items.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
    }
    if !cur.trim().is_empty() {
        items.push(cur);
    }
    items
}

fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    for (i, c) in s.char_indices().skip(open) {
        if in_str {
            if c == '\'' {
                in_str = false;
            }
        } else if c == '\'' {
            in_str = true;
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::value::Value;

    #[test]
    fn imports_a_sqlite_style_dump() {
        let dump = r#"
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE "users" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "name" TEXT NOT NULL DEFAULT 'anon',
  "score" REAL,
  UNIQUE("name")
);
INSERT INTO "users" VALUES(1,'alice',9.5);
INSERT INTO "users" VALUES(2,'o''brien',3.0);
CREATE INDEX idx_name ON users(name);
CREATE TRIGGER t AFTER INSERT ON users BEGIN SELECT 1; END;
COMMIT;
"#;
        let mut db = Database::open_memory();
        let report = db.import_sql(dump);
        assert_eq!(report.executed, 3, "create + 2 inserts: {report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
        // pragma, begin, index, trigger, commit are skipped.
        assert!(report.skipped.len() >= 4, "{report:?}");

        let rows = db
            .query("SELECT id, name FROM users ORDER BY id")
            .unwrap()
            .rows()
            .unwrap()
            .to_vec();
        assert_eq!(
            rows,
            vec![
                vec![Value::Int(1), Value::Text("alice".into())],
                vec![Value::Int(2), Value::Text("o'brien".into())],
            ]
        );
    }
}
