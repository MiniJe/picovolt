//! A deliberately small SQL front-end.
//!
//! PicoVolt's focus is the storage/MVCC engine, not query planning, so this
//! parser covers exactly the statement forms the specification demonstrates —
//! `CREATE TABLE`, `INSERT`, `SELECT * ... [BEFORE tx]`, and `DELETE`. Anything
//! richer is intentionally out of scope and reported as [`PvError::Query`].

use crate::core::errors::{PvError, Result};
use crate::core::value::Value;

/// A parsed statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// `CREATE TABLE name (col, col, ...)`
    CreateTable {
        /// Table name.
        name: String,
        /// Declared column names.
        columns: Vec<String>,
    },
    /// `INSERT INTO name VALUES (v, v, ...)`
    Insert {
        /// Target table.
        table: String,
        /// Row values, positional.
        values: Vec<Value>,
    },
    /// `CREATE INDEX ON name (col)`
    CreateIndex {
        /// Table to index.
        table: String,
        /// Column to index.
        column: String,
    },
    /// `SELECT <proj> FROM name [WHERE col = value] [BEFORE tx] [ORDER BY col [ASC|DESC]] [LIMIT n]`
    Select {
        /// Source table.
        table: String,
        /// What to return: `*`, a column list, or `COUNT(*)`.
        projection: Projection,
        /// Optional time-travel snapshot id.
        before: Option<u64>,
        /// Optional `column = value` equality filter.
        filter: Option<(String, Value)>,
        /// Optional sort.
        order: Option<OrderBy>,
        /// Optional cap on the number of rows returned.
        limit: Option<usize>,
    },
    /// `UPDATE name SET col = value WHERE col2 = value2`
    Update {
        /// Target table.
        table: String,
        /// Column to assign and its new value.
        set: (String, Value),
        /// Equality predicate selecting rows to update.
        filter: (String, Value),
    },
    /// `DELETE FROM name WHERE col = value`
    Delete {
        /// Target table.
        table: String,
        /// Predicate column.
        column: String,
        /// Predicate value (equality).
        value: Value,
    },
    /// `DROP TABLE name`
    DropTable {
        /// Table to drop.
        table: String,
    },
}

/// What a `SELECT` returns.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// `*` — every column.
    All,
    /// A specific list of columns.
    Columns(Vec<String>),
    /// `COUNT(*)` — a single-row, single-column count.
    Count,
}

/// An `ORDER BY column [ASC|DESC]` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    /// Column to sort on.
    pub column: String,
    /// Descending if `true`, ascending otherwise.
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Int(i64),
    LParen,
    RParen,
    Comma,
    Eq,
    Star,
}

fn tokenize(sql: &str) -> Result<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ws if ws.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            ',' => {
                chars.next();
                toks.push(Tok::Comma);
            }
            '=' => {
                chars.next();
                toks.push(Tok::Eq);
            }
            '*' => {
                chars.next();
                toks.push(Tok::Star);
            }
            ';' => {
                chars.next(); // statement terminator, ignored
            }
            '\'' => {
                chars.next(); // opening quote
                let mut s = String::new();
                let mut closed = false;
                loop {
                    match chars.next() {
                        // A doubled quote `''` is an escaped literal `'` (SQL style).
                        Some('\'') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            s.push('\'');
                        }
                        Some('\'') => {
                            closed = true;
                            break;
                        }
                        Some(ch) => s.push(ch),
                        None => break,
                    }
                }
                if !closed {
                    return Err(PvError::Query("unterminated string literal".into()));
                }
                toks.push(Tok::Str(s));
            }
            '-' | '0'..='9' => {
                let mut num = String::new();
                if c == '-' {
                    num.push(c);
                    chars.next();
                }
                let mut saw_digit = false;
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        num.push(d);
                        saw_digit = true;
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !saw_digit {
                    return Err(PvError::Query("expected digits after '-'".into()));
                }
                let v: i64 = num
                    .parse()
                    .map_err(|_| PvError::Query(format!("invalid integer: {num}")))?;
                toks.push(Tok::Int(v));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut w = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        w.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Word(w));
            }
            other => return Err(PvError::Query(format!("unexpected character: {other:?}"))),
        }
    }
    Ok(toks)
}

