//! C ABI (enabled by the `capi` feature).
//!
//! A thin, stable, panic-safe C-callable surface over the in-process engine, so
//! PicoVolt can be embedded from C, Go (cgo), Python (ctypes), and any language
//! with a C FFI. Build a shared library with:
//!
//! ```sh
//! cargo build --release --features capi
//! ```
//!
//! which produces `target/release/{libpicovolt.so | picovolt.dll |
//! libpicovolt.dylib}`. The matching header is [`include/picovolt.h`](https://github.com/MiniJe/picovolt/blob/main/include/picovolt.h).
//!
//! ## Contract
//!
//! - A [`PvDb`] handle is created by `pv_open_*` and must be released with
//!   `pv_close`. It is **not** thread-safe: do not use one handle from multiple
//!   threads without external synchronization.
//! - `pv_query` returns a newly allocated, NUL-terminated JSON string the caller
//!   frees with `pv_string_free`; `pv_export` returns a byte buffer freed with
//!   `pv_bytes_free`. Mixing up the free functions is undefined behavior.
//! - On error, fallible functions return NULL (or `0`) and record a message
//!   retrievable on the same thread with `pv_last_error`.
//! - Panics never cross the FFI boundary: every entry point catches them and
//!   reports them through `pv_last_error`.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::Database;

/// Opaque handle to a PicoVolt database. Allocate with `pv_open_*`, free with
/// `pv_close`.
pub struct PvDb {
    inner: Database,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: impl Into<Vec<u8>>) {
    // Strip any interior NUL bytes so the message is always a valid C string.
    let bytes: Vec<u8> = msg
        .into()
        .into_iter()
        .map(|b| if b == 0 { b'?' } else { b })
        .collect();
    let cstr = CString::new(bytes).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(cstr));
}

fn clear_last_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

/// Run `f`, converting any panic into a recorded error and the `on_panic`
/// sentinel, so a panic never unwinds across the C boundary.
fn guard<T>(on_panic: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            set_last_error("panic in a PicoVolt FFI call");
            on_panic
        }
    }
}

/// Borrow a C string as `&str`, or `None` if it is NULL or not valid UTF-8.
///
/// # Safety
/// `p` must be NULL or a valid pointer to a NUL-terminated string that stays
/// alive for the duration of the call.
unsafe fn cstr_to_str<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// Allocate a NUL-terminated C string owning `s`, or NULL if `s` contained an
/// interior NUL (it never does for our JSON output).
fn string_to_c(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => {
            set_last_error("result contained an interior NUL byte");
            ptr::null_mut()
        }
    }
}

/// The PicoVolt library version, e.g. `"0.4.0"`, as a static NUL-terminated
/// string. Never NULL; do not free.
#[no_mangle]
pub extern "C" fn pv_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// The most recent error message recorded on the calling thread, or NULL if
/// there is none.
///
/// The returned pointer is owned by PicoVolt and remains valid only until the
/// next PicoVolt call on this thread. Copy it if you need to keep it; do not
/// free it.
#[no_mangle]
pub extern "C" fn pv_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Open a new, empty in-memory database. Returns NULL only on allocation
/// failure or panic (see `pv_last_error`).
#[no_mangle]
pub extern "C" fn pv_open_memory() -> *mut PvDb {
    guard(ptr::null_mut(), || {
        clear_last_error();
        Box::into_raw(Box::new(PvDb {
            inner: Database::open_memory(),
        }))
    })
}

