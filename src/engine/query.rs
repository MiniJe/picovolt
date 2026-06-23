//! A deliberately small SQL front-end.
//!
//! PicoVolt's focus is the storage/MVCC engine, not query planning, so this is a
//! compact hand-written tokenizer + recursive-descent parser. It covers
//! `CREATE TABLE`/`INDEX`, `INSERT`, `DROP TABLE`, `UPDATE`/`DELETE`, and
//! `SELECT` with column/aggregate projection, `WHERE` predicates (comparison
//! operators, `AND`/`OR`, `LIKE`), `GROUP BY`, `BEFORE tx` time-travel,
//! `ORDER BY`, and `LIMIT`. Anything beyond that (joins, subqueries) is
//! intentionally out of scope and reported as [`PvError::Query`].

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
    /// `SELECT <proj> FROM name [WHERE <pred>] [GROUP BY cols] [BEFORE tx]
    /// [ORDER BY col [ASC|DESC]] [LIMIT n]`
    Select {
        /// Source table.
        table: String,
        /// What to return: `*`, a column list, or select items (columns and
        /// aggregates).
        projection: Projection,
        /// Optional time-travel snapshot id.
        before: Option<u64>,
        /// Optional `WHERE` predicate.
        filter: Option<Predicate>,
        /// Columns to group by; empty for a non-grouped query.
        group_by: Vec<String>,
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
    /// `*`: every column.
    All,
    /// A specific list of columns.
    Columns(Vec<String>),
    /// A list of select items: columns and/or aggregate terms. With no `GROUP BY`
    /// and only aggregates, this produces a single row. With `GROUP BY`, it
    /// produces one row per group, and any bare column must be a grouping column.
    Items(Vec<SelectItem>),
}

/// One entry in a `SELECT` list when columns and aggregates are mixed.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    /// A bare column reference (must be a grouping column under `GROUP BY`).
    Column(String),
    /// An aggregate term such as `SUM(amount)`.
    Aggregate(Aggregate),
}

/// An aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    /// Row or non-null count.
    Count,
    /// Sum of integer values.
    Sum,
    /// Minimum value (any comparable type).
    Min,
    /// Maximum value (any comparable type).
    Max,
    /// Average of integer values, returned as an exact fixed-point
    /// [`Value::Decimal`](crate::core::value::Value::Decimal). It is numeric and
    /// orderable, but not yet storable on disk or constructible from a literal.
    Avg,
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
    Dec(i128),
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

/// Build a fixed-point decimal mantissa (scaled by `10^DECIMAL_SCALE`) from the
/// integer and fractional digit strings of a literal such as `12.50`. Extra
/// fractional digits past the scale are truncated; fewer are zero-padded.
fn decimal_mantissa(int_part: &str, frac: &str, negative: bool) -> Option<i128> {
    use crate::core::value::{DECIMAL_DEN, DECIMAL_SCALE};
    let int_val: i128 = int_part.parse().ok()?;
    let scale = DECIMAL_SCALE as usize;
    let mut f = frac.to_string();
    if f.len() > scale {
        f.truncate(scale);
    }
    while f.len() < scale {
        f.push('0');
    }
    let frac_val: i128 = f.parse().ok()?;
    let mag = int_val.checked_mul(DECIMAL_DEN)?.checked_add(frac_val)?;
    Some(if negative { -mag } else { mag })
}

/// Substitute each `?` placeholder in `sql` with the matching parameter, rendered
/// as a safely-escaped SQL literal. Placeholders inside string literals are left
/// untouched, and the parameter count must match exactly. This is what lets the
/// bindings offer parameterized queries without callers building SQL by hand.
pub fn bind_params(sql: &str, params: &[Value]) -> crate::Result<String> {
    let mut out = String::with_capacity(sql.len() + params.len() * 4);
    let mut in_str = false;
    let mut next = 0usize;
    let mut chars = sql.chars().peekable();
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
        } else if c == '?' {
            let v = params.get(next).ok_or_else(|| {
                crate::PvError::Schema(format!(
                    "parameter ? number {} has no bound value ({} provided)",
                    next + 1,
                    params.len()
                ))
            })?;
            out.push_str(&value_to_sql_literal(v)?);
            next += 1;
        } else {
            out.push(c);
        }
    }
    if next != params.len() {
        return Err(crate::PvError::Schema(format!(
            "{} parameters provided but the statement has {} placeholder(s)",
            params.len(),
            next
        )));
    }
    Ok(out)
}

