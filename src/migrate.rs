//! Best-effort import of a SQL dump (such as the output of `sqlite3 mydb .dump`)
//! into PicoVolt.
//!
//! PicoVolt's SQL is a subset, so the importer rewrites what it can and skips
//! the rest rather than aborting: `CREATE TABLE` is reduced to column names
//! (types and constraints are dropped, since PicoVolt tables are untyped),
//! quoted identifiers are preserved, and statement kinds the engine does not
//! support (PRAGMA, transactions, triggers, views, indexes, ALTER, ATTACH) are
//! skipped with a reason. Each statement that does run is reported, and a
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
            let stmt = raw.trim();
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
    let open = first_unquoted_paren(stmt)?;
    let name = create_table_name(&stmt[..open])?;
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
        if let Some((name, quoted, _)) = first_identifier(item) {
            let keyword = name.to_ascii_uppercase();
            if !quoted
                && matches!(
                    keyword.as_str(),
                    "PRIMARY" | "FOREIGN" | "UNIQUE" | "CHECK" | "CONSTRAINT" | "KEY"
                )
            {
                continue; // a table-level constraint, not a column definition
            }
            cols.push(name);
        }
    }
    cols
}

/// Return the first identifier as SQL source, whether it was delimited, and the
/// byte offset immediately after it. Keeping the delimiters means a quoted
/// keyword remains an identifier when the rewritten statement is parsed.
fn first_identifier(s: &str) -> Option<(String, bool, usize)> {
    let trimmed = s.trim_start();
    let leading = s.len() - trimmed.len();
    let mut chars = trimmed.char_indices().peekable();
    let (_, first) = chars.next()?;
    if let Some(closing) = quote_closing(first) {
        let mut has_content = false;
        while let Some((index, ch)) = chars.next() {
            if ch == closing {
                if chars.peek().is_some_and(|(_, next)| *next == closing) {
                    chars.next();
                    has_content = true;
                    continue;
                }
                if !has_content {
                    return None;
                }
                let end = index + ch.len_utf8();
                return Some((trimmed[..end].to_owned(), true, leading + end));
            }
            has_content = true;
        }
        None
    } else {
        if !(first.is_alphabetic() || first == '_') {
            return None;
        }
        let end = trimmed
            .char_indices()
            .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
            .map(|(index, ch)| index + ch.len_utf8())
            .last()?;
        Some((trimmed[..end].to_owned(), false, leading + end))
    }
}

fn create_table_name(head: &str) -> Option<String> {
    let mut rest = strip_keyword(head, "CREATE")?;
    rest = strip_keyword(rest, "TABLE")?;
    if starts_keyword(rest, "IF") {
        rest = strip_keyword(rest, "IF")?;
        rest = strip_keyword(rest, "NOT")?;
        rest = strip_keyword(rest, "EXISTS")?;
    }
    let (name, _, consumed) = first_identifier(rest)?;
    if !rest[consumed..].trim().is_empty() {
        return None;
    }
    Some(name)
}

fn starts_keyword(input: &str, keyword: &str) -> bool {
    strip_keyword(input, keyword).is_some()
}

fn strip_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let trimmed = input.trim_start();
    let prefix = trimmed.get(..keyword.len())?;
    if !prefix.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &trimmed[keyword.len()..];
    if rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(rest)
}

fn quote_closing(opening: char) -> Option<char> {
    match opening {
        '\'' => Some('\''),
        '"' => Some('"'),
        '`' => Some('`'),
        '[' => Some(']'),
        _ => None,
    }
}

fn first_unquoted_paren(sql: &str) -> Option<usize> {
    let mut quoted = None;
    let mut chars = sql.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if let Some(closing) = quoted {
            if ch == closing {
                if chars.peek().is_some_and(|(_, next)| *next == closing) {
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else if let Some(closing) = quote_closing(ch) {
            quoted = Some(closing);
        } else if ch == '(' {
            return Some(index);
        }
    }
    None
}

fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(closing) = quoted {
            cur.push(c);
            if c == closing {
                if chars.peek() == Some(&closing) {
                    cur.push(closing);
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else if let Some(closing) = quote_closing(c) {
            quoted = Some(closing);
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
    let mut quoted = None;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(closing) = quoted {
            cur.push(c);
            if c == closing {
                if chars.peek() == Some(&closing) {
                    cur.push(closing);
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else {
            match c {
                ch if quote_closing(ch).is_some() => {
                    quoted = quote_closing(ch);
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
    let mut quoted = None;
    let mut chars = s
        .char_indices()
        .skip_while(|(index, _)| *index < open)
        .peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(closing) = quoted {
            if c == closing {
                if chars.peek().is_some_and(|(_, next)| *next == closing) {
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else if let Some(closing) = quote_closing(c) {
            quoted = Some(closing);
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
