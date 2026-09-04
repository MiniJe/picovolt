"""Python bindings for the PicoVolt embedded database engine, via its C ABI.

These use ``ctypes`` and load the prebuilt shared library, so there is no build
step on the Python side. Build the library from the repository root first::

    cargo build --release --features capi

Then either run from a checkout (the loader searches ``target/release``) or set
``PICOVOLT_LIB`` to the library path.

Example::

    from picovolt import Database

    db = Database.open_memory()
    db.query("CREATE TABLE users (id, name)")
    db.query("INSERT INTO users VALUES (1, 'alice')")
    print(db.query("SELECT * FROM users"))
    # {'columns': ['id', 'name'], 'rows': [[1, 'alice']]}

A ``Database`` is not safe for concurrent use.
"""

from __future__ import annotations

import ctypes
import json
import os
import sys
from ctypes import POINTER, byref, c_char_p, c_size_t, c_uint8, c_uint64, c_void_p

__all__ = [
    "Database",
    "PreparedStatement",
    "PicoVoltError",
    "version",
    "__version__",
]
__version__ = "1.6.0"


class PicoVoltError(RuntimeError):
    """Raised when a PicoVolt FFI call fails."""


def _candidate_paths() -> list[str]:
    paths: list[str] = []
    override = os.environ.get("PICOVOLT_LIB")
    if override:
        paths.append(override)
    names = {
        "win32": ["picovolt.dll"],
        "darwin": ["libpicovolt.dylib"],
    }.get(sys.platform, ["libpicovolt.so"])
    here = os.path.dirname(os.path.abspath(__file__))
    # A wheel bundles the shared library inside the package directory.
    for name in names:
        paths.append(os.path.join(here, name))
    # In a source checkout, fall back to the cargo build output (repo root is
    # three levels up from this file).
    target = os.path.normpath(os.path.join(here, "..", "..", "..", "target", "release"))
    for name in names:
        paths.append(os.path.join(target, name))
    return paths


def _load_library() -> ctypes.CDLL:
    last_error: OSError | None = None
    for path in _candidate_paths():
        try:
            return ctypes.CDLL(path)
        except OSError as exc:  # not found / wrong arch
            last_error = exc
    raise PicoVoltError(
        "could not load the PicoVolt shared library. Build it with "
        "`cargo build --release --features capi`, or set PICOVOLT_LIB to its "
        f"path. Last loader error: {last_error}"
    )


_lib = _load_library()

# Declare argument and return types so ctypes marshals pointers correctly.
_lib.pv_version.restype = c_char_p
_lib.pv_last_error.restype = c_char_p
_lib.pv_open_memory.restype = c_void_p
_lib.pv_open_dev.restype = c_void_p
_lib.pv_open_dev.argtypes = [c_char_p]
_lib.pv_open_prod.restype = c_void_p
_lib.pv_open_prod.argtypes = [c_char_p]
_lib.pv_query.restype = c_void_p  # char* we own and must free
_lib.pv_query.argtypes = [c_void_p, c_char_p]
_lib.pv_query_params.restype = c_void_p
_lib.pv_query_params.argtypes = [c_void_p, c_char_p, c_char_p]
_lib.pv_prepare.restype = c_void_p
_lib.pv_prepare.argtypes = [c_void_p, c_char_p]
_lib.pv_stmt_parameter_count.restype = c_size_t
_lib.pv_stmt_parameter_count.argtypes = [c_void_p]
_lib.pv_stmt_execute.restype = c_void_p
_lib.pv_stmt_execute.argtypes = [c_void_p, c_void_p, c_char_p]
_lib.pv_stmt_close.restype = None
_lib.pv_stmt_close.argtypes = [c_void_p]
_lib.pv_import_sql.restype = c_void_p
_lib.pv_import_sql.argtypes = [c_void_p, c_char_p]
_lib.pv_current_tx.restype = c_uint64
_lib.pv_current_tx.argtypes = [c_void_p]
_lib.pv_begin_transaction.restype = ctypes.c_int32
_lib.pv_begin_transaction.argtypes = [c_void_p]
_lib.pv_commit_transaction.restype = ctypes.c_int32
_lib.pv_commit_transaction.argtypes = [c_void_p]
_lib.pv_rollback_transaction.restype = ctypes.c_int32
_lib.pv_rollback_transaction.argtypes = [c_void_p]
_lib.pv_in_transaction.restype = ctypes.c_int32
_lib.pv_in_transaction.argtypes = [c_void_p]
_lib.pv_export.restype = c_void_p
_lib.pv_export.argtypes = [c_void_p, POINTER(c_size_t)]
_lib.pv_import.restype = c_void_p
_lib.pv_import.argtypes = [POINTER(c_uint8), c_size_t]
_lib.pv_string_free.restype = None
_lib.pv_string_free.argtypes = [c_void_p]
_lib.pv_bytes_free.restype = None
_lib.pv_bytes_free.argtypes = [c_void_p, c_size_t]
_lib.pv_close.restype = None
_lib.pv_close.argtypes = [c_void_p]