fn value_to_sql_literal(v: &Value) -> crate::Result<String> {
    Ok(match v {
        Value::Null => "NULL".to_string(),
        Value::Int(i) => i.to_string(),
        // The fixed-point text (e.g. "1.500000") re-parses as the same decimal.
        Value::Decimal(_) => v.to_string(),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(_) => {
            return Err(crate::PvError::Schema(
                "blob parameters are not supported in SQL parameter binding".into(),
            ))
        }
    })
}

/// Render `msg` annotated with the line and column of character index `char_pos`
/// in `sql`, plus the offending line and a caret. `char_pos` is clamped to the
/// input length, so an end-of-input position points just past the last character.
fn point_at(sql: &str, char_pos: usize, msg: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let pos = char_pos.min(chars.len());
    let line_start = chars[..pos]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let line_end = chars[pos..]
        .iter()
        .position(|&c| c == '\n')
        .map(|i| pos + i)
        .unwrap_or(chars.len());
    let line_no = chars[..line_start].iter().filter(|&&c| c == '\n').count() + 1;
    let col = pos - line_start + 1;
    let line_text: String = chars[line_start..line_end].iter().collect();
    let caret = " ".repeat(pos - line_start);
    format!("{msg} (line {line_no}, column {col})\n  {line_text}\n  {caret}^")
}

/// A character cursor that tracks the index of the next character to read, so the
/// tokenizer can record where each token begins.
struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            chars: s.chars().peekable(),
            pos: 0,
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
}

/// Tokenize `sql` into `(token, start_char_index)` pairs.
fn tokenize(sql: &str) -> Result<Vec<(Tok, usize)>> {
    let mut toks = Vec::new();
    let mut lx = Lexer::new(sql);
    let err = |pos, msg: &str| PvError::Query(point_at(sql, pos, msg));
    while let Some(c) = lx.peek() {
        let start = lx.pos;
        match c {
            ws if ws.is_whitespace() => {
                lx.bump();
            }
            '(' => {
                lx.bump();
                toks.push((Tok::LParen, start));
            }
            ')' => {
                lx.bump();
                toks.push((Tok::RParen, start));
            }
            ',' => {
                lx.bump();
                toks.push((Tok::Comma, start));
            }
            '=' => {
                lx.bump();
                toks.push((Tok::Eq, start));
            }
            '<' => {
                lx.bump();
                match lx.peek() {
                    Some('=') => {
                        lx.bump();
                        toks.push((Tok::Le, start));
                    }
                    Some('>') => {
                        lx.bump();
                        toks.push((Tok::Ne, start));
                    }
                    _ => toks.push((Tok::Lt, start)),
                }
            }
            '>' => {
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    toks.push((Tok::Ge, start));
                } else {
                    toks.push((Tok::Gt, start));
                }
            }
            '!' => {
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    toks.push((Tok::Ne, start));
                } else {
                    return Err(err(start, "expected `=` after `!`"));
                }
            }
            '*' => {
                lx.bump();
                toks.push((Tok::Star, start));
            }
            ';' => {
                lx.bump(); // statement terminator, ignored
            }
            '\'' => {
                lx.bump(); // opening quote
                let mut s = String::new();
                let mut closed = false;
                loop {
                    match lx.bump() {
                        // A doubled quote `''` is an escaped literal `'` (SQL style).
                        Some('\'') if lx.peek() == Some('\'') => {
                            lx.bump();
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
                    return Err(err(start, "unterminated string literal"));
                }
                toks.push((Tok::Str(s), start));
            }
            '-' | '0'..='9' => {
                let negative = c == '-';
                if negative {
                    lx.bump();
                }
                let mut int_part = String::new();
                while let Some(d) = lx.peek() {
                    if d.is_ascii_digit() {
                        int_part.push(d);
                        lx.bump();
                    } else {
                        break;
                    }
                }
                if int_part.is_empty() {
                    return Err(err(start, "expected digits"));
                }
                if lx.peek() == Some('.') {
                    lx.bump();
                    let mut frac = String::new();
                    while let Some(d) = lx.peek() {
                        if d.is_ascii_digit() {
                            frac.push(d);
                            lx.bump();
                        } else {
                            break;
                        }
                    }
                    if frac.is_empty() {
                        return Err(err(start, "expected digits after `.`"));
                    }
                    let m = decimal_mantissa(&int_part, &frac, negative)
                        .ok_or_else(|| err(start, "decimal literal out of range"))?;
                    toks.push((Tok::Dec(m), start));
                } else {
                    let mut s = String::new();
                    if negative {
                        s.push('-');
                    }
                    s.push_str(&int_part);
                    let v: i64 = s
                        .parse()
                        .map_err(|_| err(start, &format!("invalid integer `{s}`")))?;
                    toks.push((Tok::Int(v), start));
                }
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut w = String::new();
                while let Some(d) = lx.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        w.push(d);
                        lx.bump();
                    } else {
                        break;
                    }
                }
                toks.push((Tok::Word(w), start));
            }
            other => return Err(err(start, &format!("unexpected character `{other}`"))),
        }
    }
    Ok(toks)
}

