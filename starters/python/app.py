import picovolt.dbapi2 as pv

with pv.connect("starter.pv") as db:
    db.execute(
        "CREATE TABLE IF NOT EXISTS visits ("
        "id INTEGER PRIMARY KEY, "
        "path TEXT NOT NULL, "
        "source TEXT DEFAULT 'python' "
        "CHECK (source IN ('python', 'go', 'rust')))"
    )
    next_id = db.execute("SELECT COUNT(*) FROM visits").fetchone()[0] + 1
    db.execute("INSERT INTO visits (id, path) VALUES (?, ?)", (next_id, "/"))

# Reopen the workspace so the starter verifies durable storage, not merely the
# connection's in-memory view. It is safe to run repeatedly.
with pv.connect("starter.pv") as reopened:
    print(reopened.execute("SELECT * FROM visits ORDER BY id DESC LIMIT 3").fetchall())