/// Cursor over a token stream with small typed consumers.
struct Cursor {
    toks: Vec<Tok>,
    pos: usize,
}

impl Cursor {
    fn new(toks: Vec<Tok>) -> Self {
        Self { toks, pos: 0 }
    }

    fn next(&mut self) -> Result<Tok> {
        let t = self
            .toks
            .get(self.pos)
            .cloned()
            .ok_or_else(|| PvError::Query("unexpected end of statement".into()))?;
        self.pos += 1;
        Ok(t)
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    /// Consume a keyword (case-insensitive), erroring if it doesn't match.
    fn keyword(&mut self, kw: &str) -> Result<()> {
        match self.next()? {
            Tok::Word(w) if w.eq_ignore_ascii_case(kw) => Ok(()),
            other => Err(PvError::Query(format!("expected `{kw}`, found {other:?}"))),
        }
    }

    fn ident(&mut self) -> Result<String> {
        match self.next()? {
            Tok::Word(w) => Ok(w),
            other => Err(PvError::Query(format!(
                "expected identifier, found {other:?}"
            ))),
        }
    }

    fn expect(&mut self, tok: Tok) -> Result<()> {
        let got = self.next()?;
        if got == tok {
            Ok(())
        } else {
            Err(PvError::Query(format!("expected {tok:?}, found {got:?}")))
        }
    }

    fn value(&mut self) -> Result<Value> {
        match self.next()? {
            Tok::Int(i) => Ok(Value::Int(i)),
            Tok::Str(s) => Ok(Value::Text(s)),
            Tok::Word(w) if w.eq_ignore_ascii_case("null") => Ok(Value::Null),
            other => Err(PvError::Query(format!("expected a value, found {other:?}"))),
        }
    }

    fn finish(&self) -> Result<()> {
        if self.pos == self.toks.len() {
            Ok(())
        } else {
            Err(PvError::Query("trailing tokens after statement".into()))
        }
    }
}

/// Parse a single SQL statement.
pub fn parse(sql: &str) -> Result<Statement> {
    let mut cur = Cursor::new(tokenize(sql)?);
    let stmt = match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("create") => parse_create(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("insert") => parse_insert(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("select") => parse_select(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("update") => parse_update(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("delete") => parse_delete(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("drop") => parse_drop(&mut cur)?,
        other => return Err(PvError::Query(format!("unsupported statement: {other:?}"))),
    };
    cur.finish()?;
    Ok(stmt)
}

fn parse_create(cur: &mut Cursor) -> Result<Statement> {
    match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("table") => {
            let name = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let mut columns = Vec::new();
            loop {
                columns.push(cur.ident()?);
                match cur.next()? {
                    Tok::Comma => continue,
                    Tok::RParen => break,
                    other => {
                        return Err(PvError::Query(format!(
                            "expected ',' or ')', found {other:?}"
                        )))
                    }
                }
            }
            Ok(Statement::CreateTable { name, columns })
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("index") => {
            cur.keyword("on")?;
            let table = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let column = cur.ident()?;
            cur.expect(Tok::RParen)?;
            Ok(Statement::CreateIndex { table, column })
        }
        other => Err(PvError::Query(format!(
            "expected TABLE or INDEX after CREATE, found {other:?}"
        ))),
    }
}

fn parse_insert(cur: &mut Cursor) -> Result<Statement> {
    cur.keyword("into")?;
    let table = cur.ident()?;
    cur.keyword("values")?;
    cur.expect(Tok::LParen)?;
    let mut values = Vec::new();
    loop {
        values.push(cur.value()?);
        match cur.next()? {
            Tok::Comma => continue,
            Tok::RParen => break,
            other => {
                return Err(PvError::Query(format!(
                    "expected ',' or ')', found {other:?}"
                )))
            }
        }
    }
    Ok(Statement::Insert { table, values })
}

fn parse_projection(cur: &mut Cursor) -> Result<Projection> {
    if matches!(cur.peek(), Some(Tok::Star)) {
        cur.next()?;
        return Ok(Projection::All);
    }
    if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("count")) {
        cur.next()?; // COUNT
        cur.expect(Tok::LParen)?;
        cur.expect(Tok::Star)?;
        cur.expect(Tok::RParen)?;
        return Ok(Projection::Count);
    }
    let mut columns = vec![cur.ident()?];
    while matches!(cur.peek(), Some(Tok::Comma)) {
        cur.next()?;
        columns.push(cur.ident()?);
    }
    Ok(Projection::Columns(columns))
}

fn parse_select(cur: &mut Cursor) -> Result<Statement> {
    let projection = parse_projection(cur)?;
    cur.keyword("from")?;
    let table = cur.ident()?;

    let mut filter = None;
    if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("where")) {
        cur.next()?; // consume WHERE
        let column = cur.ident()?;
        cur.expect(Tok::Eq)?;
        let value = cur.value()?;
        filter = Some((column, value));
    }

    let before = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("before")) {
        cur.next()?; // consume BEFORE
        match cur.next()? {
            Tok::Int(i) if i >= 0 => Some(i as u64),
            other => {
                return Err(PvError::Query(format!(
                    "BEFORE expects a non-negative integer, found {other:?}"
                )))
            }
        }
    } else {
        None
    };

    let order = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("order")) {
        cur.next()?; // ORDER
        cur.keyword("by")?;
        let column = cur.ident()?;
        let descending = match cur.peek() {
            Some(Tok::Word(w)) if w.eq_ignore_ascii_case("desc") => {
                cur.next()?;
                true
            }
            Some(Tok::Word(w)) if w.eq_ignore_ascii_case("asc") => {
                cur.next()?;
                false
            }
            _ => false,
        };
        Some(OrderBy { column, descending })
    } else {
        None
    };

    let limit = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("limit")) {
        cur.next()?; // consume LIMIT
        match cur.next()? {
            Tok::Int(i) if i >= 0 => Some(i as usize),
            other => {
                return Err(PvError::Query(format!(
                    "LIMIT expects a non-negative integer, found {other:?}"
                )))
            }
        }
    } else {
        None
    };

