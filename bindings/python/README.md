# PicoVolt for Python

Python bindings for the [PicoVolt](https://github.com/MiniJe/picovolt) embedded
database engine, via its C ABI and `ctypes`. Pure Python: no compiler is needed
on the Python side, only the prebuilt shared library.

Like the engine itself, this is for embedded use (single writer, SQL with MVCC
time-travel, compile-to-`.pvdb`), not a drop-in for a concurrent server's
primary database.

## Setup

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
`export`, `current_tx`, and the module-level `version()`.

Run the demo:

```sh
python example.py
```

Packaging per-platform wheels that bundle the compiled library is future work;
for now this wraps a library you build or ship yourself.