/// Cursor over a token stream with small typed consumers. Carries the source text
/// so parse errors can point at the offending token's line and column.
struct Cursor {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    sql: String,
    end: usize,
}

impl Cursor {
    fn new(toks: Vec<(Tok, usize)>, sql: &str) -> Self {
        Self {
            toks,
            pos: 0,
            sql: sql.to_string(),
            end: sql.chars().count(),
        }
    }

    /// Character index of the current (not-yet-consumed) token, or end-of-input.
    fn here(&self) -> usize {
        self.toks.get(self.pos).map(|(_, p)| *p).unwrap_or(self.end)
    }

    /// A positioned parse error at the current token.
    fn err(&self, msg: impl std::fmt::Display) -> PvError {
        self.err_at(self.here(), msg)
    }

    /// A positioned parse error at a specific character index (used when an error
    /// is about a token that was just consumed).
    fn err_at(&self, at: usize, msg: impl std::fmt::Display) -> PvError {
        PvError::Query(point_at(&self.sql, at, &msg.to_string()))
    }

    fn next(&mut self) -> Result<Tok> {
        match self.toks.get(self.pos) {
            Some((t, _)) => {
                let t = t.clone();
                self.pos += 1;
                Ok(t)
            }
            None => Err(self.err("unexpected end of statement")),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    fn peek2(&self) -> Option<&Tok> {
        self.toks.get(self.pos + 1).map(|(t, _)| t)
    }

    /// Consume a keyword (case-insensitive), erroring if it doesn't match.
    fn keyword(&mut self, kw: &str) -> Result<()> {
        let at = self.here();
        match self.next()? {
            Tok::Word(w) if w.eq_ignore_ascii_case(kw) => Ok(()),
            other => Err(self.err_at(at, format!("expected `{kw}`, found {other:?}"))),
        }
    }

    fn ident(&mut self) -> Result<String> {
        let at = self.here();
        match self.next()? {
            Tok::Word(w) => Ok(w),
            other => Err(self.err_at(at, format!("expected identifier, found {other:?}"))),
        }
    }

    fn expect(&mut self, tok: Tok) -> Result<()> {
        let at = self.here();
        let got = self.next()?;
        if got == tok {
            Ok(())
        } else {
            Err(self.err_at(at, format!("expected {tok:?}, found {got:?}")))
        }
    }

    fn value(&mut self) -> Result<Value> {
        let at = self.here();
        match self.next()? {
            Tok::Int(i) => Ok(Value::Int(i)),
            Tok::Dec(m) => Ok(Value::Decimal(m)),
            Tok::Str(s) => Ok(Value::Text(s)),
            Tok::Word(w) if w.eq_ignore_ascii_case("null") => Ok(Value::Null),
            other => Err(self.err_at(at, format!("expected a value, found {other:?}"))),
        }
    }

    fn finish(&self) -> Result<()> {
        if self.pos == self.toks.len() {
            Ok(())
        } else {
            Err(self.err("trailing tokens after statement"))
        }
    }
}

/// Parse a single SQL statement.
pub fn parse(sql: &str) -> Result<Statement> {
    let mut cur = Cursor::new(tokenize(sql)?, sql);
    let at = cur.here();
    let stmt = match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("create") => parse_create(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("insert") => parse_insert(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("select") => parse_select(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("update") => parse_update(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("delete") => parse_delete(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("drop") => parse_drop(&mut cur)?,
        other => return Err(cur.err_at(at, format!("unsupported statement: {other:?}"))),
    };
    cur.finish()?;
    Ok(stmt)
}

fn parse_create(cur: &mut Cursor) -> Result<Statement> {
    let at = cur.here();
    match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("table") => {
            let name = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let mut columns = Vec::new();
            loop {
                columns.push(cur.ident()?);
                let sep = cur.here();
                match cur.next()? {
                    Tok::Comma => continue,
                    Tok::RParen => break,
                    other => {
                        return Err(cur.err_at(sep, format!("expected `,` or `)`, found {other:?}")))
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
        other => Err(cur.err_at(
            at,
            format!("expected TABLE or INDEX after CREATE, found {other:?}"),
        )),
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
        let sep = cur.here();
        match cur.next()? {
            Tok::Comma => continue,
            Tok::RParen => break,
            other => return Err(cur.err_at(sep, format!("expected `,` or `)`, found {other:?}"))),
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
        "AVG" => Some(AggFunc::Avg),
        _ => None,
    }
}

fn parse_projection(cur: &mut Cursor) -> Result<Projection> {
    if matches!(cur.peek(), Some(Tok::Star)) {
        cur.next()?;
        return Ok(Projection::All);
    }
    let mut items = vec![parse_select_item(cur)?];
    while matches!(cur.peek(), Some(Tok::Comma)) {
        cur.next()?;
        items.push(parse_select_item(cur)?);
    }
    // Keep the simpler Columns form when every item is a bare column.
    if items.iter().all(|i| matches!(i, SelectItem::Column(_))) {
        let cols = items
            .into_iter()
            .map(|i| match i {
                SelectItem::Column(c) => c,
                SelectItem::Aggregate(_) => unreachable!("all items checked to be columns"),
            })
            .collect();
        Ok(Projection::Columns(cols))
    } else {
        Ok(Projection::Items(items))
    }
}

fn parse_select_item(cur: &mut Cursor) -> Result<SelectItem> {
    // An aggregate is a known function name immediately followed by `(`.
    let is_agg = matches!(cur.peek(), Some(Tok::Word(w)) if agg_func(w).is_some())
        && matches!(cur.peek2(), Some(Tok::LParen));
    if is_agg {
        Ok(SelectItem::Aggregate(parse_aggregate(cur)?))
    } else {
        Ok(SelectItem::Column(cur.ident()?))
    }
}

fn parse_aggregate(cur: &mut Cursor) -> Result<Aggregate> {
    let at = cur.here();
    let word = cur.ident()?;
    let func =
        agg_func(&word).ok_or_else(|| cur.err_at(at, format!("unknown aggregate `{word}`")))?;
    cur.expect(Tok::LParen)?;
    let column = if matches!(cur.peek(), Some(Tok::Star)) {
        cur.next()?;
        None
    } else {
        Some(cur.ident()?)
    };
    cur.expect(Tok::RParen)?;
    if column.is_none() && func != AggFunc::Count {
        return Err(cur.err_at(at, "only COUNT(*) may use `*`; SUM/MIN/MAX need a column"));
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
    let op_at = cur.here();
    let op = match cur.next()? {
        Tok::Eq => CompareOp::Eq,
        Tok::Ne => CompareOp::Ne,
        Tok::Lt => CompareOp::Lt,
        Tok::Le => CompareOp::Le,
        Tok::Gt => CompareOp::Gt,
        Tok::Ge => CompareOp::Ge,
        Tok::Word(w) if w.eq_ignore_ascii_case("like") => CompareOp::Like,
        other => {
            return Err(cur.err_at(
                op_at,
                format!("expected a comparison operator, found {other:?}"),
            ))
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

    let group_by = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("group")) {
        cur.next()?; // consume GROUP
        cur.keyword("by")?;
        let mut cols = vec![cur.ident()?];
        while matches!(cur.peek(), Some(Tok::Comma)) {
            cur.next()?;
            cols.push(cur.ident()?);
        }
        cols
    } else {
        Vec::new()
    };

    let before = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("before")) {
        cur.next()?; // consume BEFORE
        let at = cur.here();
        match cur.next()? {
            Tok::Int(i) if i >= 0 => Some(i as u64),
            other => {
                return Err(cur.err_at(
                    at,
                    format!("BEFORE expects a non-negative integer, found {other:?}"),
                ))
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
        let at = cur.here();
        match cur.next()? {
            Tok::Int(i) if i >= 0 => Some(i as usize),
            other => {
                return Err(cur.err_at(
                    at,
                    format!("LIMIT expects a non-negative integer, found {other:?}"),
                ))
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
        group_by,
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
                group_by: vec![],
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
                group_by: vec![],
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
                group_by: vec![],
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
                group_by: vec![],
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
                group_by: vec![],
                order: None,
                limit: None,
            }
        );
        // COUNT(*).
        assert_eq!(
            parse("SELECT COUNT(*) FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Items(vec![SelectItem::Aggregate(Aggregate {
                    func: AggFunc::Count,
                    column: None,
                })]),
                before: None,
                filter: None,
                group_by: vec![],
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
                group_by: vec![],
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
                projection: Projection::Items(vec![
                    SelectItem::Aggregate(Aggregate {
                        func: AggFunc::Sum,
                        column: Some("amount".into())
                    }),
                    SelectItem::Aggregate(Aggregate {
                        func: AggFunc::Max,
                        column: Some("id".into())
                    }),
                    SelectItem::Aggregate(Aggregate {
                        func: AggFunc::Count,
                        column: Some("id".into())
                    }),
                ]),
                before: None,
                filter: None,
                group_by: vec![],
                order: None,
                limit: None,
            }
        );
        // SUM(*) is rejected; only COUNT may use `*`.
        assert!(parse("SELECT SUM(*) FROM t").is_err());
        // AVG parses to its own aggregate and requires a column.
        assert_eq!(
            parse("SELECT AVG(amount) FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Items(vec![SelectItem::Aggregate(Aggregate {
                    func: AggFunc::Avg,
                    column: Some("amount".into()),
                })]),
                before: None,
                filter: None,
                group_by: vec![],
                order: None,
                limit: None,
            }
        );
        assert!(parse("SELECT AVG(*) FROM t").is_err());
    }

    #[test]
    fn parses_group_by() {
        assert_eq!(
            parse("SELECT tier, COUNT(*) FROM users GROUP BY tier").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Items(vec![
                    SelectItem::Column("tier".into()),
                    SelectItem::Aggregate(Aggregate {
                        func: AggFunc::Count,
                        column: None,
                    }),
                ]),
                before: None,
                filter: None,
                group_by: vec!["tier".into()],
                order: None,
                limit: None,
            }
        );
        // A column literally named `sum` (no parens) is still a column.
        assert_eq!(
            parse("SELECT sum FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Columns(vec!["sum".into()]),
                before: None,
                filter: None,
                group_by: vec![],
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

    #[test]
    fn parse_errors_are_positioned() {
        // A parse error names the offending token's line and column and draws a
        // caret under the source.
        let e = parse("SELECT * users").unwrap_err().to_string();
        assert!(e.contains("expected `from`"), "{e}");
        assert!(e.contains("line 1, column 10"), "{e}"); // `users` begins at column 10
        assert!(e.contains("SELECT * users"), "{e}"); // the offending line is echoed
        assert!(e.contains('^'), "{e}");

        // Tokenizer errors are positioned too.
        let e = parse("SELECT $ FROM t").unwrap_err().to_string();
        assert!(e.contains("unexpected character"), "{e}");
        assert!(e.contains("line 1, column 8"), "{e}");

        let e = parse("SELECT * FROM t WHERE name = 'abc")
            .unwrap_err()
            .to_string();
        assert!(e.contains("unterminated string literal"), "{e}");
        assert!(e.contains('^'), "{e}");

        // End-of-input errors point just past the end.
        let e = parse("SELECT * FROM").unwrap_err().to_string();
        assert!(e.contains("unexpected end of statement"), "{e}");
        assert!(e.contains("line 1, column 14"), "{e}");
        assert!(e.contains('^'), "{e}");
    }
}
