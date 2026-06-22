"""A runnable PicoVolt demo from Python: CRUD, an aggregate, and time-travel.

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
        db.query("CREATE TABLE fruit (name, qty)")
        db.query("INSERT INTO fruit VALUES ('apple', 3)")
        db.query("INSERT INTO fruit VALUES ('pear', 5)")

        # `BEFORE n` reads the table as of transaction n (inclusive); the last
        # insert is the newest tx, so this snapshot predates the delete below.
        after_inserts = db.current_tx()
        db.query("DELETE FROM fruit WHERE name = 'pear'")

        print("now:           ", db.query("SELECT * FROM fruit"))
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
