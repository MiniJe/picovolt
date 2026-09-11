// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
package picovolt

/*
#cgo CFLAGS: -I${SRCDIR}/include
#cgo LDFLAGS: -lpicovolt
#include <stdlib.h>
#include "picovolt.h"
*/
import "C"

import (
	"errors"
	"runtime"
	"strings"
	"unsafe"
)

// Vault owns an exclusive native encrypted-file handle. It is not thread-safe.
// Writes use Request batch actions; successful batches are persisted atomically.
type Vault struct{ ptr *C.PvVault }

// OpenVault opens or creates a native 2.2 encrypted vault. The caller owns the
// secret slice and its lifetime; C-owned copies are zeroized on drop.
// password=false requires a raw 32-byte key; true requires 12..1024 exact bytes.
func OpenVault(path string, secret []byte, password, create bool) (*Vault, error) {
	if strings.ContainsRune(path, 0) {
		return nil, errors.New("picovolt: path contains NUL")
	}
	if (!password && len(secret) != 32) || (password && (len(secret) < 12 || len(secret) > 1024)) {
		return nil, errors.New("picovolt: invalid secret length")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var mode, makeNew C.int32_t
	if password {
		mode = 1
	}
	if create {
		makeNew = 1
	}
	ptr := C.pv_vault_open(cPath, (*C.uint8_t)(unsafe.Pointer(&secret[0])), C.size_t(len(secret)), mode, makeNew)
	runtime.KeepAlive(secret)
	if ptr == nil {
		return nil, lastError()
	}
	return &Vault{ptr: ptr}, nil
}

// Request accepts the documented JSON query/batch/backup/inspect/retrieve actions.
func (v *Vault) Request(request string) (string, error) {
	if v.ptr == nil {
		return "", errors.New("picovolt: vault is closed")
	}
	if len(request) > 1024*1024 || strings.ContainsRune(request, 0) {
		return "", errors.New("picovolt: invalid vault request length or NUL")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	input := C.CString(request)
	defer C.free(unsafe.Pointer(input))
	output := C.pv_vault_request(v.ptr, input)
	if output == nil {
		return "", lastError()
	}
	defer C.pv_string_free(output)
	return C.GoString(output), nil
}

// RotateKey re-encrypts the current vault only. Existing backups retain old keys.
func (v *Vault) RotateKey(secret []byte, password bool) error {
	if v.ptr == nil {
		return errors.New("picovolt: vault is closed")
	}
	if (!password && len(secret) != 32) || (password && (len(secret) < 12 || len(secret) > 1024)) {
		return errors.New("picovolt: invalid secret length")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var mode C.int32_t
	if password {
		mode = 1
	}
	result := C.pv_vault_rotate(v.ptr, (*C.uint8_t)(unsafe.Pointer(&secret[0])), C.size_t(len(secret)), mode)
	runtime.KeepAlive(secret)
	if result == 0 {
		return lastError()
	}
	return nil
}
func (v *Vault) Close() {
	if v.ptr != nil {
		C.pv_vault_close(v.ptr)
		v.ptr = nil
	}
}
