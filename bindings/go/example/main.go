// A runnable PicoVolt demo from Go: CRUD, an aggregate, and time-travel.
//
// From the repository root:
//
//	cargo build --release --features capi
//	cd bindings/go/example
//	# point the loader at the freshly built library, then run:
//	#   Linux:   LD_LIBRARY_PATH=../../../target/release go run .
//	#   macOS:   DYLD_LIBRARY_PATH=../../../target/release go run .
//	#   Windows: set PATH to include ..\..\..\target\release, then: go run .
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

	must(db.Query("CREATE TABLE fruit (name, qty)"))
	must(db.Query("INSERT INTO fruit VALUES ('apple', 3)"))
	must(db.Query("INSERT INTO fruit VALUES ('pear', 5)"))

	// "BEFORE n" reads the table as of transaction n (inclusive); the last
	// insert is the newest tx, so this snapshot predates the delete below.
	afterInserts := db.CurrentTx()

	must(db.Query("DELETE FROM fruit WHERE name = 'pear'"))

	rows, err := db.Query("SELECT * FROM fruit")
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
