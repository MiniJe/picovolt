import picovolt.dbapi2 as pv

db = pv.connect("starter.pv")
db.execute("CREATE TABLE IF NOT EXISTS visits (id PRIMARY KEY, path NOT NULL)")
next_id = db.execute("SELECT COUNT(*) FROM visits").fetchone()[0] + 1
db.execute("INSERT INTO visits VALUES (?, ?)", (next_id, "/"))
db.commit()
db.close()

# Reopen the workspace so the starter verifies durable storage, not merely the
# connection's in-memory view. It is safe to run repeatedly.
reopened = pv.connect("starter.pv")
print(reopened.execute("SELECT * FROM visits").fetchall())
reopened.close()
