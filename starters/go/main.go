package main

import (
	"database/sql"
	"fmt"
	"log"

	_ "github.com/MiniJe/picovolt/bindings/go/pvsql"
)

func main() {
	db, err := sql.Open("picovolt", "starter.pv")
	if err != nil { log.Fatal(err) }
	defer db.Close()
	if _, err = db.Exec("CREATE TABLE visits (id PRIMARY KEY, path NOT NULL)"); err != nil {
		fmt.Println(err)
	}
	if _, err = db.Exec("INSERT INTO visits VALUES (?, ?)", 1, "/"); err != nil { log.Fatal(err) }
}
