package main

import (
	"database/sql"
	"fmt"
	"log"

	_ "github.com/MiniJe/picovolt/bindings/go/pvsql"
)

func main() {
	db, err := sql.Open("picovolt", "starter.pv")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if _, err = db.Exec(`CREATE TABLE IF NOT EXISTS visits (
		id INTEGER PRIMARY KEY,
		path TEXT NOT NULL,
		source TEXT DEFAULT 'go' CHECK (source IN ('go', 'python', 'rust'))
	)`); err != nil {
		log.Fatal(err)
	}

	var count int64
	if err = db.QueryRow("SELECT COUNT(*) FROM visits").Scan(&count); err != nil {
		log.Fatal(err)
	}
	if _, err = db.Exec(
		"INSERT INTO visits (id, path) VALUES (?, ?)",
		count+1,
		"/",
	); err != nil {
		log.Fatal(err)
	}

	rows, err := db.Query("SELECT id, path FROM visits ORDER BY id DESC LIMIT 3")
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	for rows.Next() {
		var id int64
		var path string
		if err := rows.Scan(&id, &path); err != nil {
			log.Fatal(err)
		}
		fmt.Printf("%d\t%s\n", id, path)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
}