    Ok(Statement::Select {
        table,
        projection,
        before,
        filter,
        order,
        limit,
    })
}

fn parse_update(cur: &mut Cursor) -> Result<Statement> {
    let table = cur.ident()?;
    cur.keyword("set")?;
    let set_column = cur.ident()?;
    cur.expect(Tok::Eq)?;
    let set_value = cur.value()?;
    cur.keyword("where")?;
    let where_column = cur.ident()?;
    cur.expect(Tok::Eq)?;
    let where_value = cur.value()?;
    Ok(Statement::Update {
        table,
        set: (set_column, set_value),
        filter: (where_column, where_value),
    })
}

fn parse_drop(cur: &mut Cursor) -> Result<Statement> {
    cur.keyword("table")?;
    let table = cur.ident()?;
    Ok(Statement::DropTable { table })
}

fn parse_delete(cur: &mut Cursor) -> Result<Statement> {
    cur.keyword("from")?;
    let table = cur.ident()?;
    cur.keyword("where")?;
    let column = cur.ident()?;
    cur.expect(Tok::Eq)?;
    let value = cur.value()?;
    Ok(Statement::Delete {
        table,
        column,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table() {
        assert_eq!(
            parse("CREATE TABLE users (id, name, status)").unwrap(),
            Statement::CreateTable {
                name: "users".into(),
                columns: vec!["id".into(), "name".into(), "status".into()],
            }
        );
    }

    #[test]
    fn parses_insert_with_mixed_literals() {
        assert_eq!(
            parse("INSERT INTO users VALUES (1, 'alice', NULL)").unwrap(),
            Statement::Insert {
                table: "users".into(),
                values: vec![Value::Int(1), Value::Text("alice".into()), Value::Null],
            }
        );
    }

    #[test]
    fn parses_select_with_and_without_time_travel() {
        assert_eq!(
            parse("SELECT * FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                before: None,
                filter: None,
                order: None,
                limit: None,
            }
        );
        assert_eq!(
            parse("SELECT * FROM users BEFORE 7;").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                before: Some(7),
                filter: None,
                order: None,
                limit: None,
            }
        );
    }

    #[test]
    fn parses_select_with_where_before_and_limit() {
        assert_eq!(
            parse("SELECT * FROM users WHERE status = 'active'").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                before: None,
                filter: Some(("status".into(), Value::Text("active".into()))),
                order: None,
                limit: None,
            }
        );
        assert_eq!(
            parse("SELECT * FROM users WHERE id = 5 BEFORE 9 LIMIT 10").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                before: Some(9),
                filter: Some(("id".into(), Value::Int(5))),
                order: None,
                limit: Some(10),
            }
        );
    }

    #[test]
    fn parses_projection_order_and_count() {
        // Column projection.
        assert_eq!(
            parse("SELECT id, name FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Columns(vec!["id".into(), "name".into()]),
                before: None,
                filter: None,
                order: None,
                limit: None,
            }
        );
        // COUNT(*).
        assert!(matches!(
            parse("SELECT COUNT(*) FROM users").unwrap(),
            Statement::Select {
                projection: Projection::Count,
                ..
            }
        ));
        // ORDER BY ... DESC.
        assert_eq!(
            parse("SELECT * FROM users ORDER BY name DESC").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                before: None,
                filter: None,
                order: Some(OrderBy {
                    column: "name".into(),
                    descending: true,
                }),
                limit: None,
            }
        );
    }

    #[test]
    fn parses_update_and_drop() {
        assert_eq!(
            parse("UPDATE users SET status = 'gone' WHERE id = 3").unwrap(),
            Statement::Update {
                table: "users".into(),
                set: ("status".into(), Value::Text("gone".into())),
                filter: ("id".into(), Value::Int(3)),
            }
        );
        assert_eq!(
            parse("DROP TABLE users").unwrap(),
            Statement::DropTable {
                table: "users".into()
            }
        );
    }

    #[test]
    fn parses_create_index() {
        assert_eq!(
            parse("CREATE INDEX ON users (status)").unwrap(),
            Statement::CreateIndex {
                table: "users".into(),
                column: "status".into(),
            }
        );
    }

    #[test]
    fn parses_delete() {
        assert_eq!(
            parse("DELETE FROM users WHERE id = 1").unwrap(),
            Statement::Delete {
                table: "users".into(),
                column: "id".into(),
                value: Value::Int(1),
            }
        );
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        // `''` inside a string literal is an escaped single quote.
        assert_eq!(
            parse("INSERT INTO t VALUES ('it''s done')").unwrap(),
            Statement::Insert {
                table: "t".into(),
                values: vec![Value::Text("it's done".into())],
            }
        );
        // An unterminated literal is still an error.
        assert!(parse("INSERT INTO t VALUES ('oops)").is_err());
    }

    #[test]
    fn rejects_garbage_and_unsupported() {
        assert!(parse("TRUNCATE users").is_err());
        assert!(parse("SELECT * FROM").is_err());
        assert!(parse("INSERT INTO t VALUES (1,").is_err());
        assert!(parse("UPDATE t SET a = 1").is_err()); // missing WHERE
    }
}
