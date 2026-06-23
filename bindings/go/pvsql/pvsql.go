// Package pvsql registers PicoVolt as a database/sql driver named "picovolt",
// so it can be used through Go's standard database/sql API:
//
//	import (
//		"database/sql"
//		_ "github.com/MiniJe/picovolt/bindings/go/pvsql"
//	)
//
//	db, _ := sql.Open("picovolt", "memory")        // or "dev:./app.pv", "prod:app.pvdb"
//	db.Exec("CREATE TABLE t (id, name)")
//	rows, _ := db.Query("SELECT * FROM t")
//
// PicoVolt has no bound parameters, so build the SQL string yourself; passing
// query arguments returns an error. Transactions are not supported.
package pvsql

import (
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"io"
	"strings"
	"sync"

	picovolt "github.com/MiniJe/picovolt/bindings/go"
)

func init() { sql.Register("picovolt", drv{}) }

var errNoArgs = errors.New("picovolt: query parameters are not supported; build the SQL string yourself")

type drv struct{}

// Open accepts "memory" (or ""), "dev:<path>", or "prod:<path>". A bare path is
// treated as a development workspace.
func (drv) Open(name string) (driver.Conn, error) {
	var db *picovolt.DB
	var err error
	switch {
	case name == "" || name == "memory" || name == ":memory:":
		db, err = picovolt.OpenMemory()
	case strings.HasPrefix(name, "dev:"):
		db, err = picovolt.OpenDev(name[len("dev:"):])
	case strings.HasPrefix(name, "prod:"):
		db, err = picovolt.OpenProd(name[len("prod:"):])
	default:
		db, err = picovolt.OpenDev(name)
	}
	if err != nil {
		return nil, err
	}
	return &conn{db: db}, nil
}

type conn struct {
	mu sync.Mutex
	db *picovolt.DB
}

func (c *conn) Prepare(q string) (driver.Stmt, error) { return &stmt{c: c, q: q}, nil }
func (c *conn) Close() error                          { c.db.Close(); return nil }
func (c *conn) Begin() (driver.Tx, error)             { return nil, errors.New("picovolt: transactions are not supported") }

func (c *conn) run(q string) (string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.db.Query(q)
}

type stmt struct {
	c *conn
	q string
}

func (s *stmt) Close() error  { return nil }
func (s *stmt) NumInput() int { return 0 }

func (s *stmt) Exec(args []driver.Value) (driver.Result, error) {
	if len(args) > 0 {
		return nil, errNoArgs
	}
	out, err := s.c.run(s.q)
	if err != nil {
		return nil, err
	}
	var r struct {
		Mutated *int64 `json:"mutated"`
	}
	_ = json.Unmarshal([]byte(out), &r)
	var n int64
	if r.Mutated != nil {
		n = *r.Mutated
	}
	return result{n: n}, nil
}

func (s *stmt) Query(args []driver.Value) (driver.Rows, error) {
	if len(args) > 0 {
		return nil, errNoArgs
	}
	out, err := s.c.run(s.q)
	if err != nil {
		return nil, err
	}
	var r struct {
		Columns []string            `json:"columns"`
		Rows    [][]json.RawMessage `json:"rows"`
	}
	if err := json.Unmarshal([]byte(out), &r); err != nil {
		return nil, err
	}
	return &rows{cols: r.Columns, data: r.Rows}, nil
}

type result struct{ n int64 }

func (result) LastInsertId() (int64, error) { return 0, errors.New("picovolt: no LastInsertId") }
func (r result) RowsAffected() (int64, error) { return r.n, nil }

type rows struct {
	cols []string
	data [][]json.RawMessage
	i    int
}

func (r *rows) Columns() []string { return r.cols }
func (r *rows) Close() error      { return nil }

func (r *rows) Next(dest []driver.Value) error {
	if r.i >= len(r.data) {
		return io.EOF
	}
	row := r.data[r.i]
	r.i++
	for j := range dest {
		if j < len(row) {
			dest[j] = decodeValue(row[j])
		} else {
			dest[j] = nil
		}
	}
	return nil
}

// decodeValue maps a PicoVolt JSON value to a database/sql value: null -> nil,
// number -> int64, string (text or decimal) -> string, byte array -> []byte.
func decodeValue(raw json.RawMessage) driver.Value {
	s := strings.TrimSpace(string(raw))
	if s == "" || s == "null" {
		return nil
	}
	switch s[0] {
	case '"':
		var str string
		_ = json.Unmarshal(raw, &str)
		return str
	case '[':
		var nums []int
		_ = json.Unmarshal(raw, &nums)
		b := make([]byte, len(nums))
		for i, v := range nums {
			b[i] = byte(v)
		}
		return b
	default:
		var n int64
		if err := json.Unmarshal(raw, &n); err == nil {
			return n
		}
		var f float64
		_ = json.Unmarshal(raw, &f)
		return f
	}
}
