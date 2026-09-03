package pvsql

import (
	"database/sql"
	"testing"
)

// Drives the registered database/sql driver with bound parameters, and checks
// that a SQL-injection payload is stored as data rather than executed.
func TestParameterizedQueries(t *testing.T) {
	db, err := sql.Open("picovolt", "memory")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()

	if _, err := db.Exec("CREATE TABLE u (id, name)"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO u VALUES (?, ?)", 1, "o'brien"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO u VALUES (?, ?)", 2, "x'); DROP TABLE u; --"); err != nil {
		t.Fatal(err)
	}

	var name string
	if err := db.QueryRow("SELECT name FROM u WHERE id = ?", 1).Scan(&name); err != nil {
		t.Fatal(err)
	}
	if name != "o'brien" {
		t.Fatalf("got name %q, want o'brien", name)
	}

	var n int
	if err := db.QueryRow("SELECT COUNT(*) FROM u").Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("count = %d, the injection was not contained", n)
	}
}

func TestTransactionsCommitAndRollback(t *testing.T) {
	db, err := sql.Open("picovolt", "memory")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE ledger (id PRIMARY KEY, amount)"); err != nil {
		t.Fatal(err)
	}

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("INSERT INTO ledger VALUES (1, 10)"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}

	tx, err = db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("INSERT INTO ledger VALUES (2, 20)"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}

	var n int
	if err := db.QueryRow("SELECT COUNT(*) FROM ledger").Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("count = %d, want 1 after rollback", n)
	}
}
