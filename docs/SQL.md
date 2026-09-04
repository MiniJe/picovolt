# SQL compatibility

PicoVolt implements a deliberately small SQL subset for embedded applications.
It is not a drop-in SQLite or PostgreSQL engine. The same parser and behavior are
used by the Rust, JavaScript, Python, Go, C, CLI, and HTTP surfaces.

## Supported statements

- `CREATE TABLE [IF NOT EXISTS]` with optional schema-light type declarations,
  column-level `PRIMARY KEY`, `UNIQUE`, `NOT NULL`, and literal `DEFAULT`
  constraints, plus column- or table-level `CHECK` predicates
- `CREATE [UNIQUE] INDEX ON table (column)`
- single-row and multi-row `INSERT`, optional target-column lists, explicit
  `DEFAULT`, and `DEFAULT VALUES`
- `UPDATE table SET column = value | DEFAULT WHERE ...`
- `DELETE FROM table WHERE ...`
- `DROP TABLE [IF EXISTS] table`
- `SELECT`, including projection, `DISTINCT`, aggregates, equality joins,
  filtering, grouping, time travel, ordering, and pagination
- `BEGIN`, `COMMIT`, and `ROLLBACK` on stateful database handles
- `EXPLAIN SELECT ...` returns `step`, `operation`, and `detail` columns without
  scanning rows. Indexed equality joins are shown as adaptive: execution probes
  once per distinct left key when the estimate favors it and otherwise builds a
  right-side map. See [Data tools](DATA_TOOLS.md) for interpretation and limits

Positional `?` parameters work through the parameterized and prepared-statement
APIs. Parameters represent values, not table or column names.

Identifiers may be bare words or delimited with SQL double quotes, SQLite-style
backticks, or square brackets. Delimit names that contain punctuation, spaces,
or SQL keywords; doubled closing delimiters represent a literal delimiter.

## Schema, defaults, and checks

Column type declarations are optional. PicoVolt accepts common compatibility
forms such as `INTEGER`, `TEXT`, `VARCHAR(40)`, and `NUMERIC(8, 0)`, as well as
other single-word type names. A type modifier may be `(positive_size)` or
`(positive_precision, nonnegative_scale)`.

These declarations are schema-light syntax: values remain dynamically typed,
and PicoVolt does not coerce values, reject a value because of its declared type,
limit text length, or enforce decimal precision. Use constraints when the shape
of stored data must be enforced.

```sql
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY,
  state TEXT NOT NULL DEFAULT 'queued',
  attempts INTEGER DEFAULT 0 CHECK (attempts >= 0),
  note VARCHAR(40),
  CHECK (state IN ('queued', 'running', 'done'))
);
```

`DEFAULT` accepts only deterministic `NULL`, integer, decimal, and text
literals. A named-column insert fills each omitted column from its declared
default, or with `NULL` when that column has no default. The `DEFAULT` keyword in
a values list does the same for that position.

```sql
INSERT INTO jobs (id, note) VALUES (1, 'first');
INSERT INTO jobs VALUES (2, DEFAULT, DEFAULT, NULL);
INSERT INTO jobs (id, state) VALUES (3, DEFAULT), (4, 'done');
UPDATE jobs SET state = DEFAULT WHERE id = 4;
```

`INSERT INTO table DEFAULT VALUES` creates one row using every declared default;
columns without defaults become `NULL`, after which all constraints are checked.
It cannot be combined with a target-column list.

```sql
CREATE TABLE settings (
  enabled INTEGER DEFAULT 1,
  mode TEXT DEFAULT 'safe',
  CHECK (enabled IN (0, 1)),
  CHECK (mode != 'broken')
);

INSERT INTO settings DEFAULT VALUES;
```

Column- and table-level `CHECK` constraints share the predicate subset used by
`WHERE`: a column compared with a literal using `=`, `!=`/`<>`, `<`, `<=`, `>`,
`>=`, `LIKE`, or `NOT LIKE`; `[NOT] IN` with a literal list; `[NOT] BETWEEN`
literal bounds; `IS [NOT] NULL`; and combinations using `AND`, `OR`, and
parentheses. Checks can refer to any column in the table.

