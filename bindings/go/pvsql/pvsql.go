// Package pvsql registers PicoVolt as a database/sql driver named "picovolt",
// so it can be used through Go's standard database/sql API:
//
//	import (
//		"database/sql"
//		_ "github.com/MiniJe/picovolt/bindings/go/pvsql"
//	)
//
//	db, _ := sql.Open("picovolt", "memory") // or "dev:./app.pv", "prod:app.pvdb"
//	db.SetMaxOpenConns(1)
//	db.SetMaxIdleConns(1)
//	db.Exec("CREATE TABLE t (id, name)")
//	rows, _ := db.Query("SELECT * FROM t")
//
// Query parameters are supported through `?` placeholders, each substituted as a
// safely-escaped SQL literal. Transactions use the engine's explicit lifecycle.
// Keep the pool at one connection: every driver connection owns a separate
// PicoVolt handle, and development workspaces have a single-writer contract.
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

func (c *conn) Prepare(q string) (driver.Stmt, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	prepared, err := c.db.Prepare(q)
	if err != nil {
		return nil, err
	}
	return &stmt{c: c, prepared: prepared}, nil
}
func (c *conn) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.db.Close()
	return nil
}
func (c *conn) Begin() (driver.Tx, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if err := c.db.Begin(); err != nil {
		return nil, err
	}
	return &tx{c: c}, nil
}

type tx struct {
	c    *conn
	done bool
}

func (t *tx) Commit() error {
	t.c.mu.Lock()
	defer t.c.mu.Unlock()
	if t.done {
		return errors.New("picovolt: transaction is already closed")
	}
	if err := t.c.db.Commit(); err != nil {
		return err
	}
	t.done = true
	return nil
}

func (t *tx) Rollback() error {
	t.c.mu.Lock()
	defer t.c.mu.Unlock()
	if t.done {
		return errors.New("picovolt: transaction is already closed")
	}
	if err := t.c.db.Rollback(); err != nil {
		return err
	}
	t.done = true
	return nil
}

type stmt struct {
	c        *conn
	prepared *picovolt.Stmt
}

func (s *stmt) Close() error {
	s.c.mu.Lock()
	defer s.c.mu.Unlock()
	if s.prepared != nil {
		s.prepared.Close()
		s.prepared = nil
	}
	return nil
}

func (s *stmt) NumInput() int {
	s.c.mu.Lock()
	defer s.c.mu.Unlock()
	if s.prepared == nil {
		return -1
	}
	return s.prepared.ParameterCount()
}

func (s *stmt) run(args []driver.Value) (string, error) {
	s.c.mu.Lock()
	defer s.c.mu.Unlock()
	if s.prepared == nil {
		return "", errors.New("picovolt: prepared statement is closed")
	}
	params := make([]any, len(args))
	for i, arg := range args {
		params[i] = arg
	}
	return s.prepared.Execute(params...)
}

func (s *stmt) Exec(args []driver.Value) (driver.Result, error) {
	out, err := s.run(args)
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
	out, err := s.run(args)
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

func (result) LastInsertId() (int64, error)   { return 0, errors.New("picovolt: no LastInsertId") }
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