/// Open a development workspace at `path` (UTF-8). Returns NULL on error (see
/// `pv_last_error`).
///
/// # Safety
/// `path` must be NULL or a valid pointer to a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pv_open_dev(path: *const c_char) -> *mut PvDb {
    guard(ptr::null_mut(), || {
        clear_last_error();
        let Some(path) = (unsafe { cstr_to_str(path) }) else {
            set_last_error("pv_open_dev: path is NULL or not valid UTF-8");
            return ptr::null_mut();
        };
        match Database::open_dev(path) {
            Ok(inner) => Box::into_raw(Box::new(PvDb { inner })),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Open a baked `.pvdb` production monolith at `path` (UTF-8), read-only.
/// Returns NULL on error (see `pv_last_error`).
///
/// # Safety
/// `path` must be NULL or a valid pointer to a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pv_open_prod(path: *const c_char) -> *mut PvDb {
    guard(ptr::null_mut(), || {
        clear_last_error();
        let Some(path) = (unsafe { cstr_to_str(path) }) else {
            set_last_error("pv_open_prod: path is NULL or not valid UTF-8");
            return ptr::null_mut();
        };
        match Database::open_prod(path) {
            Ok(inner) => Box::into_raw(Box::new(PvDb { inner })),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Run one SQL statement. Returns a newly allocated, NUL-terminated JSON string
/// the caller must free with `pv_string_free`, or NULL on error (see
/// `pv_last_error`). The JSON shape matches the JavaScript binding:
/// `{"columns":[...],"rows":[[...]]}` | `{"mutated":n}` | `{"done":true}`.
///
/// # Safety
/// `db` must be a live handle from `pv_open_*` (not yet closed), and `sql` a
/// valid pointer to a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pv_query(db: *mut PvDb, sql: *const c_char) -> *mut c_char {
    guard(ptr::null_mut(), || {
        clear_last_error();
        let Some(db) = (unsafe { db.as_mut() }) else {
            set_last_error("pv_query: db handle is NULL");
            return ptr::null_mut();
        };
        let Some(sql) = (unsafe { cstr_to_str(sql) }) else {
            set_last_error("pv_query: sql is NULL or not valid UTF-8");
            return ptr::null_mut();
        };
        match db.inner.query(sql) {
            Ok(result) => match serde_json::to_string(&crate::json::result_to_json(&result)) {
                Ok(s) => string_to_c(s),
                Err(e) => {
                    set_last_error(e.to_string());
                    ptr::null_mut()
                }
            },
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// The most recently committed transaction id (the upper bound for a
/// `... BEFORE tx` time-travel query). Returns `0` if `db` is NULL.
///
/// # Safety
/// `db` must be NULL or a live handle from `pv_open_*`.
#[no_mangle]
pub unsafe extern "C" fn pv_current_tx(db: *const PvDb) -> u64 {
    guard(0, || {
        // Clear first, like every other error-recording entry point, so a
        // successful call never leaves a stale message in `pv_last_error`.
        clear_last_error();
        match unsafe { db.as_ref() } {
            Some(db) => db.inner.current_tx(),
            None => {
                set_last_error("pv_current_tx: db handle is NULL");
                0
            }
        }
    })
}

/// Export the whole database as a `.pvdb` byte image. On success returns a
/// buffer of `*out_len` bytes (free it with `pv_bytes_free`) and writes the
/// length through `out_len`; returns NULL on error (see `pv_last_error`).
///
/// # Safety
/// `db` must be a live handle and `out_len` a valid, writable `size_t*`.
#[no_mangle]
pub unsafe extern "C" fn pv_export(db: *mut PvDb, out_len: *mut usize) -> *mut u8 {
    guard(ptr::null_mut(), || {
        clear_last_error();
        if out_len.is_null() {
            set_last_error("pv_export: out_len is NULL");
            return ptr::null_mut();
        }
        let Some(db) = (unsafe { db.as_mut() }) else {
            set_last_error("pv_export: db handle is NULL");
            unsafe { *out_len = 0 };
            return ptr::null_mut();
        };
        match db.inner.bake_to_bytes() {
            Ok(bytes) => {
                let len = bytes.len();
                let mut boxed = bytes.into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                std::mem::forget(boxed);
                unsafe { *out_len = len };
                ptr
            }
            Err(e) => {
                set_last_error(e.to_string());
                unsafe { *out_len = 0 };
                ptr::null_mut()
            }
        }
    })
}

/// Import a database from a `.pvdb` byte image (e.g. one from `pv_export`).
/// Returns NULL on error (see `pv_last_error`).
///
/// # Safety
/// `bytes` must point to at least `len` readable bytes (or be NULL, which is an
/// error).
#[no_mangle]
pub unsafe extern "C" fn pv_import(bytes: *const u8, len: usize) -> *mut PvDb {
    guard(ptr::null_mut(), || {
        clear_last_error();
        if bytes.is_null() {
            set_last_error("pv_import: bytes is NULL");
            return ptr::null_mut();
        }
        let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
        match Database::import_bytes(slice) {
            Ok(inner) => Box::into_raw(Box::new(PvDb { inner })),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Free a string returned by `pv_query`. NULL is ignored.
///
/// # Safety
/// `s` must be NULL or a pointer returned by `pv_query` and not already freed.
#[no_mangle]
pub unsafe extern "C" fn pv_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Free a buffer returned by `pv_export`. `len` must be the same length that
/// `pv_export` reported. NULL is ignored.
///
/// # Safety
/// `ptr`/`len` must be a buffer returned by `pv_export`, with its exact length,
/// not already freed.
#[no_mangle]
pub unsafe extern "C" fn pv_bytes_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    }
}

/// Close a database handle and free its resources. NULL is ignored. Using the
/// handle after this call is undefined behavior.
///
/// # Safety
/// `db` must be NULL or a handle from `pv_open_*` that has not already been
/// closed.
#[no_mangle]
pub unsafe extern "C" fn pv_close(db: *mut PvDb) {
    if !db.is_null() {
        drop(unsafe { Box::from_raw(db) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn run(db: *mut PvDb, sql: &str) -> Option<String> {
        let c = CString::new(sql).unwrap();
        let res = pv_query(db, c.as_ptr());
        if res.is_null() {
            return None;
        }
        let s = CStr::from_ptr(res).to_str().unwrap().to_owned();
        pv_string_free(res);
        Some(s)
    }

    #[test]
    fn roundtrip_memory() {
        unsafe {
            let db = pv_open_memory();
            assert!(!db.is_null());
            assert!(run(db, "CREATE TABLE t (id, name)")
                .unwrap()
                .contains("\"done\":true"));
            assert!(run(db, "INSERT INTO t VALUES (1, 'alice')")
                .unwrap()
                .contains("\"mutated\":1"));
            let sel = run(db, "SELECT * FROM t").unwrap();
            assert!(sel.contains("\"columns\"") && sel.contains("alice"));
            assert!(pv_current_tx(db) > 0);
            pv_close(db);
        }
    }

    #[test]
    fn error_path_sets_last_error() {
        unsafe {
            let db = pv_open_memory();
            clear_last_error();
            let bad = pv_query(db, CString::new("SELECT FROM").unwrap().as_ptr());
            assert!(bad.is_null());
            let err = pv_last_error();
            assert!(!err.is_null());
            assert!(!CStr::from_ptr(err).to_bytes().is_empty());
            pv_close(db);
        }
    }

    #[test]
    fn export_import_roundtrip() {
        unsafe {
            let db = pv_open_memory();
            run(db, "CREATE TABLE t (id)").unwrap();
            run(db, "INSERT INTO t VALUES (42)").unwrap();
            let mut len: usize = 0;
            let bytes = pv_export(db, &mut len as *mut usize);
            assert!(!bytes.is_null() && len > 0);
            let db2 = pv_import(bytes, len);
            assert!(!db2.is_null());
            assert!(run(db2, "SELECT * FROM t").unwrap().contains("42"));
            pv_bytes_free(bytes, len);
            pv_close(db);
            pv_close(db2);
        }
    }

    #[test]
    fn null_arguments_are_safe() {
        unsafe {
            let sql = CString::new("SELECT 1").unwrap();
            assert!(pv_query(ptr::null_mut(), sql.as_ptr()).is_null());
            assert_eq!(pv_current_tx(ptr::null()), 0);
            pv_close(ptr::null_mut());
            pv_string_free(ptr::null_mut());
            pv_bytes_free(ptr::null_mut(), 0);
            let v = CStr::from_ptr(pv_version()).to_str().unwrap();
            assert!(v.chars().next().unwrap().is_ascii_digit());
        }
    }
}