Checks use SQL three-valued logic. Only `FALSE` violates a constraint; `TRUE`
and `UNKNOWN` pass. For example, `NULL` satisfies `CHECK (attempts >= 0)` because
that comparison is unknown. Add `NOT NULL` when `NULL` must also be rejected.

Constraint failures are statement-atomic. PicoVolt validates every proposed row
before changing the table, so one `PRIMARY KEY`, `UNIQUE`, `NOT NULL`, or `CHECK`
failure leaves an entire multi-row `INSERT` or matching-row `UPDATE` unchanged.
Compound writes also enter a rollback boundary after validation, so an I/O
failure cannot leave a committed prefix of a multi-row `INSERT`/`DELETE` or an
`UPDATE` with tombstoned originals but missing replacements. If such a mutation-
phase failure happens inside an explicit transaction, PicoVolt aborts the whole
transaction because savepoints are not yet available. Preflight and constraint
errors occur before that boundary and leave the explicit transaction open.

Clauses in a `SELECT` appear in this order:

```text
SELECT [DISTINCT] projection
FROM table [[AS] alias]
  { [INNER | LEFT [OUTER]] JOIN table [[AS] alias] ON column = column }...
[WHERE predicate]
[GROUP BY column, ...]
[HAVING predicate]
[BEFORE transaction_id]
[ORDER BY column [ASC | DESC], ...]
[LIMIT count] [OFFSET count]
```

`BEFORE` is PicoVolt's time-travel clause; it pins the query to the supplied
historical transaction identifier.

## Table aliases and joins

Table aliases may use `AS` or the common bare form:

```sql
SELECT u.name, o.total
FROM users AS u
JOIN orders o ON u.id = o.user_id
ORDER BY o.total DESC;
```

PicoVolt accepts any number of `JOIN`, `INNER JOIN`, and `LEFT [OUTER] JOIN`
clauses. Each `ON` clause must be one equality between a column from the newly
joined table and a column from any earlier table. The operands may appear in
either order.

```sql
SELECT u.name, o.id, i.label
FROM users u
JOIN orders o ON o.user_id = u.id
LEFT JOIN items i ON i.order_id = o.id
WHERE u.active = 1
ORDER BY u.name, i.label;
```

Joins are evaluated from left to right. A `LEFT JOIN` retains an unmatched row
from the accumulated left side and fills the new table's columns with `NULL`.
`BEFORE tx` applies the same MVCC snapshot to every table in the query.

Use distinct aliases for self-joins:

```sql
SELECT employee.name AS employee, manager.name AS manager
FROM people employee
LEFT JOIN people manager ON manager.id = employee.manager_id
ORDER BY employee.id;
```

Once a table has an alias, use that alias as its qualifier. In projection,
filtering, grouping, and ordering, an unqualified column is accepted only when it
resolves unambiguously. Qualifying both `ON` operands is strongly recommended;
duplicate qualifiers and ambiguous references are errors.

Aggregate and grouped joins use the same syntax as single-table queries:

```sql
SELECT u.name, COUNT(i.label) AS item_count
FROM users u
LEFT JOIN orders o ON u.id = o.user_id
LEFT JOIN items i ON o.id = i.order_id
GROUP BY u.name
HAVING COUNT(i.label) > 0
ORDER BY item_count DESC;
```

Supported aggregates are `COUNT`, `SUM`, `MIN`, `MAX`, and `AVG`. Only
`COUNT(*)` accepts `*`; the other forms take one column. `COUNT(column)` ignores
`NULL`, and `SUM`/`MIN`/`MAX`/`AVG` return `NULL` for an empty or all-null input.

## CASE and scalar functions

PicoVolt supports searched `CASE` expressions in the select list. The first true
`WHEN` branch wins. When no branch matches, PicoVolt evaluates `ELSE`, or returns
`NULL` when `ELSE` is absent.

```sql
SELECT
  name,
  CASE
    WHEN score >= 90 THEN 'excellent'
    WHEN score >= 70 THEN 'passing'
    ELSE 'review'
  END AS result
FROM students
ORDER BY name;
```

`WHEN` accepts the normal PicoVolt predicate subset: comparisons with literals,
`IN`, `BETWEEN`, `IS [NOT] NULL`, `LIKE`, and combinations using `AND`, `OR`, and
parentheses.

