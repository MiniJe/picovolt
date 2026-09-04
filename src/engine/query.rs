//! A deliberately small SQL front-end.
//!
//! PicoVolt's focus is the storage/MVCC engine, not query planning, so this is a
//! compact hand-written tokenizer + recursive-descent parser. It covers
//! schema-light `CREATE TABLE`/`INDEX`, default-aware `INSERT`, `DROP TABLE`,
//! `UPDATE`/`DELETE`, transactions, and `SELECT` with scalar/aggregate
//! projection, aliases, equality joins, predicates, grouping, `BEFORE tx`
//! time-travel, ordering, and pagination. Unsupported constructs such as
//! subqueries and set operations are rejected with positioned
//! [`PvError::Query`] diagnostics.

use crate::core::errors::{PvError, Result};
use crate::core::value::Value;
use serde::{Deserialize, Serialize};

/// A parsed statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// `EXPLAIN SELECT ...`: describe the read plan without executing it.
    Explain {
        /// The read statement whose physical path should be described.
        statement: Box<Statement>,
    },
    /// `BEGIN [TRANSACTION]`
    Begin,
    /// `COMMIT [TRANSACTION]`
    Commit,
    /// `ROLLBACK [TRANSACTION]`
    Rollback,
    /// `CREATE TABLE name (col, col, ...)`
    CreateTable {
        /// Table name.
        name: String,
        /// Declared column names.
        columns: Vec<String>,
        /// Columns declared `PRIMARY KEY` or `UNIQUE`.
        unique_columns: Vec<String>,
        /// Columns declared `PRIMARY KEY` or `NOT NULL`.
        not_null_columns: Vec<String>,
    },
    /// `CREATE TABLE IF NOT EXISTS ...`
    CreateTableIfNotExists {
        /// Table name.
        name: String,
        /// Declared column names.
        columns: Vec<String>,
        /// Columns declared `PRIMARY KEY` or `UNIQUE`.
        unique_columns: Vec<String>,
        /// Columns declared `PRIMARY KEY` or `NOT NULL`.
        not_null_columns: Vec<String>,
    },
    /// A schema-rich `CREATE TABLE` carrying defaults and/or check constraints.
    CreateTableSchema {
        /// Table name.
        name: String,
        /// Column declarations in storage order.
        columns: Vec<ColumnDefinition>,
        /// Table-wide and inline check predicates.
        checks: Vec<Predicate>,
        /// Suppress the existing-table error when true.
        if_not_exists: bool,
    },
    /// `INSERT INTO name VALUES (v, v, ...)`
    Insert {
        /// Target table.
        table: String,
        /// Row values, positional.
        values: Vec<Value>,
    },
    /// `INSERT INTO name VALUES (...), (...)`
    InsertMany {
        /// Target table.
        table: String,
        /// Rows to insert, in source order.
        rows: Vec<Vec<Value>>,
    },
    /// An insert that names target columns, uses `DEFAULT`, or uses
    /// `DEFAULT VALUES`.
    InsertSchema {
        /// Target table.
        table: String,
        /// Named targets, or `None` for complete positional rows. An empty list
        /// represents `DEFAULT VALUES`.
        target_columns: Option<Vec<String>>,
        /// Rows in source order.
        rows: Vec<Vec<InsertValue>>,
    },
    /// `CREATE INDEX ON name (col)`
    CreateIndex {
        /// Table to index.
        table: String,
        /// Column to index.
        column: String,
        /// Enforce uniqueness when true.
        unique: bool,
    },
    /// `SELECT <proj> FROM name [WHERE <pred>] [GROUP BY cols] [BEFORE tx]
    /// [ORDER BY col [ASC|DESC]] [LIMIT n]`
    Select {
        /// Source table.
        table: String,
        /// What to return: `*`, a column list, or select items (columns and
        /// aggregates).
        projection: Projection,
        /// `SELECT DISTINCT`: drop duplicate output rows.
        distinct: bool,
        /// Optional time-travel snapshot id.
        before: Option<u64>,
        /// Optional `WHERE` predicate.
        filter: Option<Predicate>,
        /// Columns to group by; empty for a non-grouped query.
        group_by: Vec<String>,
        /// Optional `HAVING` predicate, filtering grouped output rows.
        having: Option<HavingPred>,
        /// Sort keys, applied left to right; empty for no ordering.
        order: Vec<OrderBy>,
        /// Optional cap on the number of rows returned.
        limit: Option<usize>,
        /// Number of result rows to skip after ordering.
        offset: usize,
    },
    /// A `SELECT` over an aliased table or one or more equality joins.
    SelectJoin {
        /// What to return from the combined row.
        projection: Projection,
        /// Drop duplicate output rows.
        distinct: bool,
        /// First relation in the `FROM` clause.
        source: TableRef,
        /// Equality joins, evaluated from left to right.
        joins: Vec<JoinClause>,
        /// Optional time-travel snapshot id shared by every input table.
        before: Option<u64>,
        /// Optional predicate over the combined row.
        filter: Option<Predicate>,
        /// Columns to group by; empty for a non-grouped query.
        group_by: Vec<String>,
        /// Optional predicate over grouped joined rows.
        having: Option<HavingPred>,
        /// Sort keys over the combined row.
        order: Vec<OrderBy>,
        /// Optional result cap.
        limit: Option<usize>,
        /// Number of joined result rows to skip.
        offset: usize,
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
    /// `UPDATE name SET col = DEFAULT WHERE <pred>`.
    UpdateDefault {
        /// Target table.
        table: String,
        /// Column whose declared default should be assigned.
        column: String,
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
    /// `DROP TABLE IF EXISTS name`
    DropTableIfExists {
        /// Table to drop when present.
        table: String,
    },
}

/// A table named in `FROM` or `JOIN`, with its optional query-local alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    /// Catalog table name.
    pub name: String,
    /// Query-local qualifier. When absent, `name` is the qualifier.
    pub alias: Option<String>,
}

impl TableRef {
    /// Qualifier exposed to column references and result metadata.
    pub fn qualifier(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// One `INNER` or `LEFT` equality join in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinClause {
    /// Relation introduced by this join.
    pub table: TableRef,
    /// First column reference in the equality expression.
    pub first_column: String,
    /// Second column reference in the equality expression.
    pub second_column: String,
    /// Preserve unmatched accumulated rows when true.
    pub left_join: bool,
}

/// One schema-rich column declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDefinition {
    /// Column name.
    pub name: String,
    /// Deterministic literal default, if declared.
    pub default: Option<Value>,
    /// `PRIMARY KEY` or `UNIQUE`.
    pub unique: bool,
    /// `PRIMARY KEY` or `NOT NULL`.
    pub not_null: bool,
}

/// One value position in a schema-aware insert.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertValue {
    /// An explicit SQL literal.
    Literal(Value),
    /// Use the target column's declared default (or NULL when none exists).
    Default,
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

/// One entry in a `SELECT` list: a column or aggregate term, with an optional
/// `AS` alias that names the output column.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectItem {
    /// The column or aggregate term.
    pub expr: SelectExpr,
    /// Output name from `AS`, if any.
    pub alias: Option<String>,
}

/// The term inside a [`SelectItem`].
#[derive(Debug, Clone, PartialEq)]
pub enum SelectExpr {
    /// A bare column reference (must be a grouping column under `GROUP BY`).
    Column(String),
    /// An aggregate term such as `SUM(amount)`.
    Aggregate(Aggregate),
    /// A row-level expression such as `LOWER(name)` or `CASE WHEN ... END`.
    Scalar(ScalarExpr),
}

/// A non-aggregate expression evaluated against one input row.
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarExpr {
    /// A column reference, optionally qualified by a table or alias.
    Column(String),
    /// A SQL literal.
    Literal(Value),
    /// A focused built-in function call.
    Function {
        /// Function implementation.
        function: ScalarFunc,
        /// Function arguments.
        arguments: Vec<ScalarExpr>,
    },
    /// Searched CASE: the first true predicate wins, otherwise `else_expr` or NULL.
    Case {
        /// Ordered `WHEN predicate THEN expression` branches.
        branches: Vec<(Predicate, ScalarExpr)>,
        /// Optional `ELSE` expression.
        else_expr: Option<Box<ScalarExpr>>,
    },
}

