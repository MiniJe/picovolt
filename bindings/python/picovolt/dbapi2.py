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
blob parameters are unsupported.
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
        self._read_only = database.startswith("prod:")

    def cursor(self) -> "Cursor":
        return Cursor(self)

    def execute(self, sql: str, params=None) -> "Cursor":
        return self.cursor().execute(sql, params)

    def executemany(self, sql: str, seq_of_params) -> "Cursor":
        return self.cursor().executemany(sql, seq_of_params)

    def commit(self) -> None:
        """Commit pending statements."""
        if self._db.in_transaction:
            self._db.commit()

    def rollback(self) -> None:
        """Roll back pending statements."""
        if self._db.in_transaction:
            self._db.rollback()

    def close(self) -> None:
        if self._db.in_transaction:
            self._db.rollback()
        self._db.close()

    def _ensure_transaction(self) -> None:
        if not self._read_only and not self._db.in_transaction:
            self._db.begin()

    def __enter__(self) -> "Connection":
        return self

    def __exit__(self, exc_type: object, _exc: object, _tb: object) -> None:
        if exc_type is None:
            self.commit()
        else:
            self.rollback()
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
            self._con._ensure_transaction()
            res = self._con._db.query(sql, list(params) if params else None)
        except PicoVoltError as exc:
            raise ProgrammingError(str(exc)) from None
        self._set_result(res)
        return self

    def executemany(self, sql: str, seq_of_params) -> "Cursor":
        try:
            statement = self._con._db.prepare(sql)
            try:
                for params in seq_of_params:
                    self._con._ensure_transaction()
                    res = statement.execute(params)
                    self._set_result(res)
            finally:
                statement.close()
        except PicoVoltError as exc:
            raise ProgrammingError(str(exc)) from None
        return self

    def _set_result(self, res) -> None:
        if isinstance(res, dict) and "columns" in res:
            self.description = [(c, None, None, None, None, None, None) for c in res["columns"]]
            self._rows = res["rows"]
            self.rowcount = len(self._rows)
        else:
            self.description = None
            self._rows = []
            self.rowcount = res.get("mutated", -1) if isinstance(res, dict) else -1
        self._idx = 0

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