The following scalar functions are available in select-list expressions and may
be nested:

| Function | Arguments | Result |
|---|---:|---|
| `LOWER(value)` | 1 | Unicode lowercase text; `NULL` stays `NULL` |
| `UPPER(value)` | 1 | Unicode uppercase text; `NULL` stays `NULL` |
| `TRIM(value)` | 1 | Text without leading/trailing Unicode whitespace |
| `LENGTH(value)` | 1 | Unicode scalar count for text or byte count for blobs |
| `ABS(value)` | 1 | Absolute integer or decimal value, with overflow checking |
| `COALESCE(a, ...)` | 1 or more | First non-`NULL` value, otherwise `NULL` |
| `NULLIF(a, b)` | 2 | `NULL` when the values compare equal, otherwise `a` |

```sql
SELECT
  UPPER(TRIM(name)) AS display_name,
  COALESCE(NULLIF(TRIM(city), ''), 'unknown') AS city,
  ABS(balance_delta) AS magnitude
FROM accounts;
```

Text functions reject non-text, non-null values. `LENGTH` also accepts blobs, and
`ABS` accepts integers and fixed-point decimals. PicoVolt reports wrong argument
counts, unsupported functions, type mismatches, and numeric overflow as errors
instead of coercing values silently.

## Predicates, ordering, and values

`WHERE` supports `=`, `!=`/`<>`, `<`, `<=`, `>`, `>=`, `LIKE`, `NOT LIKE`,
`[NOT] IN`, `[NOT] BETWEEN`, and `IS [NOT] NULL`. `AND` binds more tightly than
`OR`; parentheses override precedence. Use `IS NULL` rather than `= NULL`.
Comparisons with `NULL` produce `UNKNOWN`; `WHERE`, `HAVING`, and `CASE WHEN`
discard that result, while `CHECK` accepts it as described above.

`ORDER BY` accepts one or more columns with optional `ASC` or `DESC`, followed by
optional `LIMIT` and `OFFSET`. Joined rows may be filtered, grouped, sorted, and
projected by qualified columns. Integer and decimal values compare by numeric
magnitude in predicates, joins, and numeric aggregates.

## Current limits

- Declared types and size/precision modifiers are compatibility syntax, not
  enforced static types. Foreign keys/`REFERENCES`, generated columns, and
  function or expression defaults such as `CURRENT_TIMESTAMP` are unsupported.
- `PRIMARY KEY` and `UNIQUE` constraints are column-level only; named and
  table-level forms are unsupported. `UPDATE` assigns one column per statement.
- `CHECK` operands are columns and literals from the documented predicate
  subset; column-to-column comparisons, functions, and general expressions are
  unsupported.
- Join types are limited to `INNER` and `LEFT`; `RIGHT`, `FULL`, `CROSS`, and
  `NATURAL` joins are unsupported.
- `ON` accepts one column equality, not arbitrary predicates, composite keys, or
  expressions.
- Scalar functions and `CASE` are select-list expressions. They cannot yet be
  placed directly in `WHERE`, `JOIN ON`, `GROUP BY`, `HAVING`, `ORDER BY`, or an
  aggregate argument, and scalar expressions cannot be mixed with grouping or
  aggregates in the same select list.
- PicoVolt supports searched `CASE WHEN predicate THEN ...`; simple
  `CASE value WHEN ...` is unsupported.
- Qualified wildcards such as `u.*`, subqueries, common table expressions,
  `UNION`, window functions, and arithmetic expressions are unsupported.
- Bare identifiers contain letters, numbers, or `_`. Use `"name"`, `` `name` ``,
  or `[name]` for punctuation, spaces, or a name that is also a SQL keyword.
Parser errors identify the line and column and draw a caret under the offending
token. Missing or ambiguous columns, duplicate table qualifiers, unsupported
join types, and unsupported scalar functions produce explicit errors. Unsupported
SQL should be treated as an application compatibility issue rather than assumed
to have SQLite semantics. Recursive expressions and predicates are bounded, so
machine-generated or untrusted SQL that exceeds those limits is rejected rather
than exhausting the process stack.
