import picovolt.dbapi2 as dbapi
from picovolt import Database


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
