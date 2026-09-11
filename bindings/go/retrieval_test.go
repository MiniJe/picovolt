package picovolt

import (
	"strings"
	"testing"
)

func TestRetrieval(t *testing.T) {
	db, err := OpenMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err = db.Query("CREATE TABLE docs(id,body)"); err != nil {
		t.Fatal(err)
	}
	if _, err = db.Query("INSERT INTO docs VALUES(1,'verified backup restore')"); err != nil {
		t.Fatal(err)
	}
	request := `{"kind":"full_text","sql":"SELECT id,body FROM docs","id_column":"id","text_columns":["body"],"query":"backup restore","limit":5}`
	result, err := db.Retrieve(request)
	if err != nil || !strings.Contains(result, `"id":"1"`) {
		t.Fatalf("retrieve: %s %v", result, err)
	}
	if _, err = db.Retrieve(strings.Replace(request, "SELECT id,body FROM docs", "DELETE FROM docs WHERE id=1", 1)); err == nil {
		t.Fatal("retrieval accepted mutation")
	}
	if _, err = db.Retrieve(request + "\x00garbage"); err == nil {
		t.Fatal("retrieval accepted NUL-truncated JSON")
	}
}
