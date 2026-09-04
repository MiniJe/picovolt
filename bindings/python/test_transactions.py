import pytest

import picovolt.dbapi2 as dbapi
from picovolt import Database, PicoVoltError


def test_low_level_transaction_commit_and_rollback():
    db = Database.open_memory()
    try:
        db.query("CREATE TABLE ledger (id PRIMARY KEY, amount)")
        db.begin()
        db.query("INSERT INTO ledger VALUES (1, 10)")
        db.rollback()
        assert db.query("SELECT COUNT(*) FROM ledger")["rows"] == [[0]]

        db.begin()
        db.query("INSERT INTO ledger VALUES (2, 20)")
        db.commit()
        assert db.query("SELECT COUNT(*) FROM ledger")["rows"] == [[1]]
    finally:
        db.close()


def test_dbapi_commit_and_rollback():
    con = dbapi.connect("memory")
    try:
        con.execute("CREATE TABLE ledger (id PRIMARY KEY, amount)")
        con.execute("INSERT INTO ledger VALUES (?, ?)", (1, 10))
        con.commit()

        con.execute("INSERT INTO ledger VALUES (?, ?)", (2, 20))
        con.rollback()
        assert con.execute("SELECT COUNT(*) FROM ledger").fetchone() == (1,)
    finally:
        con.close()


def test_prepared_statement_reuses_sql_and_validates_arity():
    db = Database.open_memory()
    try:
        db.query("CREATE TABLE notes (id PRIMARY KEY, body, marker)")
        with pytest.raises(PicoVoltError):
            db.prepare("SELECT FROM")
        with db.prepare("INSERT INTO notes VALUES (?, ?, '?')") as insert:
            assert insert.parameter_count == 2

            insert.execute((1, "o'brien"))
            insert.execute((2, "x'); DROP TABLE notes; --"))

            with pytest.raises(PicoVoltError, match="expects 2 parameters, got 1"):
                insert.execute((3,))

        assert db.query("SELECT COUNT(*) FROM notes")["rows"] == [[2]]
        assert db.query("SELECT body FROM notes WHERE id = 2")["rows"] == [
            ["x'); DROP TABLE notes; --"]
        ]
        with pytest.raises(PicoVoltError, match="statement is closed"):
            insert.execute((3, "three"))
    finally:
        db.close()


def test_prepared_statement_rejects_closed_database():
    db = Database.open_memory()
    db.query("CREATE TABLE closed_db (id)")
    statement = db.prepare("SELECT * FROM closed_db WHERE id = ?")
    db.close()
    try:
        with pytest.raises(PicoVoltError, match="database is closed"):
            statement.execute((1,))
    finally:
        statement.close()
        statement.close()


def test_dbapi_executemany_reuses_prepared_statement():
    con = dbapi.connect("memory")
    try:
        con.execute("CREATE TABLE batches (id PRIMARY KEY, name)")
        cursor = con.executemany(
            "INSERT INTO batches VALUES (?, ?)",
            [(1, "one"), (2, "two"), (3, "three")],
        )
        assert cursor.rowcount == 1
        assert con.execute("SELECT COUNT(*) FROM batches").fetchone() == (3,)
    finally:
        con.close()
