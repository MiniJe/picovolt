// Package picovolt provides Go bindings for the PicoVolt embedded database
// engine via its C ABI (cgo).
//
// Build the shared library from the repository root first:
//
//	cargo build --release --features capi
//
// The cgo directives below point at ../../include for the header and
// ../../target/release for the library, so `go build`/`go test` work from this
// directory after that. At run time the dynamic loader must also find the
// library (see the package README for LD_LIBRARY_PATH / PATH / install notes).
//
// A DB handle is not safe for concurrent use; guard it yourself if you share it
// across goroutines.
package picovolt

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -lpicovolt
#include <stdlib.h>
#include "picovolt.h"
*/
import "C"

import (
	"errors"
	"runtime"
	"unsafe"
)

// DB is a handle to a PicoVolt database.
type DB struct {
	ptr *C.PvDb
}

// The C ABI records errors in a thread-local that pv_last_error reads back. A
// goroutine can migrate OS threads between the failing call and the error read
// (two separate cgo transitions), which would read the wrong thread's slot. The
// fallible wrappers below pin the goroutine with runtime.LockOSThread for the
// whole call-plus-read window so both transitions hit the same OS thread.

// Version returns the PicoVolt library version, e.g. "0.4.0".
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

// CurrentTx returns the most recently committed transaction id (the upper bound
// for a "... BEFORE tx" time-travel query).
func (db *DB) CurrentTx() uint64 {
	if db.ptr == nil {
		return 0
	}
	return uint64(C.pv_current_tx(db.ptr))
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
	return C.GoBytes(unsafe.Pointer(buf), C.int(n)), nil
}

// Close releases the database. It is safe to call more than once. Idiomatic use
// is `defer db.Close()` right after a successful open.
func (db *DB) Close() {
	if db.ptr != nil {
		C.pv_close(db.ptr)
		db.ptr = nil
	}
}
