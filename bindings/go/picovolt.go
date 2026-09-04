// Package picovolt provides Go bindings for the PicoVolt embedded database
// engine via its C ABI (cgo).
//
// The module carries the matching C header. Install or download the native
// PicoVolt library separately and make it visible to the linker (for example,
// with CGO_LDFLAGS=-L/path/to/lib) and to the platform's dynamic loader. See the
// package README for release-asset and source-build examples.
//
// A DB handle is not safe for concurrent use; guard it yourself if you share it
// across goroutines.
package picovolt

/*
#cgo CFLAGS: -I${SRCDIR}/include
#cgo LDFLAGS: -lpicovolt
#include <stdlib.h>
#include "picovolt.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

// DB is a handle to a PicoVolt database.
type DB struct {
	ptr *C.PvDb
}

// Stmt is a reusable positional-parameter SQL statement. It retains a native
// prepared handle and the database used for execution. Close the statement
// before closing its DB. A Stmt is not safe for concurrent use, matching its DB.
type Stmt struct {
	db             *DB
	ptr            *C.PvStmt
	parameterCount int
}

// The C ABI records errors in a thread-local that pv_last_error reads back. A
// goroutine can migrate OS threads between the failing call and the error read
// (two separate cgo transitions), which would read the wrong thread's slot. The
// fallible wrappers below pin the goroutine with runtime.LockOSThread for the
// whole call-plus-read window so both transitions hit the same OS thread.

// Version returns the PicoVolt library version, e.g. "1.7.0".
func Version() string {
	return C.GoString(C.pv_version())
}

// lastError reads the thread-local error message recorded by the last call.
func lastError() error {
	msg := C.pv_last_error()
	if msg == nil {
		return errors.New("picovolt: unknown error")
	}
	return errors.New("picovolt: " + C.GoString(msg))
}

// OpenMemory opens a new, empty in-memory database.
func OpenMemory() (*DB, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ptr := C.pv_open_memory()
	if ptr == nil {
		return nil, lastError()
	}
	return &DB{ptr: ptr}, nil
}

// OpenDev opens a development workspace at path.
func OpenDev(path string) (*DB, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	ptr := C.pv_open_dev(cpath)
	if ptr == nil {
		return nil, lastError()
	}
	return &DB{ptr: ptr}, nil
}

// OpenProd opens a baked .pvdb monolith at path (read-only).
func OpenProd(path string) (*DB, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	ptr := C.pv_open_prod(cpath)
	if ptr == nil {
		return nil, lastError()
	}
	return &DB{ptr: ptr}, nil
}

// Import opens a database from a .pvdb byte image (e.g. one from Export).
func Import(image []byte) (*DB, error) {
	if len(image) == 0 {
		return nil, errors.New("picovolt: empty image")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ptr := C.pv_import((*C.uint8_t)(unsafe.Pointer(&image[0])), C.size_t(len(image)))
	if ptr == nil {
		return nil, lastError()
	}
	return &DB{ptr: ptr}, nil
}

// Query runs one SQL statement and returns the result as a JSON string:
//
//	{"columns":[...],"rows":[[...]]} | {"mutated":n} | {"done":true}
func (db *DB) Query(sql string) (string, error) {
	if db.ptr == nil {
		return "", errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	csql := C.CString(sql)
	defer C.free(unsafe.Pointer(csql))
	res := C.pv_query(db.ptr, csql)
	if res == nil {
		return "", lastError()
	}
	defer C.pv_string_free(res)
	return C.GoString(res), nil
}

// QueryParams runs one SQL statement, binding `?` placeholders to a JSON array
// of parameters (e.g. `[1, "alice", null]`). Each is substituted as a
// safely-escaped SQL literal. Returns the JSON result string.
func (db *DB) QueryParams(sql, paramsJSON string) (string, error) {
	if db.ptr == nil {
		return "", errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	csql := C.CString(sql)
	defer C.free(unsafe.Pointer(csql))
	cparams := C.CString(paramsJSON)
	defer C.free(unsafe.Pointer(cparams))
	res := C.pv_query_params(db.ptr, csql, cparams)
	if res == nil {
		return "", lastError()
	}
	defer C.pv_string_free(res)
	return C.GoString(res), nil
}

// Prepare validates and retains a reusable positional-parameter statement.
func (db *DB) Prepare(sql string) (*Stmt, error) {
	if db.ptr == nil {
		return nil, errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	csql := C.CString(sql)
	defer C.free(unsafe.Pointer(csql))
	ptr := C.pv_prepare(db.ptr, csql)
	if ptr == nil {
		return nil, lastError()
	}
	return &Stmt{
		db:             db,
		ptr:            ptr,
		parameterCount: int(C.pv_stmt_parameter_count(ptr)),
	}, nil
}

// ParameterCount returns the exact number of positional values Execute expects.
func (s *Stmt) ParameterCount() int {
	if s == nil {
		return 0
	}
	return s.parameterCount
}

// Execute runs the prepared SQL with one value per positional `?` and returns
// PicoVolt's JSON result string. Blob parameters are not supported.
func (s *Stmt) Execute(params ...any) (string, error) {
	if s == nil || s.ptr == nil {
		return "", errors.New("picovolt: prepared statement is closed")
	}
	if s.db == nil || s.db.ptr == nil {
		return "", errors.New("picovolt: database is closed")
	}
	if len(params) != s.parameterCount {
		return "", fmt.Errorf(
			"picovolt: prepared statement expects %d parameters, got %d",
			s.parameterCount,
			len(params),
		)
	}
	for _, param := range params {
		if _, ok := param.([]byte); ok {
			return "", errors.New("picovolt: []byte (blob) parameters are not supported")
		}
	}
	payload := []byte("[]")
	if len(params) != 0 {
		var err error
		payload, err = json.Marshal(params)
		if err != nil {
			return "", fmt.Errorf("picovolt: encode prepared parameters: %w", err)
		}
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cparams := C.CString(string(payload))
	defer C.free(unsafe.Pointer(cparams))
	result := C.pv_stmt_execute(s.ptr, s.db.ptr, cparams)
	if result == nil {
		return "", lastError()
	}
	defer C.pv_string_free(result)
	return C.GoString(result), nil
}

// Close releases the native statement handle. It is safe to call more than
// once and does not close the database.
func (s *Stmt) Close() {
	if s != nil && s.ptr != nil {
		C.pv_stmt_close(s.ptr)
		s.ptr = nil
		s.db = nil
	}
}

// ImportSQL imports a SQL dump (e.g. `sqlite3 db .dump`). It returns a JSON
// report string `{"executed":n,"skipped":[...],"errors":[...]}`.
func (db *DB) ImportSQL(dump string) (string, error) {
	if db.ptr == nil {
		return "", errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cdump := C.CString(dump)
	defer C.free(unsafe.Pointer(cdump))
	res := C.pv_import_sql(db.ptr, cdump)
	if res == nil {
		return "", lastError()
	}
	defer C.pv_string_free(res)
	return C.GoString(res), nil
}

// CurrentTx returns the most recently committed transaction id (the upper bound
// for a "... BEFORE tx" time-travel query).
func (db *DB) CurrentTx() uint64 {
	if db.ptr == nil {
		return 0
	}
	return uint64(C.pv_current_tx(db.ptr))
}

// Begin starts an explicit multi-statement transaction.
func (db *DB) Begin() error {
	return db.transactionControl(0)
}

// Commit makes the active transaction durable.
func (db *DB) Commit() error {
	return db.transactionControl(1)
}

// Rollback restores the state from before Begin.
func (db *DB) Rollback() error {
	return db.transactionControl(2)
}

// InTransaction reports whether Begin has not yet been committed or rolled back.
func (db *DB) InTransaction() bool {
	if db.ptr == nil {
		return false
	}
	return C.pv_in_transaction(db.ptr) != 0
}

func (db *DB) transactionControl(action int) error {
	if db.ptr == nil {
		return errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var ok C.int32_t
	switch action {
	case 0:
		ok = C.pv_begin_transaction(db.ptr)
	case 1:
		ok = C.pv_commit_transaction(db.ptr)
	default:
		ok = C.pv_rollback_transaction(db.ptr)
	}
	if ok == 0 {
		return lastError()
	}
	return nil
}

// Export returns the database as a .pvdb byte image.
func (db *DB) Export() ([]byte, error) {
	if db.ptr == nil {
		return nil, errors.New("picovolt: database is closed")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var n C.size_t
	buf := C.pv_export(db.ptr, &n)
	if buf == nil {
		return nil, lastError()
	}
	defer C.pv_bytes_free(buf, n)
	const maxCInt = uint64(1<<31 - 1)
	if uint64(n) > maxCInt {
		return nil, errors.New("picovolt: export exceeds the Go binding's 2 GiB copy limit")
	}
	return C.GoBytes(unsafe.Pointer(buf), C.int(n)), nil
}

// Close releases the database. It is safe to call more than once. Idiomatic use
// is `defer db.Close()` right after a successful open.
func (db *DB) Close() {
	if db.ptr != nil {
		if db.InTransaction() {
			_ = db.Rollback()
		}
		C.pv_close(db.ptr)
		db.ptr = nil
	}
}
