//! A deliberately small SQL front-end.
//!
//! PicoVolt's focus is the storage/MVCC engine, not query planning, so this is a
//! compact hand-written tokenizer + recursive-descent parser. It covers
//! `CREATE TABLE`/`INDEX`, `INSERT`, `DROP TABLE`, `UPDATE`/`DELETE`, and
//! `SELECT` with column/aggregate projection, `WHERE` predicates (comparison
//! operators, `AND`/`OR`, `LIKE`), `BEFORE tx` time-travel, `ORDER BY`, and
//! `LIMIT`. Anything beyond that (joins, subqueries, `GROUP BY`) is intentionally
//! out of scope and reported as [`PvError::Query`].

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
    /// `SELECT <proj> FROM name [WHERE <pred>] [BEFORE tx] [ORDER BY col [ASC|DESC]] [LIMIT n]`
    Select {
        /// Source table.
        table: String,
        /// What to return: `*`, a column list, or aggregates.
        projection: Projection,
        /// Optional time-travel snapshot id.
        before: Option<u64>,
        /// Optional `WHERE` predicate.
        filter: Option<Predicate>,
        /// Optional sort.
        order: Option<OrderBy>,
        /// Optional cap on the number of rows returned.
        limit: Option<usize>,
    },
    /// `UPDATE name SET col = value WHERE <pred>`
    Update {
        /// Target table.
        table: String,
        /// Column to assign and its new value.
        set: (String, Value),
        /// Predicate selecting rows to update.
        filter: Predicate,
    },
    /// `DELETE FROM name WHERE <pred>`
    Delete {
        /// Target table.
        table: String,
        /// Predicate selecting rows to delete.
        filter: Predicate,
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
    /// One or more aggregate terms (`COUNT(*)`, `SUM(col)`, …) over the whole
    /// (filtered) result — produces a single row.
    Aggregates(Vec<Aggregate>),
}

/// An aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    /// Row / non-null count.
    Count,
    /// Sum of integer values.
    Sum,
    /// Minimum value (any comparable type).
    Min,
    /// Maximum value (any comparable type).
    Max,
}

/// One aggregate term, e.g. `SUM(amount)`. `column` is `None` only for `COUNT(*)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// Which aggregate function.
    pub func: AggFunc,
    /// Target column, or `None` for `COUNT(*)`.
    pub column: Option<String>,
}

/// A comparison operator in a `WHERE` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    /// `=`
    Eq,
    /// `!=` / `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `LIKE` (`%` = any run, `_` = any single char)
    Like,
}

/// A `WHERE` predicate: comparisons combined with `AND` / `OR`. `AND` binds
/// tighter than `OR`; parentheses override precedence.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// `column <op> value`
    Compare {
        /// Column on the left.
        column: String,
        /// The comparison.
        op: CompareOp,
        /// Literal on the right.
        value: Value,
    },
    /// `a AND b`
    And(Box<Predicate>, Box<Predicate>),
    /// `a OR b`
    Or(Box<Predicate>, Box<Predicate>),
}

impl Predicate {
    /// Convenience constructor for `column = value`.
    pub fn eq(column: impl Into<String>, value: Value) -> Self {
        Predicate::Compare {
            column: column.into(),
            op: CompareOp::Eq,
            value,
        }
    }
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
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
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
            '<' => {
                chars.next();
                match chars.peek() {
                    Some('=') => {
                        chars.next();
                        toks.push(Tok::Le);
                    }
                    Some('>') => {
                        chars.next();
                        toks.push(Tok::Ne);
                    }
                    _ => toks.push(Tok::Lt),
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Tok::Ge);
                } else {
                    toks.push(Tok::Gt);
                }
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Tok::Ne);
                } else {
                    return Err(PvError::Query("expected `=` after `!`".into()));
                }
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

    fn peek2(&self) -> Option<&Tok> {
        self.toks.get(self.pos + 1)
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

fn agg_func(word: &str) -> Option<AggFunc> {
    match word.to_ascii_uppercase().as_str() {
        "COUNT" => Some(AggFunc::Count),
        "SUM" => Some(AggFunc::Sum),
        "MIN" => Some(AggFunc::Min),
        "MAX" => Some(AggFunc::Max),
        _ => None,
    }
}

fn parse_projection(cur: &mut Cursor) -> Result<Projection> {
    if matches!(cur.peek(), Some(Tok::Star)) {
        cur.next()?;
        return Ok(Projection::All);
    }
    // Aggregate projection: a known function name immediately followed by `(`.
    let is_agg = matches!(cur.peek(), Some(Tok::Word(w)) if agg_func(w).is_some())
        && matches!(cur.peek2(), Some(Tok::LParen));
    if is_agg {
        let mut aggs = vec![parse_aggregate(cur)?];
        while matches!(cur.peek(), Some(Tok::Comma)) {
            cur.next()?;
            aggs.push(parse_aggregate(cur)?);
        }
        return Ok(Projection::Aggregates(aggs));
    }
    let mut columns = vec![cur.ident()?];
    while matches!(cur.peek(), Some(Tok::Comma)) {
        cur.next()?;
        columns.push(cur.ident()?);
    }
    Ok(Projection::Columns(columns))
}

fn parse_aggregate(cur: &mut Cursor) -> Result<Aggregate> {
    let word = cur.ident()?;
    let func =
        agg_func(&word).ok_or_else(|| PvError::Query(format!("unknown aggregate `{word}`")))?;
    cur.expect(Tok::LParen)?;
    let column = if matches!(cur.peek(), Some(Tok::Star)) {
        cur.next()?;
        None
    } else {
        Some(cur.ident()?)
    };
    cur.expect(Tok::RParen)?;
    if column.is_none() && func != AggFunc::Count {
        return Err(PvError::Query(
            "only COUNT(*) may use `*`; SUM/MIN/MAX need a column".into(),
        ));
    }
    Ok(Aggregate { func, column })
}

/// Parse a `WHERE` predicate (entry point: lowest-precedence `OR`).
fn parse_predicate(cur: &mut Cursor) -> Result<Predicate> {
    let mut left = parse_and(cur)?;
    while matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("or")) {
        cur.next()?;
        let right = parse_and(cur)?;
        left = Predicate::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(cur: &mut Cursor) -> Result<Predicate> {
    let mut left = parse_comparison(cur)?;
    while matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("and")) {
        cur.next()?;
        let right = parse_comparison(cur)?;
        left = Predicate::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_comparison(cur: &mut Cursor) -> Result<Predicate> {
    // Parenthesised sub-predicate.
    if matches!(cur.peek(), Some(Tok::LParen)) {
        cur.next()?;
        let inner = parse_predicate(cur)?;
        cur.expect(Tok::RParen)?;
        return Ok(inner);
    }
    let column = cur.ident()?;
    let op = match cur.next()? {
        Tok::Eq => CompareOp::Eq,
        Tok::Ne => CompareOp::Ne,
        Tok::Lt => CompareOp::Lt,
        Tok::Le => CompareOp::Le,
        Tok::Gt => CompareOp::Gt,
        Tok::Ge => CompareOp::Ge,
        Tok::Word(w) if w.eq_ignore_ascii_case("like") => CompareOp::Like,
        other => {
            return Err(PvError::Query(format!(
                "expected a comparison operator, found {other:?}"
            )))
        }
    };
    let value = cur.value()?;
    Ok(Predicate::Compare { column, op, value })
}

fn parse_select(cur: &mut Cursor) -> Result<Statement> {
    let projection = parse_projection(cur)?;
    cur.keyword("from")?;
    let table = cur.ident()?;

    let filter = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("where")) {
        cur.next()?; // consume WHERE
        Some(parse_predicate(cur)?)
    } else {
        None
    };

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
    let filter = parse_predicate(cur)?;
    Ok(Statement::Update {
        table,
        set: (set_column, set_value),
        filter,
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
    let filter = parse_predicate(cur)?;
    Ok(Statement::Delete { table, filter })
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
                filter: Some(Predicate::eq("status", Value::Text("active".into()))),
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
                filter: Some(Predicate::eq("id", Value::Int(5))),
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
        assert_eq!(
            parse("SELECT COUNT(*) FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Aggregates(vec![Aggregate {
                    func: AggFunc::Count,
                    column: None,
                }]),
                before: None,
                filter: None,
                order: None,
                limit: None,
            }
        );
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
                filter: Predicate::eq("id", Value::Int(3)),
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
                filter: Predicate::eq("id", Value::Int(1)),
            }
        );
    }

    #[test]
    fn and_binds_tighter_than_or() {
        use CompareOp::*;
        // a = 1 OR b > 2 AND c <= 3  parses as  a=1 OR (b>2 AND c<=3)
        let filter = match parse("SELECT * FROM t WHERE a = 1 OR b > 2 AND c <= 3").unwrap() {
            Statement::Select {
                filter: Some(p), ..
            } => p,
            other => panic!("expected select with filter, got {other:?}"),
        };
        assert_eq!(
            filter,
            Predicate::Or(
                Box::new(Predicate::Compare {
                    column: "a".into(),
                    op: Eq,
                    value: Value::Int(1)
                }),
                Box::new(Predicate::And(
                    Box::new(Predicate::Compare {
                        column: "b".into(),
                        op: Gt,
                        value: Value::Int(2)
                    }),
                    Box::new(Predicate::Compare {
                        column: "c".into(),
                        op: Le,
                        value: Value::Int(3)
                    }),
                )),
            )
        );
    }

    #[test]
    fn parens_override_precedence_like_and_ne() {
        use CompareOp::*;
        let filter = match parse("DELETE FROM t WHERE (a = 1 OR b = 2) AND name LIKE 'a%'").unwrap()
        {
            Statement::Delete { filter, .. } => filter,
            other => panic!("expected delete, got {other:?}"),
        };
        assert_eq!(
            filter,
            Predicate::And(
                Box::new(Predicate::Or(
                    Box::new(Predicate::eq("a", Value::Int(1))),
                    Box::new(Predicate::eq("b", Value::Int(2))),
                )),
                Box::new(Predicate::Compare {
                    column: "name".into(),
                    op: Like,
                    value: Value::Text("a%".into())
                }),
            )
        );
        // `!=` and `<>` are the same operator.
        assert_eq!(
            parse("SELECT * FROM t WHERE x != 1").unwrap(),
            parse("SELECT * FROM t WHERE x <> 1").unwrap()
        );
    }

    #[test]
    fn parses_aggregates() {
        assert_eq!(
            parse("SELECT SUM(amount), MAX(id), COUNT(id) FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Aggregates(vec![
                    Aggregate {
                        func: AggFunc::Sum,
                        column: Some("amount".into())
                    },
                    Aggregate {
                        func: AggFunc::Max,
                        column: Some("id".into())
                    },
                    Aggregate {
                        func: AggFunc::Count,
                        column: Some("id".into())
                    },
                ]),
                before: None,
                filter: None,
                order: None,
                limit: None,
            }
        );
        // SUM(*) is rejected; only COUNT may use `*`.
        assert!(parse("SELECT SUM(*) FROM t").is_err());
        // A column literally named `sum` (no parens) is still a column.
        assert_eq!(
            parse("SELECT sum FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Columns(vec!["sum".into()]),
                before: None,
                filter: None,
                order: None,
                limit: None,
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