/// Scalar functions intentionally supported by PicoVolt's focused SQL surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarFunc {
    /// Unicode lowercase text conversion.
    Lower,
    /// Unicode uppercase text conversion.
    Upper,
    /// Trim leading and trailing Unicode whitespace.
    Trim,
    /// Count Unicode scalar values in text or bytes in a blob.
    Length,
    /// Numeric absolute value with overflow checking.
    Abs,
    /// Return the first non-NULL argument.
    Coalesce,
    /// Return NULL when two values compare equal, otherwise the first value.
    NullIf,
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
    /// Average of numeric values, returned as an exact fixed-point
    /// [`Value::Decimal`].
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `NOT LIKE`
    NotLike,
}

/// A `WHERE` predicate: comparisons combined with `AND` / `OR`. `AND` binds
/// tighter than `OR`; parentheses override precedence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// `column [NOT] IN (v1, v2, ...)`. A null column value matches neither form; a
    /// null inside the list makes a non-match UNKNOWN (so `NOT IN` over a list that
    /// contains a null returns no rows), per SQL three-valued logic.
    In {
        /// Column on the left.
        column: String,
        /// The set of candidate literals.
        values: Vec<Value>,
        /// `NOT IN` when true.
        negated: bool,
    },
    /// `column [NOT] BETWEEN low AND high` — inclusive bounds. A null column value
    /// matches neither form.
    Between {
        /// Column on the left.
        column: String,
        /// Inclusive lower bound.
        low: Value,
        /// Inclusive upper bound.
        high: Value,
        /// `NOT BETWEEN` when true.
        negated: bool,
    },
    /// `column IS [NOT] NULL`.
    IsNull {
        /// Column tested for null.
        column: String,
        /// `IS NOT NULL` when true.
        negated: bool,
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

/// A `HAVING` predicate: like a `WHERE` predicate, but each comparison tests a
/// grouped output column or an aggregate computed over the group, so it can filter
/// on an aggregate that does not appear in the `SELECT` list.
#[derive(Debug, Clone, PartialEq)]
pub enum HavingPred {
    /// `term <op> value`
    Compare {
        /// A group column / alias, or an aggregate.
        term: HavingTerm,
        /// The comparison operator (`LIKE`/`NOT LIKE` are not allowed in `HAVING`).
        op: CompareOp,
        /// Literal on the right.
        value: Value,
    },
    /// `a AND b`
    And(Box<HavingPred>, Box<HavingPred>),
    /// `a OR b`
    Or(Box<HavingPred>, Box<HavingPred>),
}

/// The left-hand side of a `HAVING` comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum HavingTerm {
    /// A grouped output column: a group column or a select-list alias.
    Column(String),
    /// An aggregate computed over each group, e.g. `COUNT(*)` or `SUM(amount)`.
    Aggregate(Aggregate),
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
    /// A delimited identifier (`"name"`, `` `name` ``, or `[name]`). Keeping
    /// this distinct from [`Tok::Word`] prevents quoted keywords from being
    /// interpreted as SQL clauses.
    Ident(String),
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
    Dot,
    Semicolon,
}

/// Hard limits for parser-owned recursive structures. SQL accepted at a trust
/// boundary must fail predictably instead of exhausting the process stack.
const MAX_SQL_PAREN_DEPTH: usize = 64;
const MAX_SCALAR_DEPTH: usize = 64;
const MAX_PREDICATE_NODES: usize = 256;

/// Build a fixed-point decimal mantissa (scaled by `10^DECIMAL_SCALE`) from the
/// integer and fractional digit strings of a literal such as `12.50`. Extra
/// fractional digits past the scale are truncated; fewer are zero-padded.
fn decimal_mantissa(int_part: &str, frac: &str, negative: bool) -> Option<i128> {
    use crate::core::value::{DECIMAL_DEN, DECIMAL_SCALE};
    let int_val: u128 = int_part.parse().ok()?;
    let scale = DECIMAL_SCALE as usize;
    let mut f = frac.to_string();
    if f.len() > scale {
        f.truncate(scale);
    }
    while f.len() < scale {
        f.push('0');
    }
    let frac_val: u128 = f.parse().ok()?;
    let mag = int_val
        .checked_mul(DECIMAL_DEN as u128)?
        .checked_add(frac_val)?;
    if negative {
        let min_magnitude = (i128::MAX as u128) + 1;
        if mag == min_magnitude {
            Some(i128::MIN)
        } else {
            i128::try_from(mag).ok()?.checked_neg()
        }
    } else {
        i128::try_from(mag).ok()
    }
}

fn sql_quote_closing(opening: char) -> Option<char> {
    match opening {
        '\'' => Some('\''),
        '"' => Some('"'),
        '`' => Some('`'),
        '[' => Some(']'),
        _ => None,
    }
}

