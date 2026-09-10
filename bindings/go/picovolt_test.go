package picovolt

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestAtomicBatchAndCommitLog(t *testing.T) {
	db, err := OpenDev(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if err = db.EnableCommitLog(CommitLogOptions{}); err != nil {
		t.Fatal(err)
	}
	if _, err = db.Query("CREATE TABLE t (id PRIMARY KEY)"); err != nil {
		t.Fatal(err)
	}
	if n, e := db.ExecuteMany("INSERT INTO t VALUES (?)", [][]any{{1}, {2}}); e != nil || n != 2 {
		t.Fatalf("count %d, error %v", n, e)
	}
	if _, e := db.ExecuteMany("INSERT INTO t VALUES (?)", [][]any{{3}, {1}}); e == nil {
		t.Fatal("expected atomic batch failure")
	}
	status, e := db.CommitLogStatus()
	if e != nil {
		t.Fatal(e)
	}
	var s struct {
		Head uint64 `json:"head_sequence"`
	}
	if e = json.Unmarshal([]byte(status), &s); e != nil || s.Head != 2 {
		t.Fatalf("status %s, %v", status, e)
	}
	if _, e = db.ChangesSince(0, 64); e != nil {
		t.Fatal(e)
	}
	if e = db.PruneChanges(s.Head); e != nil {
		t.Fatal(e)
	}
	if _, e = db.ChangesSince(0, 64); e == nil {
		t.Fatal("expected expired cursor")
	}
}

func TestPreparedStatementReuseAndArity(t *testing.T) {
	db, err := OpenMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Query("CREATE TABLE notes (id PRIMARY KEY, body, marker)"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Prepare("SELECT FROM"); err == nil {
		t.Fatal("expected Prepare to validate SQL")
	}

	insert, err := db.Prepare("INSERT INTO notes VALUES (?, ?, '?')")
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	if got := insert.ParameterCount(); got != 2 {
		t.Fatalf("ParameterCount() = %d, want 2", got)
	}
	if _, err := insert.Execute(1, "o'brien"); err != nil {
		t.Fatal(err)
	}
	if _, err := insert.Execute(2, "x'); DROP TABLE notes; --"); err != nil {
		t.Fatal(err)
	}
	if _, err := insert.Execute(3); err == nil || !strings.Contains(err.Error(), "expects 2 parameters, got 1") {
		t.Fatalf("wrong-arity Execute error = %v", err)
	}

	out, err := db.Query("SELECT COUNT(*) FROM notes")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "[[2]]") {
		t.Fatalf("unexpected count result: %s", out)
	}
}

func TestClosedPreparedStatementRejectsExecution(t *testing.T) {
	db, err := OpenMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Query("CREATE TABLE closed_statement (id)"); err != nil {
		t.Fatal(err)
	}
	statement, err := db.Prepare("SELECT * FROM closed_statement WHERE id = ?")
	if err != nil {
		t.Fatal(err)
	}
	statement.Close()
	statement.Close()
	if _, err := statement.Execute(1); err == nil || !strings.Contains(err.Error(), "statement is closed") {
		t.Fatalf("Execute after Close error = %v", err)
	}
}

func TestPreparedStatementRejectsClosedDatabase(t *testing.T) {
	db, err := OpenMemory()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Query("CREATE TABLE closed_db (id)"); err != nil {
		t.Fatal(err)
	}
	statement, err := db.Prepare("SELECT * FROM closed_db WHERE id = ?")
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	if _, err := statement.Execute(1); err == nil || !strings.Contains(err.Error(), "database is closed") {
		t.Fatalf("Execute after DB.Close error = %v", err)
	}
	statement.Close()
	statement.Close()
}
