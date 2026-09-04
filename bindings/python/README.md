# PicoVolt for Python

Python bindings for the [PicoVolt](https://github.com/MiniJe/picovolt) embedded
database engine, via its C ABI and `ctypes`. Pure Python: no compiler is needed
on the Python side, only the prebuilt shared library.

Like the engine itself, this is for embedded use (single writer, SQL with MVCC
time-travel, compile-to-`.pvdb`), not a drop-in for a concurrent server's
primary database.

## Install

Platform wheels bundle the native library, so released builds install without a
Rust compiler:

```sh
python -m pip install picovolt
```

Build the shared library from the repository root:

```sh
cargo build --release --features capi
```

That produces `target/release/libpicovolt.so` (Linux),
`target/release/libpicovolt.dylib` (macOS), or `target/release/picovolt.dll`
(Windows). When you run from a checkout, the wrapper finds it in
`target/release` automatically. Otherwise set `PICOVOLT_LIB` to the file:

```sh
export PICOVOLT_LIB=/path/to/libpicovolt.so
```

## Usage

```python
from picovolt import Database

with Database.open_memory() as db:
    db.query("CREATE TABLE users (id, name)")
    db.query("INSERT INTO users VALUES (1, 'alice')")
    print(db.query("SELECT * FROM users"))
    # {'columns': ['id', 'name'], 'rows': [[1, 'alice']]}
```

`query` returns the already-parsed result (a `dict`): `{"columns": [...],
"rows": [[...]]}` for a `SELECT`, `{"mutated": n}` for a mutation, or
`{"done": True}` otherwise. Other methods: `open_dev`, `open_prod`, `from_bytes`,
`export`, `begin`, `commit`, `rollback`, `in_transaction`, `current_tx`, and the
module-level `version()`. A PEP 249 adapter is available as
`picovolt.dbapi2`.

For SQL executed repeatedly, prepare it once. The wrapper caches the SQL text
and positional-parameter count, validates arity before each execution, and
passes values through the same injection-safe binder as `query`:

```python
with db.prepare("INSERT INTO users VALUES (?, ?)") as insert:
    print(insert.parameter_count)  # 2
    insert.execute((1, "Ada"))
    insert.execute((2, "Lin"))
```

Preparation validates SQL and retains a native statement handle. Close the
statement (or use its context manager) before closing the database. A statement
does not own or close its database, and executing it after either handle has
been closed raises `PicoVoltError`.

Run the demo:

```sh
python example.py
```

Release wheels are built for Linux, macOS, and Windows by GitHub Actions. Source
checkouts can instead use `PICOVOLT_LIB` to select a locally built library.
