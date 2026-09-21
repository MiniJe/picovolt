package picovolt

import (
	"encoding/json"
	"path/filepath"
	"testing"
)

func TestPersistentRetrievalLifecycle(t *testing.T) {
	root := filepath.Join(t.TempDir(), "workspace.pv")
	db, err := OpenDev(root)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { db.Close() }()
	for _, sql := range []string{
		"CREATE TABLE docs (id,tenant,body,embedding)",
		"INSERT INTO docs VALUES (1,'a','apple apple','[1,0]'),(2,'b','apple','[0,1]')",
		"CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')",
		"CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)",
	} {
		if _, err := db.Query(sql); err != nil {
			t.Fatal(err)
		}
	}
	compare := func() {
		t.Helper()
		for _, kind := range []string{"full_text", "vector", "hybrid"} {
			request := map[string]any{"kind": kind, "sql": "SELECT * FROM docs WHERE tenant='a'", "id_column": "id", "limit": 10}
			if kind != "vector" {
				request["text_columns"] = []string{"body"}
				request["query"] = "apple"
			}
			if kind != "full_text" {
				request["vector_column"] = "embedding"
				request["metric"] = "cosine"
			}
			if kind == "vector" {
				request["query"] = []float32{1, 0}
			}
			if kind == "hybrid" {
				request["vector_query"] = []float32{1, 0}
				request["text_weight"] = 0.5
				request["candidate_limit"] = 10
			}
			raw, err := json.Marshal(request)
			if err != nil {
				t.Fatal(err)
			}
			expected, err := db.Retrieve(string(raw))
			if err != nil {
				t.Fatal(err)
			}
			switch kind {
			case "full_text":
				request["index"] = "ft"
			case "vector":
				request["index"] = "vx"
			default:
				request["text_index"] = "ft"
				request["vector_index"] = "vx"
			}
			raw, err = json.Marshal(request)
			if err != nil {
				t.Fatal(err)
			}
			actual, err := db.Retrieve(string(raw))
			if err != nil || actual != expected {
				t.Fatalf("%s: %s != %s (%v)", kind, actual, expected, err)
			}
		}
	}
	compare()
	if err := db.Begin(); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Query("DELETE FROM docs WHERE id=1"); err != nil {
		t.Fatal(err)
	}
	if err := db.Rollback(); err != nil {
		t.Fatal(err)
	}
	compare()
	db.Close()
	db, err = OpenDev(root)
	if err != nil {
		t.Fatal(err)
	}
	compare()
	image, err := db.Export()
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	db, err = Import(image)
	if err != nil {
		t.Fatal(err)
	}
	compare()
}