def version() -> str:
    """Return the PicoVolt library version, e.g. ``"1.6.0"``."""
    return _lib.pv_version().decode("utf-8")


def _last_error() -> PicoVoltError:
    msg = _lib.pv_last_error()
    text = msg.decode("utf-8", "replace") if msg else "unknown error"
    return PicoVoltError(text)


class PreparedStatement:
    """A reusable positional-parameter SQL statement.

    The originating database is retained for the statement's lifetime and must
    remain open for execution. Close the statement when it is no longer needed;
    closing it does not close the database.
    """

    def __init__(self, database: "Database", sql: str) -> None:
        self._ptr = None
        self._database = None
        if not isinstance(sql, str):
            raise TypeError("sql must be a string")
        if not database._ptr:
            raise PicoVoltError("database is closed")
        ptr = _lib.pv_prepare(database._ptr, sql.encode("utf-8"))
        if not ptr:
            raise _last_error()
        self._ptr = ptr
        self._database = database
        self.source = sql
        self.parameter_count = int(_lib.pv_stmt_parameter_count(ptr))

    def execute(self, params=()) -> object:
        """Execute the statement with one value per positional ``?``."""
        if not self._ptr:
            raise PicoVoltError("prepared statement is closed")
        if self._database is None or not self._database._ptr:
            raise PicoVoltError("database is closed")
        values = [] if params is None else list(params)
        if len(values) != self.parameter_count:
            raise PicoVoltError(
                "prepared statement expects "
                f"{self.parameter_count} parameters, got {len(values)}"
            )
        payload = json.dumps(values).encode("utf-8")
        ptr = _lib.pv_stmt_execute(self._ptr, self._database._ptr, payload)
        if not ptr:
            raise _last_error()
        try:
            raw = ctypes.string_at(ptr)
        finally:
            _lib.pv_string_free(ptr)
        return json.loads(raw.decode("utf-8"))

    def close(self) -> None:
        """Release the native statement handle. Safe to call twice."""
        if self._ptr:
            _lib.pv_stmt_close(self._ptr)
            self._ptr = None
        self._database = None

    def __enter__(self) -> "PreparedStatement":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass


class Database:
    """A PicoVolt database handle."""

    def __init__(self, ptr: int) -> None:
        self._ptr = ptr

    # --- constructors -----------------------------------------------------
    @classmethod
    def open_memory(cls) -> "Database":
        """Open a new, empty in-memory database."""
        ptr = _lib.pv_open_memory()
        if not ptr:
            raise _last_error()
        return cls(ptr)

    @classmethod
    def open_dev(cls, path: str) -> "Database":
        """Open a development workspace at ``path``."""
        ptr = _lib.pv_open_dev(path.encode("utf-8"))
        if not ptr:
            raise _last_error()
        return cls(ptr)

    @classmethod
    def open_prod(cls, path: str) -> "Database":
        """Open a baked ``.pvdb`` monolith at ``path`` (read-only)."""
        ptr = _lib.pv_open_prod(path.encode("utf-8"))
        if not ptr:
            raise _last_error()
        return cls(ptr)

    @classmethod
    def from_bytes(cls, image: bytes) -> "Database":
        """Open a database from a ``.pvdb`` byte image (e.g. from :meth:`export`)."""
        if not image:
            raise PicoVoltError("empty image")
        buf = (c_uint8 * len(image)).from_buffer_copy(image)
        ptr = _lib.pv_import(buf, c_size_t(len(image)))
        if not ptr:
            raise _last_error()
        return cls(ptr)

    # --- operations -------------------------------------------------------
    def query(self, sql: str, params: object = None) -> object:
        """Run one SQL statement; return the parsed JSON result.

        With ``params`` (a sequence), ``?`` placeholders are bound to the values,
        each substituted as a safely-escaped SQL literal::

            db.query("SELECT * FROM t WHERE id = ?", [1])

        ``SELECT`` -> ``{"columns": [...], "rows": [[...]]}``;
        ``INSERT``/``UPDATE``/``DELETE`` -> ``{"mutated": n}``;
        otherwise ``{"done": True}``.
        """
        if not self._ptr:
            raise PicoVoltError("database is closed")
        if params is None:
            ptr = _lib.pv_query(self._ptr, sql.encode("utf-8"))
        else:
            payload = json.dumps(list(params)).encode("utf-8")
            ptr = _lib.pv_query_params(self._ptr, sql.encode("utf-8"), payload)
        if not ptr:
            raise _last_error()
        try:
            raw = ctypes.string_at(ptr)
        finally:
            _lib.pv_string_free(ptr)
        return json.loads(raw.decode("utf-8"))

    def prepare(self, sql: str) -> PreparedStatement:
        """Validate and retain a reusable positional-parameter statement.

        Values receive the same escaping and type validation as one-shot
        parameterized queries. The caller should close the returned statement
        or use it as a context manager.
        """
        if not self._ptr:
            raise PicoVoltError("database is closed")
        return PreparedStatement(self, sql)

    def import_sql(self, dump: str) -> object:
        """Import a SQL dump (e.g. ``sqlite3 db .dump``). Returns a report dict
        ``{"executed": n, "skipped": [...], "errors": [...]}``."""
        if not self._ptr:
            raise PicoVoltError("database is closed")
        ptr = _lib.pv_import_sql(self._ptr, dump.encode("utf-8"))
        if not ptr:
            raise _last_error()
        try:
            raw = ctypes.string_at(ptr)
        finally:
            _lib.pv_string_free(ptr)
        return json.loads(raw.decode("utf-8"))

    def current_tx(self) -> int:
        """Most recently committed transaction id (upper bound for ``BEFORE tx``)."""
        if not self._ptr:
            return 0
        return int(_lib.pv_current_tx(self._ptr))

    def begin(self) -> None:
        """Begin an explicit multi-statement transaction."""
        self._transaction_call(_lib.pv_begin_transaction)

    def commit(self) -> None:
        """Commit the active transaction."""
        self._transaction_call(_lib.pv_commit_transaction)

    def rollback(self) -> None:
        """Roll back the active transaction."""
        self._transaction_call(_lib.pv_rollback_transaction)

    @property
    def in_transaction(self) -> bool:
        """Whether an explicit transaction is active."""
        if not self._ptr:
            return False
        return bool(_lib.pv_in_transaction(self._ptr))

    def _transaction_call(self, function: object) -> None:
        if not self._ptr:
            raise PicoVoltError("database is closed")
        if not function(self._ptr):
            raise _last_error()

    def export(self) -> bytes:
        """Export the whole database as a ``.pvdb`` byte image."""
        if not self._ptr:
            raise PicoVoltError("database is closed")
        n = c_size_t(0)
        ptr = _lib.pv_export(self._ptr, byref(n))
        if not ptr:
            raise _last_error()
        try:
            data = ctypes.string_at(ptr, n.value)
        finally:
            _lib.pv_bytes_free(ptr, n.value)
        return bytes(data)

    def close(self) -> None:
        """Close the database and release its resources. Safe to call twice."""
        if self._ptr:
            try:
                if self.in_transaction:
                    self.rollback()
            finally:
                _lib.pv_close(self._ptr)
                self._ptr = None

    # --- lifecycle sugar --------------------------------------------------
    def __enter__(self) -> "Database":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass
