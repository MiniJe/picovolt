import picovolt.dbapi2 as pv

db = pv.connect("starter.pv")
db.execute("CREATE TABLE visits (id PRIMARY KEY, path NOT NULL)")
db.execute("INSERT INTO visits VALUES (?, ?)", (1, "/"))
print(db.execute("SELECT * FROM visits").fetchall())
db.close()
