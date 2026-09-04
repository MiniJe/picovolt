// A runnable PicoVolt demo from Go: prepared writes, schema constraints,
// scalar expressions, an aggregate, and time-travel.
//
// Install or download the matching native PicoVolt C ABI library, set
// CGO_LDFLAGS and the platform loader path as described in ../README.md, then
// run `go run .`. The Go module provides its own C header and does not require a
// PicoVolt source checkout.
package main

import (
	"fmt"
	"log"

	picovolt "github.com/MiniJe/picovolt/bindings/go"
)

func main() {
	fmt.Println("PicoVolt", picovolt.Version())

	db, err := picovolt.OpenMemory()
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()

	must := func(_ string, err error) {
		if err != nil {
			log.Fatal(err)
		}
	}

	must(db.Query(
		"CREATE TABLE fruit (" +
			"name TEXT PRIMARY KEY, " +
			"qty INTEGER DEFAULT 0 CHECK (qty >= 0))",
	))
	insert, err := db.Prepare("INSERT INTO fruit (name, qty) VALUES (?, ?)")
	if err != nil {
		log.Fatal(err)
	}
	defer insert.Close()
	must(insert.Execute("apple", 3))
	must(insert.Execute("pear", 5))

	// "BEFORE n" reads the table as of transaction n (inclusive); the last
	// insert is the newest tx, so this snapshot predates the delete below.
	afterInserts := db.CurrentTx()

	must(db.Query("DELETE FROM fruit WHERE name = 'pear'"))

	rows, err := db.Query(
		"SELECT UPPER(name) AS name, qty, " +
			"CASE WHEN qty >= 5 THEN 'stocked' ELSE 'low' END AS status " +
			"FROM fruit ORDER BY name",
	)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("now:           ", rows)

	// Time-travel: the table as it was before the delete.
	past, err := db.Query(fmt.Sprintf("SELECT * FROM fruit BEFORE %d", afterInserts))
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("before delete: ", past)

	total, err := db.Query("SELECT SUM(qty) FROM fruit")
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("sum(qty) now:  ", total)
}
