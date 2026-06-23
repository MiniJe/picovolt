"""A PEP 249 (DB-API 2.0) interface over PicoVolt.

Code written against the standard database API (as for ``sqlite3``) can use
PicoVolt with minimal change::

    import picovolt.dbapi2 as sqlite

    con = sqlite.connect("memory")          # or "dev:./app.pv", "prod:app.pvdb"
    cur = con.cursor()
    cur.execute("CREATE TABLE t (id, name)")
    cur.execute("INSERT INTO t VALUES (?, ?)", (1, "alice"))
    cur.execute("SELECT * FROM t WHERE id = ?", (1,))
    print(cur.fetchall())                   # [(1, 'alice')]

Limitations: parameters are positional ``?`` only (``paramstyle = "qmark"``);
there are no multi-statement transactions, so ``commit`` and ``rollback`` are
no-ops; blob parameters are unsupported.
"""

from __future__ import annotations

from . import Database, PicoVoltError

apilevel = "2.0"
threadsafety = 1
paramstyle = "qmark"

__all__ = [
    "connect", "Connection", "Cursor",
    "Error", "DatabaseError", "ProgrammingError",
    "apilevel", "threadsafety", "paramstyle",
]


class Error(Exception):
    """Base of the DB-API exception hierarchy."""


class DatabaseError(Error):
    pass


class ProgrammingError(DatabaseError):
    pass


def connect(database: str = "memory") -> "Connection":
    """Open a connection. ``database`` is "memory" (default), "dev:<path>", or
    "prod:<path>"."""
    return Connection(database)


class Connection:
    def __init__(self, database: str = "memory") -> None:
        if database in ("", "memory", ":memory:"):
            self._db = Database.open_memory()
        elif database.startswith("dev:"):
            self._db = Database.open_dev(database[4:])
        elif database.startswith("prod:"):
            self._db = Database.open_prod(database[5:])
        else:
            self._db = Database.open_dev(database)

    def cursor(self) -> "Cursor":
        return Cursor(self)

    def execute(self, sql: str, params=None) -> "Cursor":
        return self.cursor().execute(sql, params)

    def commit(self) -> None:
        """No-op: PicoVolt autocommits each statement."""

    def rollback(self) -> None:
        """No-op: PicoVolt has no multi-statement transactions."""

    def close(self) -> None:
        self._db.close()

    def __enter__(self) -> "Connection":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


class Cursor:
    def __init__(self, con: "Connection") -> None:
        self._con = con
        self._rows: list = []
        self._idx = 0
        self.description = None
        self.rowcount = -1
        self.arraysize = 1

    def execute(self, sql: str, params=None) -> "Cursor":
        try:
            res = self._con._db.query(sql, list(params) if params else None)
        except PicoVoltError as exc:
            raise ProgrammingError(str(exc)) from None
        if isinstance(res, dict) and "columns" in res:
            self.description = [(c, None, None, None, None, None, None) for c in res["columns"]]
            self._rows = res["rows"]
            self.rowcount = len(self._rows)
        else:
            self.description = None
            self._rows = []
            self.rowcount = res.get("mutated", -1) if isinstance(res, dict) else -1
        self._idx = 0
        return self

    def executemany(self, sql: str, seq_of_params) -> "Cursor":
        for params in seq_of_params:
            self.execute(sql, params)
        return self

    def fetchone(self):
        if self._idx >= len(self._rows):
            return None
        row = self._rows[self._idx]
        self._idx += 1
        return tuple(row)

    def fetchmany(self, size: int = None):
        n = self.arraysize if size is None else size
        out = [tuple(r) for r in self._rows[self._idx:self._idx + n]]
        self._idx += len(out)
        return out

    def fetchall(self):
        out = [tuple(r) for r in self._rows[self._idx:]]
        self._idx = len(self._rows)
        return out

    def close(self) -> None:
        pass

    def __iter__(self) -> "Cursor":
        return self

    def __next__(self):
        row = self.fetchone()
        if row is None:
            raise StopIteration
        return row