/// Substitute each `?` placeholder in `sql` with the matching parameter, rendered
/// as a safely-escaped SQL literal. Question marks inside string literals or
/// quoted identifiers are left untouched, and the parameter count must match
/// exactly. This is what lets the bindings offer parameterized queries without
/// callers building SQL by hand.
pub fn bind_params(sql: &str, params: &[Value]) -> crate::Result<String> {
    let mut out = String::with_capacity(sql.len() + params.len() * 4);
    let mut quoted = None;
    let mut next = 0usize;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(closing) = quoted {
            out.push(c);
            if c == closing {
                if chars.peek() == Some(&closing) {
                    out.push(closing);
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else if let Some(closing) = sql_quote_closing(c) {
            quoted = Some(closing);
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

/// Count positional `?` placeholders, ignoring question marks inside SQL string
/// literals and quoted identifiers. Used by [`crate::PreparedStatement`] to
/// validate arguments before execution.
pub fn parameter_count(sql: &str) -> usize {
    let mut count = 0;
    let mut quoted = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(closing) = quoted {
            if c == closing {
                if chars.peek() == Some(&closing) {
                    chars.next();
                } else {
                    quoted = None;
                }
            }
        } else if let Some(closing) = sql_quote_closing(c) {
            quoted = Some(closing);
        } else if c == '?' {
            count += 1;
        }
    }
    count
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
    let mut paren_depth = 0usize;
    let err = |pos, msg: &str| PvError::Query(point_at(sql, pos, msg));
    while let Some(c) = lx.peek() {
        let start = lx.pos;
        match c {
            ws if ws.is_whitespace() => {
                lx.bump();
            }
            '(' => {
                lx.bump();
                paren_depth = paren_depth.saturating_add(1);
                if paren_depth > MAX_SQL_PAREN_DEPTH {
                    return Err(err(
                        start,
                        &format!(
                            "SQL expression nesting exceeds the {MAX_SQL_PAREN_DEPTH}-level limit"
                        ),
                    ));
                }
                toks.push((Tok::LParen, start));
            }
            ')' => {
                lx.bump();
                paren_depth = paren_depth.saturating_sub(1);
                toks.push((Tok::RParen, start));
            }
            ',' => {
                lx.bump();
                toks.push((Tok::Comma, start));
            }
            '.' => {
                lx.bump();
                toks.push((Tok::Dot, start));
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
                lx.bump();
                toks.push((Tok::Semicolon, start));
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
            '"' | '`' | '[' => {
                let opening = lx.bump().expect("peeked delimiter");
                let closing = if opening == '[' { ']' } else { opening };
                let mut identifier = String::new();
                let mut closed = false;
                loop {
                    match lx.bump() {
                        Some(ch) if ch == closing && lx.peek() == Some(closing) => {
                            lx.bump();
                            identifier.push(closing);
                        }
                        Some(ch) if ch == closing => {
                            closed = true;
                            break;
                        }
                        Some(ch) if ch.is_control() => {
                            return Err(err(
                                start,
                                "quoted identifiers cannot contain control characters",
                            ));
                        }
                        Some(ch) => identifier.push(ch),
                        None => break,
                    }
                }
                if !closed {
                    return Err(err(start, "unterminated quoted identifier"));
                }
                if identifier.is_empty() {
                    return Err(err(start, "quoted identifiers cannot be empty"));
                }
                toks.push((Tok::Ident(identifier), start));
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
    predicate_nodes: usize,
}

impl Cursor {
    fn new(toks: Vec<(Tok, usize)>, sql: &str) -> Self {
        Self {
            toks,
            pos: 0,
            sql: sql.to_string(),
            end: sql.chars().count(),
            predicate_nodes: 0,
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
            Tok::Word(w) | Tok::Ident(w) => Ok(w),
            other => Err(self.err_at(at, format!("expected identifier, found {other:?}"))),
        }
    }

    fn ident_is_quoted(&self) -> bool {
        matches!(self.peek(), Some(Tok::Ident(_)))
    }

    fn column_ref(&mut self) -> Result<String> {
        let mut name = self.ident()?;
        if matches!(self.peek(), Some(Tok::Dot)) {
            self.next()?;
            name.push('.');
            name.push_str(&self.ident()?);
        }
        Ok(name)
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

    fn predicate_node(&mut self, at: usize) -> Result<()> {
        self.predicate_nodes = self.predicate_nodes.saturating_add(1);
        if self.predicate_nodes > MAX_PREDICATE_NODES {
            return Err(self.err_at(
                at,
                format!("predicate complexity exceeds the {MAX_PREDICATE_NODES}-node limit"),
            ));
        }
        Ok(())
    }

    fn finish(&self) -> Result<()> {
        if self.pos == self.toks.len() {
            Ok(())
        } else {
            match self.peek() {
                Some(Tok::Word(word)) => Err(self.err(format!(
                    "unsupported or misplaced SQL construct `{}`",
                    word.to_ascii_uppercase()
                ))),
                Some(token) => {
                    Err(self.err(format!("unexpected token {token:?} after the statement")))
                }
                None => unreachable!("position checked above"),
            }
        }
    }
}

/// Parse a single SQL statement.
pub fn parse(sql: &str) -> Result<Statement> {
    let mut cur = Cursor::new(tokenize(sql)?, sql);
    let at = cur.here();
    let stmt = match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("explain") => {
            let explained_at = cur.here();
            let explained = match cur.next()? {
                Tok::Word(w) if w.eq_ignore_ascii_case("select") => parse_select(&mut cur)?,
                other => {
                    return Err(cur.err_at(
                        explained_at,
                        format!("EXPLAIN requires SELECT, found {other:?}"),
                    ))
                }
            };
            Statement::Explain {
                statement: Box::new(explained),
            }
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("begin") => {
            consume_optional_transaction(&mut cur);
            Statement::Begin
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("commit") => {
            consume_optional_transaction(&mut cur);
            Statement::Commit
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("rollback") => {
            consume_optional_transaction(&mut cur);
            Statement::Rollback
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("create") => parse_create(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("insert") => parse_insert(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("select") => parse_select(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("update") => parse_update(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("delete") => parse_delete(&mut cur)?,
        Tok::Word(w) if w.eq_ignore_ascii_case("drop") => parse_drop(&mut cur)?,
        Tok::Word(word) => {
            return Err(cur.err_at(
                at,
                format!("unsupported statement `{}`", word.to_ascii_uppercase()),
            ))
        }
        other => return Err(cur.err_at(at, format!("unsupported statement token {other:?}"))),
    };
    while matches!(cur.peek(), Some(Tok::Semicolon)) {
        cur.next()?;
    }
    cur.finish()?;
    Ok(stmt)
}

fn consume_optional_transaction(cur: &mut Cursor) {
    if peek_kw(cur, "transaction") {
        let _ = cur.next();
    }
}

fn parse_create(cur: &mut Cursor) -> Result<Statement> {
    let at = cur.here();
    match cur.next()? {
        Tok::Word(w) if w.eq_ignore_ascii_case("table") => {
            let if_not_exists = if peek_kw(cur, "if") {
                cur.next()?;
                cur.keyword("not")?;
                cur.keyword("exists")?;
                true
            } else {
                false
            };
            let name = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let mut definitions = Vec::new();
            let mut checks = Vec::new();
            let mut rich_schema = false;
            loop {
                if ["foreign", "constraint", "unique", "primary"]
                    .iter()
                    .any(|keyword| peek_kw(cur, keyword))
                {
                    let unsupported_at = cur.here();
                    let unsupported = cur.ident()?;
                    return Err(cur.err_at(
                        unsupported_at,
                        format!(
                            "unsupported table constraint `{}`",
                            unsupported.to_ascii_uppercase()
                        ),
                    ));
                }
                if peek_kw(cur, "check") {
                    rich_schema = true;
                    checks.push(parse_check_constraint(cur)?);
                } else {
                    let column_at = cur.here();
                    let column = cur.ident()?;
                    if definitions
                        .iter()
                        .any(|definition: &ColumnDefinition| definition.name == column)
                    {
                        return Err(cur.err_at(
                            column_at,
                            format!("duplicate column declaration `{column}`"),
                        ));
                    }
                    if consume_declared_type(cur)? {
                        rich_schema = true;
                    }
                    let mut definition = ColumnDefinition {
                        name: column,
                        default: None,
                        unique: false,
                        not_null: false,
                    };
                    let mut saw_primary_key = false;
                    let mut saw_default = false;
                    loop {
                        if peek_kw(cur, "primary") {
                            let constraint_at = cur.here();
                            cur.next()?;
                            cur.keyword("key")?;
                            if saw_primary_key {
                                return Err(
                                    cur.err_at(constraint_at, "duplicate PRIMARY KEY constraint")
                                );
                            }
                            saw_primary_key = true;
                            definition.unique = true;
                            definition.not_null = true;
                        } else if peek_kw(cur, "unique") {
                            cur.next()?;
                            definition.unique = true;
                        } else if peek_kw(cur, "not") {
                            cur.next()?;
                            cur.keyword("null")?;
                            definition.not_null = true;
                        } else if peek_kw(cur, "default") {
                            let constraint_at = cur.here();
                            cur.next()?;
                            if saw_default {
                                return Err(
                                    cur.err_at(constraint_at, "duplicate DEFAULT constraint")
                                );
                            }
                            definition.default = Some(parse_default_literal(cur)?);
                            saw_default = true;
                            rich_schema = true;
                        } else if peek_kw(cur, "check") {
                            checks.push(parse_check_constraint(cur)?);
                            rich_schema = true;
                        } else {
                            break;
                        }
                    }
                    definitions.push(definition);
                }
                if let Some(Tok::Word(word)) = cur.peek() {
                    let unsupported_at = cur.here();
                    let unsupported = word.to_ascii_uppercase();
                    return Err(cur.err_at(
                        unsupported_at,
                        format!("unsupported column constraint or type modifier `{unsupported}`"),
                    ));
                }
                let sep = cur.here();
                match cur.next()? {
                    Tok::Comma => continue,
                    Tok::RParen => break,
                    other => {
                        return Err(cur.err_at(sep, format!("expected `,` or `)`, found {other:?}")))
                    }
                }
            }
            if rich_schema {
                Ok(Statement::CreateTableSchema {
                    name,
                    columns: definitions,
                    checks,
                    if_not_exists,
                })
            } else if if_not_exists {
                Ok(Statement::CreateTableIfNotExists {
                    name,
                    columns: definitions
                        .iter()
                        .map(|definition| definition.name.clone())
                        .collect(),
                    unique_columns: definitions
                        .iter()
                        .filter(|definition| definition.unique)
                        .map(|definition| definition.name.clone())
                        .collect(),
                    not_null_columns: definitions
                        .iter()
                        .filter(|definition| definition.not_null)
                        .map(|definition| definition.name.clone())
                        .collect(),
                })
            } else {
                Ok(Statement::CreateTable {
                    name,
                    columns: definitions
                        .iter()
                        .map(|definition| definition.name.clone())
                        .collect(),
                    unique_columns: definitions
                        .iter()
                        .filter(|definition| definition.unique)
                        .map(|definition| definition.name.clone())
                        .collect(),
                    not_null_columns: definitions
                        .iter()
                        .filter(|definition| definition.not_null)
                        .map(|definition| definition.name.clone())
                        .collect(),
                })
            }
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("index") => {
            cur.keyword("on")?;
            let table = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let column = cur.ident()?;
            cur.expect(Tok::RParen)?;
            Ok(Statement::CreateIndex {
                table,
                column,
                unique: false,
            })
        }
        Tok::Word(w) if w.eq_ignore_ascii_case("unique") => {
            cur.keyword("index")?;
            cur.keyword("on")?;
            let table = cur.ident()?;
            cur.expect(Tok::LParen)?;
            let column = cur.ident()?;
            cur.expect(Tok::RParen)?;
            Ok(Statement::CreateIndex {
                table,
                column,
                unique: true,
            })
        }
        other => Err(cur.err_at(
            at,
            format!("expected TABLE or INDEX after CREATE, found {other:?}"),
        )),
    }
}

fn is_column_constraint(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "primary" | "unique" | "not" | "default" | "check"
    )
}

/// Consume one schema-light type declaration. PicoVolt stores dynamic values,
/// so type names and optional `(precision[, scale])` are accepted for adapter
/// compatibility but do not coerce or reject cells.
fn consume_declared_type(cur: &mut Cursor) -> Result<bool> {
    let Some(Tok::Word(word)) = cur.peek() else {
        return Ok(false);
    };
    if is_column_constraint(word) {
        return Ok(false);
    }
    let first = cur.ident()?.to_ascii_lowercase();
    if (first == "double" && peek_kw(cur, "precision"))
        || (first == "character" && peek_kw(cur, "varying"))
    {
        cur.next()?;
    } else if first == "national" && peek_kw(cur, "character") {
        cur.next()?;
        if peek_kw(cur, "varying") {
            cur.next()?;
        }
    }
    if matches!(cur.peek(), Some(Tok::LParen)) {
        cur.next()?;
        let at = cur.here();
        match cur.next()? {
            Tok::Int(value) if value > 0 => {}
            other => {
                return Err(cur.err_at(
                    at,
                    format!("type size expects a positive integer, found {other:?}"),
                ))
            }
        }
        if matches!(cur.peek(), Some(Tok::Comma)) {
            cur.next()?;
            let at = cur.here();
            match cur.next()? {
                Tok::Int(value) if value >= 0 => {}
                other => {
                    return Err(cur.err_at(
                        at,
                        format!("type scale expects a non-negative integer, found {other:?}"),
                    ))
                }
            }
        }
        cur.expect(Tok::RParen)?;
    }
    Ok(true)
}

fn parse_check_constraint(cur: &mut Cursor) -> Result<Predicate> {
    cur.keyword("check")?;
    cur.expect(Tok::LParen)?;
    let predicate = parse_predicate(cur)?;
    cur.expect(Tok::RParen)?;
    Ok(predicate)
}

fn parse_default_literal(cur: &mut Cursor) -> Result<Value> {
    let parenthesized = matches!(cur.peek(), Some(Tok::LParen));
    if parenthesized {
        cur.next()?;
    }
    if !matches!(cur.peek(), Some(Tok::Int(_) | Tok::Dec(_) | Tok::Str(_))) && !peek_kw(cur, "null")
    {
        return Err(cur.err("DEFAULT supports only NULL, integer, decimal, and text literals"));
    }
    let value = cur.value()?;
    if parenthesized {
        cur.expect(Tok::RParen)?;
    }
    Ok(value)
}

fn parse_insert(cur: &mut Cursor) -> Result<Statement> {
    cur.keyword("into")?;
    let table = cur.ident()?;
    let target_columns = if matches!(cur.peek(), Some(Tok::LParen)) {
        cur.next()?;
        let mut columns = Vec::new();
        loop {
            let at = cur.here();
            let column = cur.ident()?;
            if columns.contains(&column) {
                return Err(cur.err_at(at, format!("duplicate INSERT target column `{column}`")));
            }
            columns.push(column);
            let separator_at = cur.here();
            match cur.next()? {
                Tok::Comma => continue,
                Tok::RParen => break,
                other => {
                    return Err(cur.err_at(
                        separator_at,
                        format!("expected `,` or `)`, found {other:?}"),
                    ))
                }
            }
        }
        Some(columns)
    } else {
        None
    };

    if peek_kw(cur, "default") {
        let at = cur.here();
        cur.next()?;
        cur.keyword("values")?;
        if target_columns.is_some() {
            return Err(cur.err_at(at, "DEFAULT VALUES cannot include a target-column list"));
        }
        return Ok(Statement::InsertSchema {
            table,
            target_columns: Some(Vec::new()),
            rows: vec![Vec::new()],
        });
    }
    cur.keyword("values")?;
    let mut rows = Vec::new();
    let mut uses_default = false;
    loop {
        cur.expect(Tok::LParen)?;
        let mut values = Vec::new();
        loop {
            if peek_kw(cur, "default") {
                cur.next()?;
                values.push(InsertValue::Default);
                uses_default = true;
            } else {
                values.push(InsertValue::Literal(cur.value()?));
            }
            let sep = cur.here();
            match cur.next()? {
                Tok::Comma => continue,
                Tok::RParen => break,
                other => {
                    return Err(cur.err_at(sep, format!("expected `,` or `)`, found {other:?}")))
                }
            }
        }
        rows.push(values);
        if !matches!(cur.peek(), Some(Tok::Comma)) {
            break;
        }
        cur.next()?;
    }
    if target_columns.is_some() || uses_default {
        Ok(Statement::InsertSchema {
            table,
            target_columns,
            rows,
        })
    } else if rows.len() == 1 {
        Ok(Statement::Insert {
            table,
            values: rows
                .pop()
                .expect("one row")
                .into_iter()
                .map(|value| match value {
                    InsertValue::Literal(value) => value,
                    InsertValue::Default => unreachable!("handled by rich insert"),
                })
                .collect(),
        })
    } else {
        Ok(Statement::InsertMany {
            table,
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| match value {
                            InsertValue::Literal(value) => value,
                            InsertValue::Default => unreachable!("handled by rich insert"),
                        })
                        .collect()
                })
                .collect(),
        })
    }
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

/// The default output-column label for an aggregate, e.g. `count`, `sum(amount)`.
/// Shared by the executor (to name aggregate result columns) and the `HAVING`
/// parser (to resolve an aggregate reference to that same column).
pub fn agg_label(agg: &Aggregate) -> String {
    let f = match agg.func {
        AggFunc::Count => "count",
        AggFunc::Sum => "sum",
        AggFunc::Min => "min",
        AggFunc::Max => "max",
        AggFunc::Avg => "avg",
    };
    match &agg.column {
        None => f.to_string(),
        Some(c) => format!("{f}({c})"),
    }
}

/// Parse a `HAVING` predicate. Same grammar as a `WHERE` predicate, except a
/// comparison's left side may be an aggregate term (e.g. `COUNT(*) > 5`), which is
/// resolved to its output-column label and matched against the grouped result.
fn parse_having(cur: &mut Cursor) -> Result<HavingPred> {
    let mut left = parse_having_and(cur)?;
    while peek_kw(cur, "or") {
        let at = cur.here();
        cur.next()?;
        cur.predicate_node(at)?;
        let right = parse_having_and(cur)?;
        left = HavingPred::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_having_and(cur: &mut Cursor) -> Result<HavingPred> {
    let mut left = parse_having_compare(cur)?;
    while peek_kw(cur, "and") {
        let at = cur.here();
        cur.next()?;
        cur.predicate_node(at)?;
        let right = parse_having_compare(cur)?;
        left = HavingPred::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_having_compare(cur: &mut Cursor) -> Result<HavingPred> {
    if matches!(cur.peek(), Some(Tok::LParen)) {
        cur.next()?;
        let inner = parse_having(cur)?;
        cur.expect(Tok::RParen)?;
        return Ok(inner);
    }
    let node_at = cur.here();
    cur.predicate_node(node_at)?;
    // The left side is an aggregate term or a grouped output column / alias.
    let is_agg = matches!(cur.peek(), Some(Tok::Word(w)) if agg_func(w).is_some())
        && matches!(cur.peek2(), Some(Tok::LParen));
    let term = if is_agg {
        HavingTerm::Aggregate(parse_aggregate(cur)?)
    } else {
        HavingTerm::Column(cur.column_ref()?)
    };
    let op_at = cur.here();
    let op = match cur.next()? {
        Tok::Eq => CompareOp::Eq,
        Tok::Ne => CompareOp::Ne,
        Tok::Lt => CompareOp::Lt,
        Tok::Le => CompareOp::Le,
        Tok::Gt => CompareOp::Gt,
        Tok::Ge => CompareOp::Ge,
        other => {
            return Err(cur.err_at(
                op_at,
                format!("expected a comparison operator, found {other:?}"),
            ))
        }
    };
    let value = cur.value()?;
    Ok(HavingPred::Compare { term, op, value })
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
    // Keep the simpler Columns form only when every item is a bare, unaliased
    // column; an alias or an aggregate forces the richer Items form.
    let all_plain = items
        .iter()
        .all(|i| matches!(i.expr, SelectExpr::Column(_)) && i.alias.is_none());
    if all_plain {
        let cols = items
            .into_iter()
            .map(|i| match i.expr {
                SelectExpr::Column(c) => c,
                SelectExpr::Aggregate(_) | SelectExpr::Scalar(_) => {
                    unreachable!("all items checked to be columns")
                }
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
    let expr = if is_agg {
        SelectExpr::Aggregate(parse_aggregate(cur)?)
    } else if is_scalar_expression_start(cur) {
        SelectExpr::Scalar(parse_scalar_expr(cur)?)
    } else {
        SelectExpr::Column(cur.column_ref()?)
    };
    // Optional `AS alias`. The alias may not be a clause keyword, so a forgotten
    // alias (`SELECT a AS FROM t`) is a clear error rather than silently eating FROM.
    let alias = if peek_kw(cur, "as") {
        cur.next()?; // consume AS
        let at = cur.here();
        let quoted = cur.ident_is_quoted();
        let name = cur.ident()?;
        if !quoted && is_reserved_word(&name) {
            return Err(cur.err_at(
                at,
                format!("expected an alias name after AS, found keyword `{name}`"),
            ));
        }
        Some(name)
    } else {
        None
    };
    Ok(SelectItem { expr, alias })
}

fn is_scalar_expression_start(cur: &Cursor) -> bool {
    match cur.peek() {
        Some(Tok::Int(_) | Tok::Dec(_) | Tok::Str(_)) => true,
        Some(Tok::Word(word)) if word.eq_ignore_ascii_case("null") => true,
        Some(Tok::Word(word)) if word.eq_ignore_ascii_case("case") => true,
        Some(Tok::Word(_)) => matches!(cur.peek2(), Some(Tok::LParen)),
        _ => false,
    }
}

fn scalar_func(word: &str) -> Option<ScalarFunc> {
    match word.to_ascii_uppercase().as_str() {
        "LOWER" => Some(ScalarFunc::Lower),
        "UPPER" => Some(ScalarFunc::Upper),
        "TRIM" => Some(ScalarFunc::Trim),
        "LENGTH" => Some(ScalarFunc::Length),
        "ABS" => Some(ScalarFunc::Abs),
        "COALESCE" => Some(ScalarFunc::Coalesce),
        "NULLIF" => Some(ScalarFunc::NullIf),
        _ => None,
    }
}

fn parse_scalar_expr(cur: &mut Cursor) -> Result<ScalarExpr> {
    parse_scalar_expr_at_depth(cur, 0)
}

fn parse_scalar_expr_at_depth(cur: &mut Cursor, depth: usize) -> Result<ScalarExpr> {
    if depth > MAX_SCALAR_DEPTH {
        return Err(cur.err(format!(
            "scalar expression nesting exceeds the {MAX_SCALAR_DEPTH}-level limit"
        )));
    }
    if peek_kw(cur, "case") {
        return parse_case(cur, depth);
    }
    if matches!(cur.peek(), Some(Tok::Int(_) | Tok::Dec(_) | Tok::Str(_))) || peek_kw(cur, "null") {
        return cur.value().map(ScalarExpr::Literal);
    }

    let at = cur.here();
    let name = cur.column_ref()?;
    if !matches!(cur.peek(), Some(Tok::LParen)) {
        return Ok(ScalarExpr::Column(name));
    }
    let function = scalar_func(&name)
        .ok_or_else(|| cur.err_at(at, format!("unsupported scalar function `{name}`")))?;
    cur.next()?; // `(`
    let mut arguments = Vec::new();
    if !matches!(cur.peek(), Some(Tok::RParen)) {
        loop {
            arguments.push(parse_scalar_expr_at_depth(cur, depth + 1)?);
            if !matches!(cur.peek(), Some(Tok::Comma)) {
                break;
            }
            cur.next()?;
        }
    }
    cur.expect(Tok::RParen)?;
    let valid_arity = match function {
        ScalarFunc::Coalesce => !arguments.is_empty(),
        ScalarFunc::NullIf => arguments.len() == 2,
        ScalarFunc::Lower
        | ScalarFunc::Upper
        | ScalarFunc::Trim
        | ScalarFunc::Length
        | ScalarFunc::Abs => arguments.len() == 1,
    };
    if !valid_arity {
        let expected = match function {
            ScalarFunc::Coalesce => "at least one argument",
            ScalarFunc::NullIf => "exactly two arguments",
            _ => "exactly one argument",
        };
        return Err(cur.err_at(at, format!("{name} expects {expected}")));
    }
    Ok(ScalarExpr::Function {
        function,
        arguments,
    })
}

fn parse_case(cur: &mut Cursor, depth: usize) -> Result<ScalarExpr> {
    let at = cur.here();
    cur.keyword("case")?;
    let mut branches = Vec::new();
    while peek_kw(cur, "when") {
        cur.next()?;
        let predicate = parse_predicate(cur)?;
        cur.keyword("then")?;
        let value = parse_scalar_expr_at_depth(cur, depth + 1)?;
        branches.push((predicate, value));
    }
    if branches.is_empty() {
        return Err(cur.err_at(at, "CASE requires at least one WHEN branch"));
    }
    let else_expr = if peek_kw(cur, "else") {
        cur.next()?;
        Some(Box::new(parse_scalar_expr_at_depth(cur, depth + 1)?))
    } else {
        None
    };
    cur.keyword("end")?;
    Ok(ScalarExpr::Case {
        branches,
        else_expr,
    })
}

/// Clause keywords that may not be used as a bare alias / output name.
fn is_reserved_word(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "begin"
            | "commit"
            | "rollback"
            | "transaction"
            | "create"
            | "table"
            | "index"
            | "insert"
            | "into"
            | "update"
            | "set"
            | "delete"
            | "drop"
            | "if"
            | "exists"
            | "key"
            | "primary"
            | "unique"
            | "foreign"
            | "not"
            | "null"
            | "is"
            | "in"
            | "between"
            | "like"
            | "asc"
            | "desc"
            | "from"
            | "where"
            | "group"
            | "having"
            | "order"
            | "before"
            | "limit"
            | "offset"
            | "by"
            | "as"
            | "and"
            | "or"
            | "select"
            | "distinct"
            | "join"
            | "inner"
            | "left"
            | "right"
            | "full"
            | "cross"
            | "natural"
            | "outer"
            | "on"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "default"
            | "values"
            | "check"
            | "constraint"
            | "references"
            | "union"
            | "intersect"
            | "except"
            | "returning"
            | "with"
            | "recursive"
            | "using"
            | "window"
            | "over"
            | "fetch"
            | "for"
            | "qualify"
    )
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
        Some(cur.column_ref()?)
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
        let at = cur.here();
        cur.next()?;
        cur.predicate_node(at)?;
        let right = parse_and(cur)?;
        left = Predicate::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(cur: &mut Cursor) -> Result<Predicate> {
    let mut left = parse_comparison(cur)?;
    while matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("and")) {
        let at = cur.here();
        cur.next()?;
        cur.predicate_node(at)?;
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
    let node_at = cur.here();
    cur.predicate_node(node_at)?;
    let column = cur.column_ref()?;

    // Keyword-led predicate forms come before the binary operators: `[NOT] IN`,
    // `[NOT] BETWEEN`, `IS [NOT] NULL`, and `NOT LIKE`.
    if peek_kw(cur, "in") {
        cur.next()?;
        return parse_in(cur, column, false);
    }
    if peek_kw(cur, "between") {
        cur.next()?;
        return parse_between(cur, column, false);
    }
    if peek_kw(cur, "is") {
        cur.next()?;
        let negated = peek_kw(cur, "not");
        if negated {
            cur.next()?;
        }
        cur.keyword("null")?;
        return Ok(Predicate::IsNull { column, negated });
    }
    if peek_kw(cur, "not") {
        cur.next()?;
        if peek_kw(cur, "in") {
            cur.next()?;
            return parse_in(cur, column, true);
        }
        if peek_kw(cur, "between") {
            cur.next()?;
            return parse_between(cur, column, true);
        }
        if peek_kw(cur, "like") {
            cur.next()?;
            let value = cur.value()?;
            return Ok(Predicate::Compare {
                column,
                op: CompareOp::NotLike,
                value,
            });
        }
        let at = cur.here();
        return Err(cur.err_at(at, "expected IN, BETWEEN, or LIKE after NOT"));
    }

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

/// Whether the next token is the given keyword (case-insensitive).
fn peek_kw(cur: &Cursor, kw: &str) -> bool {
    matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(kw))
}

/// Parse the `(v1, v2, ...)` value list of an `IN` predicate (at least one value).
fn parse_in(cur: &mut Cursor, column: String, negated: bool) -> Result<Predicate> {
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
    Ok(Predicate::In {
        column,
        values,
        negated,
    })
}

/// Parse the `low AND high` bounds of a `BETWEEN` predicate.
fn parse_between(cur: &mut Cursor, column: String, negated: bool) -> Result<Predicate> {
    let low = cur.value()?;
    cur.keyword("and")?;
    let high = cur.value()?;
    Ok(Predicate::Between {
        column,
        low,
        high,
        negated,
    })
}

/// Parse one `ORDER BY` key: a column with an optional `ASC`/`DESC` direction.
fn parse_order_key(cur: &mut Cursor) -> Result<OrderBy> {
    let column = cur.column_ref()?;
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
    Ok(OrderBy { column, descending })
}

fn parse_row_count(cur: &mut Cursor, keyword: &str) -> Result<Option<usize>> {
    if !peek_kw(cur, keyword) {
        return Ok(None);
    }
    cur.next()?;
    let at = cur.here();
    match cur.next()? {
        Tok::Int(i) if i >= 0 => usize::try_from(i).map(Some).map_err(|_| {
            cur.err_at(
                at,
                format!(
                    "{} is too large for this platform",
                    keyword.to_ascii_uppercase()
                ),
            )
        }),
        other => Err(cur.err_at(
            at,
            format!(
                "{} expects a non-negative integer, found {other:?}",
                keyword.to_ascii_uppercase()
            ),
        )),
    }
}

/// Parse `table [AS] alias`. A bare alias is accepted only when the next word is
/// not a clause keyword, so `FROM users WHERE ...` cannot consume `WHERE` as an
/// alias. The returned position identifies the exposed qualifier for diagnostics.
fn parse_table_ref(cur: &mut Cursor) -> Result<(TableRef, usize)> {
    let name_at = cur.here();
    let name = cur.ident()?;
    if peek_kw(cur, "as") {
        cur.next()?;
        let alias_at = cur.here();
        let quoted = cur.ident_is_quoted();
        let alias = cur.ident()?;
        if !quoted && is_reserved_word(&alias) {
            return Err(cur.err_at(
                alias_at,
                format!("expected a table alias after AS, found keyword `{alias}`"),
            ));
        }
        return Ok((
            TableRef {
                name,
                alias: Some(alias),
            },
            alias_at,
        ));
    }

    let bare_alias = match cur.peek() {
        Some(Tok::Word(word)) if !is_reserved_word(word) => Some((word.clone(), cur.here())),
        Some(Tok::Ident(identifier)) => Some((identifier.clone(), cur.here())),
        _ => None,
    };
    if let Some((alias, alias_at)) = bare_alias {
        cur.next()?;
        Ok((
            TableRef {
                name,
                alias: Some(alias),
            },
            alias_at,
        ))
    } else {
        Ok((TableRef { name, alias: None }, name_at))
    }
}

fn parse_select(cur: &mut Cursor) -> Result<Statement> {
    // `distinct` is the DISTINCT keyword only when it leads a real projection — not
    // when it is itself the selected column, e.g. `SELECT distinct FROM t` or
    // `SELECT distinct, a FROM t` (mirrors how an aggregate name only counts when
    // followed by `(`).
    let distinct = peek_kw(cur, "distinct")
        && !matches!(cur.peek2(), Some(Tok::Comma))
        && !matches!(cur.peek2(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("from"));
    if distinct {
        cur.next()?; // consume DISTINCT
    }
    let projection = parse_projection(cur)?;
    cur.keyword("from")?;
    let (source, source_at) = parse_table_ref(cur)?;
    let mut qualifiers = vec![(source.qualifier().to_string(), source_at)];
    let mut joins = Vec::new();

    while peek_kw(cur, "join") || peek_kw(cur, "inner") || peek_kw(cur, "left") {
        let left_join = if peek_kw(cur, "left") {
            cur.next()?;
            if peek_kw(cur, "outer") {
                cur.next()?;
            }
            true
        } else {
            if peek_kw(cur, "inner") {
                cur.next()?;
            }
            false
        };
        cur.keyword("join")?;
        let (table, qualifier_at) = parse_table_ref(cur)?;
        let qualifier = table.qualifier().to_string();
        if qualifiers.iter().any(|(known, _)| known == &qualifier) {
            return Err(cur.err_at(
                qualifier_at,
                format!("duplicate table qualifier `{qualifier}`"),
            ));
        }
        qualifiers.push((qualifier, qualifier_at));
        cur.keyword("on")?;
        let first_column = cur.column_ref()?;
        cur.expect(Tok::Eq)?;
        let second_column = cur.column_ref()?;
        joins.push(JoinClause {
            table,
            first_column,
            second_column,
            left_join,
        });
    }

    if peek_kw(cur, "right")
        || peek_kw(cur, "full")
        || peek_kw(cur, "cross")
        || peek_kw(cur, "natural")
    {
        let at = cur.here();
        let kind = cur.ident()?;
        return Err(cur.err_at(
            at,
            format!("unsupported join type `{}`", kind.to_ascii_uppercase()),
        ));
    }

    let filter = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("where")) {
        cur.next()?; // consume WHERE
        Some(parse_predicate(cur)?)
    } else {
        None
    };

    let group_by = if matches!(cur.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("group")) {
        cur.next()?; // consume GROUP
        cur.keyword("by")?;
        let mut cols = vec![cur.column_ref()?];
        while matches!(cur.peek(), Some(Tok::Comma)) {
            cur.next()?;
            cols.push(cur.column_ref()?);
        }
        cols
    } else {
        Vec::new()
    };

    let having = if peek_kw(cur, "having") {
        cur.next()?; // consume HAVING
        Some(parse_having(cur)?)
    } else {
        None
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
        let mut keys = vec![parse_order_key(cur)?];
        while matches!(cur.peek(), Some(Tok::Comma)) {
            cur.next()?; // consume `,`
            keys.push(parse_order_key(cur)?);
        }
        keys
    } else {
        Vec::new()
    };

    let limit = parse_row_count(cur, "limit")?;
    let offset = parse_row_count(cur, "offset")?.unwrap_or(0);

    if source.alias.is_some() || !joins.is_empty() {
        Ok(Statement::SelectJoin {
            projection,
            distinct,
            source,
            joins,
            before,
            filter,
            group_by,
            having,
            order,
            limit,
            offset,
        })
    } else {
        Ok(Statement::Select {
            table: source.name,
            projection,
            distinct,
            before,
            filter,
            group_by,
            having,
            order,
            limit,
            offset,
        })
    }
}

fn parse_update(cur: &mut Cursor) -> Result<Statement> {
    let table = cur.ident()?;
    cur.keyword("set")?;
    let set_column = cur.ident()?;
    cur.expect(Tok::Eq)?;
    let use_default = peek_kw(cur, "default");
    let set_value = if use_default {
        cur.next()?;
        None
    } else {
        Some(cur.value()?)
    };
    cur.keyword("where")?;
    let filter = parse_predicate(cur)?;
    match set_value {
        Some(set_value) => Ok(Statement::Update {
            table,
            set: (set_column, set_value),
            filter,
        }),
        None => Ok(Statement::UpdateDefault {
            table,
            column: set_column,
            filter,
        }),
    }
}

fn parse_drop(cur: &mut Cursor) -> Result<Statement> {
    cur.keyword("table")?;
    let if_exists = if peek_kw(cur, "if") {
        cur.next()?;
        cur.keyword("exists")?;
        true
    } else {
        false
    };
    let table = cur.ident()?;
    if if_exists {
        Ok(Statement::DropTableIfExists { table })
    } else {
        Ok(Statement::DropTable { table })
    }
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
    fn parses_transaction_control() {
        assert_eq!(parse("BEGIN").unwrap(), Statement::Begin);
        assert_eq!(parse("BEGIN TRANSACTION").unwrap(), Statement::Begin);
        assert_eq!(parse("COMMIT").unwrap(), Statement::Commit);
        assert_eq!(parse("ROLLBACK TRANSACTION").unwrap(), Statement::Rollback);
        assert!(parse("COMMIT NOW").is_err());
    }

    #[test]
    fn parses_create_table() {
        assert_eq!(
            parse("CREATE TABLE users (id, name, status)").unwrap(),
            Statement::CreateTable {
                name: "users".into(),
                columns: vec!["id".into(), "name".into(), "status".into()],
                unique_columns: vec![],
                not_null_columns: vec![],
            }
        );
    }

    #[test]
    fn parses_conditional_table_ddl() {
        assert!(matches!(
            parse("CREATE TABLE IF NOT EXISTS cache (id PRIMARY KEY)").unwrap(),
            Statement::CreateTableIfNotExists { name, .. } if name == "cache"
        ));
        assert_eq!(
            parse("DROP TABLE IF EXISTS cache").unwrap(),
            Statement::DropTableIfExists {
                table: "cache".into()
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
    fn parses_multi_row_insert() {
        assert_eq!(
            parse("INSERT INTO users VALUES (1, 'alice'), (2, 'bob')").unwrap(),
            Statement::InsertMany {
                table: "users".into(),
                rows: vec![
                    vec![Value::Int(1), Value::Text("alice".into())],
                    vec![Value::Int(2), Value::Text("bob".into())],
                ],
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
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        assert_eq!(
            parse("SELECT * FROM users BEFORE 7;").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                distinct: false,
                before: Some(7),
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
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
                distinct: false,
                before: None,
                filter: Some(Predicate::eq("status", Value::Text("active".into()))),
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        assert_eq!(
            parse("SELECT * FROM users WHERE id = 5 BEFORE 9 LIMIT 10").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                distinct: false,
                before: Some(9),
                filter: Some(Predicate::eq("id", Value::Int(5))),
                group_by: vec![],
                having: None,
                order: vec![],
                limit: Some(10),
                offset: 0,
            }
        );
    }

    #[test]
    fn parses_offset() {
        match parse("SELECT * FROM users ORDER BY id LIMIT 10 OFFSET 20").unwrap() {
            Statement::Select { limit, offset, .. } => {
                assert_eq!(limit, Some(10));
                assert_eq!(offset, 20);
            }
            other => panic!("expected select, got {other:?}"),
        }
        assert!(parse("SELECT * FROM users OFFSET -1").is_err());
    }

    #[test]
    fn parses_projection_order_and_count() {
        // Column projection.
        assert_eq!(
            parse("SELECT id, name FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Columns(vec!["id".into(), "name".into()]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        // COUNT(*).
        assert_eq!(
            parse("SELECT COUNT(*) FROM users").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::Items(vec![SelectItem {
                    expr: SelectExpr::Aggregate(Aggregate {
                        func: AggFunc::Count,
                        column: None,
                    }),
                    alias: None,
                }]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        // ORDER BY ... DESC.
        assert_eq!(
            parse("SELECT * FROM users ORDER BY name DESC").unwrap(),
            Statement::Select {
                table: "users".into(),
                projection: Projection::All,
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![OrderBy {
                    column: "name".into(),
                    descending: true,
                }],
                limit: None,
                offset: 0,
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
                unique: false,
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
                    SelectItem {
                        expr: SelectExpr::Aggregate(Aggregate {
                            func: AggFunc::Sum,
                            column: Some("amount".into())
                        }),
                        alias: None,
                    },
                    SelectItem {
                        expr: SelectExpr::Aggregate(Aggregate {
                            func: AggFunc::Max,
                            column: Some("id".into())
                        }),
                        alias: None,
                    },
                    SelectItem {
                        expr: SelectExpr::Aggregate(Aggregate {
                            func: AggFunc::Count,
                            column: Some("id".into())
                        }),
                        alias: None,
                    },
                ]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        // SUM(*) is rejected; only COUNT may use `*`.
        assert!(parse("SELECT SUM(*) FROM t").is_err());
        // AVG parses to its own aggregate and requires a column.
        assert_eq!(
            parse("SELECT AVG(amount) FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Items(vec![SelectItem {
                    expr: SelectExpr::Aggregate(Aggregate {
                        func: AggFunc::Avg,
                        column: Some("amount".into()),
                    }),
                    alias: None,
                }]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
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
                    SelectItem {
                        expr: SelectExpr::Column("tier".into()),
                        alias: None,
                    },
                    SelectItem {
                        expr: SelectExpr::Aggregate(Aggregate {
                            func: AggFunc::Count,
                            column: None,
                        }),
                        alias: None,
                    },
                ]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec!["tier".into()],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
        // A column literally named `sum` (no parens) is still a column.
        assert_eq!(
            parse("SELECT sum FROM t").unwrap(),
            Statement::Select {
                table: "t".into(),
                projection: Projection::Columns(vec!["sum".into()]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
    }

    #[test]
    fn parses_distinct_alias_and_multi_order() {
        match parse("SELECT DISTINCT id AS uid FROM t").unwrap() {
            Statement::Select {
                distinct,
                projection: Projection::Items(items),
                ..
            } => {
                assert!(distinct);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].alias.as_deref(), Some("uid"));
                assert!(matches!(&items[0].expr, SelectExpr::Column(c) if c == "id"));
            }
            other => panic!("expected a distinct, aliased select, got {other:?}"),
        }
        match parse("SELECT * FROM t ORDER BY a ASC, b DESC").unwrap() {
            Statement::Select { order, .. } => {
                assert_eq!(order.len(), 2);
                assert_eq!(order[0].column, "a");
                assert!(!order[0].descending);
                assert_eq!(order[1].column, "b");
                assert!(order[1].descending);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_aliased_n_table_join_plan() {
        let statement = parse(
            "SELECT u.name, COUNT(i.label) AS n FROM users AS u \
             JOIN orders o ON o.user_id = u.id \
             LEFT JOIN items AS i ON o.id = i.order_id \
             GROUP BY u.name HAVING COUNT(i.label) > 0 BEFORE 7 ORDER BY u.name",
        )
        .unwrap();
        let Statement::SelectJoin {
            source,
            joins,
            before,
            group_by,
            ..
        } = statement
        else {
            panic!("expected a joined SELECT")
        };
        assert_eq!(source.name, "users");
        assert_eq!(source.alias.as_deref(), Some("u"));
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[0].table.name, "orders");
        assert_eq!(joins[0].table.alias.as_deref(), Some("o"));
        assert!(!joins[0].left_join);
        assert_eq!(joins[1].table.alias.as_deref(), Some("i"));
        assert!(joins[1].left_join);
        assert_eq!(before, Some(7));
        assert_eq!(group_by, ["u.name"]);

        let duplicate = parse("SELECT * FROM users u JOIN orders u ON u.id = u.user_id")
            .unwrap_err()
            .to_string();
        assert!(duplicate.contains("duplicate table qualifier `u`"));
    }

    #[test]
    fn parses_in_between_isnull_not() {
        use CompareOp::*;
        let f = |sql: &str| match parse(sql).unwrap() {
            Statement::Select {
                filter: Some(p), ..
            } => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            f("SELECT * FROM t WHERE x IN (1, 2)"),
            Predicate::In {
                column: "x".into(),
                values: vec![Value::Int(1), Value::Int(2)],
                negated: false,
            }
        );
        assert_eq!(
            f("SELECT * FROM t WHERE x NOT IN (1)"),
            Predicate::In {
                column: "x".into(),
                values: vec![Value::Int(1)],
                negated: true,
            }
        );
        assert_eq!(
            f("SELECT * FROM t WHERE x NOT BETWEEN 1 AND 5"),
            Predicate::Between {
                column: "x".into(),
                low: Value::Int(1),
                high: Value::Int(5),
                negated: true,
            }
        );
        assert_eq!(
            f("SELECT * FROM t WHERE x IS NOT NULL"),
            Predicate::IsNull {
                column: "x".into(),
                negated: true,
            }
        );
        assert_eq!(
            f("SELECT * FROM t WHERE name NOT LIKE 'a%'"),
            Predicate::Compare {
                column: "name".into(),
                op: NotLike,
                value: Value::Text("a%".into()),
            }
        );
        // BETWEEN's inner AND must not swallow a following conjunct.
        assert!(matches!(
            f("SELECT * FROM t WHERE x BETWEEN 1 AND 5 AND y = 2"),
            Predicate::And(_, _)
        ));
        // An IN list needs at least one value, and a bare NOT is a parse error.
        assert!(parse("SELECT * FROM t WHERE x IN ()").is_err());
        assert!(parse("SELECT * FROM t WHERE x NOT = 1").is_err());
    }

    #[test]
    fn parses_having_aggregate_and_column_terms() {
        match parse("SELECT city, COUNT(*) FROM t GROUP BY city HAVING COUNT(*) > 1").unwrap() {
            Statement::Select {
                having: Some(HavingPred::Compare { term, op, value }),
                ..
            } => {
                assert!(matches!(term, HavingTerm::Aggregate(_)));
                assert_eq!(op, CompareOp::Gt);
                assert_eq!(value, Value::Int(1));
            }
            other => panic!("{other:?}"),
        }
        match parse("SELECT city, COUNT(*) AS n FROM t GROUP BY city HAVING n >= 2").unwrap() {
            Statement::Select {
                having: Some(HavingPred::Compare { term, .. }),
                ..
            } => assert!(matches!(term, HavingTerm::Column(c) if c == "n")),
            other => panic!("{other:?}"),
        }
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
    fn quoted_identifiers_preserve_keywords_and_punctuation() {
        assert_eq!(
            parse(r#"SELECT "select", [a;b], `o'brien` FROM "odd table""#).unwrap(),
            Statement::Select {
                table: "odd table".into(),
                projection: Projection::Columns(vec![
                    "select".into(),
                    "a;b".into(),
                    "o'brien".into(),
                ]),
                distinct: false,
                before: None,
                filter: None,
                group_by: vec![],
                having: None,
                order: vec![],
                limit: None,
                offset: 0,
            }
        );
    }

    #[test]
    fn decimal_literal_accepts_the_minimum_mantissa() {
        let sql = format!("INSERT INTO t VALUES ({})", Value::Decimal(i128::MIN));
        assert_eq!(
            parse(&sql).unwrap(),
            Statement::Insert {
                table: "t".into(),
                values: vec![Value::Decimal(i128::MIN)],
            }
        );
    }

    #[test]
    fn parameter_binding_ignores_all_quoted_question_marks() {
        let sql = r#"SELECT '?', "?", `?`, [?] FROM t WHERE id = ?"#;
        assert_eq!(parameter_count(sql), 1);
        assert_eq!(
            bind_params(sql, &[Value::Int(7)]).unwrap(),
            r#"SELECT '?', "?", `?`, [?] FROM t WHERE id = 7"#
        );
    }

    #[test]
    fn parses_schema_defaults_checks_and_default_dml() {
        let statement = parse(
            "CREATE TABLE jobs (id INTEGER PRIMARY KEY, state VARCHAR(20) DEFAULT 'queued', \
             attempts NUMERIC(8, 0) DEFAULT (0) CHECK (attempts >= 0), CHECK (state != 'broken'))",
        )
        .unwrap();
        let Statement::CreateTableSchema {
            columns, checks, ..
        } = statement
        else {
            panic!("expected schema-rich CREATE TABLE")
        };
        assert_eq!(columns.len(), 3);
        assert!(columns[0].unique && columns[0].not_null);
        assert_eq!(columns[1].default, Some(Value::Text("queued".into())));
        assert_eq!(columns[2].default, Some(Value::Int(0)));
        assert_eq!(checks.len(), 2);

        let Statement::InsertSchema {
            target_columns,
            rows,
            ..
        } = parse("INSERT INTO jobs (id, state) VALUES (1, DEFAULT), (2, 'done')").unwrap()
        else {
            panic!("expected schema-aware insert")
        };
        assert_eq!(target_columns.unwrap(), ["id", "state"]);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0][1], InsertValue::Default));
        assert!(matches!(
            parse("INSERT INTO jobs DEFAULT VALUES").unwrap(),
            Statement::InsertSchema {
                target_columns: Some(columns),
                rows,
                ..
            } if columns.is_empty() && rows == vec![Vec::new()]
        ));
        assert!(matches!(
            parse("UPDATE jobs SET state = DEFAULT WHERE id = 1").unwrap(),
            Statement::UpdateDefault { column, .. } if column == "state"
        ));
    }

    #[test]
    fn unsupported_schema_constructs_are_actionable_and_positioned() {
        let error = parse("CREATE TABLE t (id INTEGER DEFAULT CURRENT_TIMESTAMP)")
            .unwrap_err()
            .to_string();
        assert!(error.contains("DEFAULT supports only"), "{error}");
        assert!(error.contains("line 1, column"), "{error}");
        assert!(error.contains('^'), "{error}");

        let error = parse("CREATE TABLE child (id INTEGER REFERENCES parent(id))")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported column constraint"), "{error}");
        assert!(error.contains("`REFERENCES`"), "{error}");

        let error = parse("CREATE TABLE child (id INTEGER, FOREIGN KEY (id) REFERENCES p(id))")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unsupported table constraint `FOREIGN`"),
            "{error}"
        );
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

        let e = parse("SELECT * FROM t UNION SELECT * FROM u")
            .unwrap_err()
            .to_string();
        assert!(e.contains("unsupported or misplaced SQL construct `UNION`"));
        assert!(e.contains("line 1, column 17"), "{e}");
    }

    #[test]
    fn parser_rejects_excessive_nesting_and_predicate_complexity() {
        let nested = format!(
            "SELECT * FROM t WHERE {}id = 1{}",
            "(".repeat(MAX_SQL_PAREN_DEPTH + 1),
            ")".repeat(MAX_SQL_PAREN_DEPTH + 1)
        );
        let error = parse(&nested).unwrap_err().to_string();
        assert!(error.contains("expression nesting exceeds"), "{error}");

        let mut nested_case = "'done'".to_string();
        for _ in 0..=MAX_SCALAR_DEPTH {
            nested_case = format!("CASE WHEN id = 1 THEN {nested_case} END");
        }
        let error = parse(&format!("SELECT {nested_case} FROM t"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("scalar expression nesting exceeds"),
            "{error}"
        );

        let predicate = vec!["id = 1"; MAX_PREDICATE_NODES + 1].join(" AND ");
        let error = parse(&format!("SELECT * FROM t WHERE {predicate}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("predicate complexity exceeds"), "{error}");
    }

    #[test]
    fn statement_boundaries_and_statement_keywords_cannot_be_aliases() {
        assert!(parse("SELECT * FROM t;").is_ok());
        assert!(parse("SELECT * FROM t;;").is_ok());

        for sql in [
            "SELECT * FROM t; DELETE FROM t WHERE id = 1",
            "SELECT * FROM t DELETE",
            "SELECT * FROM t UPDATE",
            "SELECT * FROM t CREATE",
        ] {
            let error = parse(sql).unwrap_err().to_string();
            assert!(
                error.contains("unsupported or misplaced SQL construct"),
                "{sql}: {error}"
            );
        }
    }
}
