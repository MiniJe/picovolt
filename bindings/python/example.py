"""A runnable PicoVolt demo: prepared writes, constraints, and time-travel.

From the repository root::

    cargo build --release --features capi
    python bindings/python/example.py
"""

import os
import sys

# Allow running straight from a checkout without installing the package.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from picovolt import Database, version  # noqa: E402


def main() -> None:
    print("PicoVolt", version())

    with Database.open_memory() as db:
        db.query(
            "CREATE TABLE fruit ("
            "name TEXT PRIMARY KEY, "
            "qty INTEGER DEFAULT 0 CHECK (qty >= 0))"
        )
        with db.prepare("INSERT INTO fruit (name, qty) VALUES (?, ?)") as insert:
            insert.execute(("apple", 3))
            insert.execute(("pear", 5))

        # `BEFORE n` reads the table as of transaction n (inclusive); the last
        # insert is the newest tx, so this snapshot predates the delete below.
        after_inserts = db.current_tx()
        db.query("DELETE FROM fruit WHERE name = 'pear'")

        print(
            "now:           ",
            db.query(
                "SELECT UPPER(name) AS name, qty, "
                "CASE WHEN qty >= 5 THEN 'stocked' ELSE 'low' END AS status "
                "FROM fruit ORDER BY name"
            ),
        )
        print(
            "before delete: ",
            db.query(f"SELECT * FROM fruit BEFORE {after_inserts}"),
        )
        print("avg(qty) now:  ", db.query("SELECT AVG(qty) FROM fruit"))

        # Round-trip the whole database through a .pvdb byte image.
        image = db.export()
        restored = Database.from_bytes(image)
        print("restored rows: ", restored.query("SELECT * FROM fruit"))
        restored.close()


if __name__ == "__main__":
    main()
