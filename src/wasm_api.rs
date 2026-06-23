//! WebAssembly / npm bindings (enabled by the `wasm` feature).
//!
//! Exposes an **in-memory** PicoVolt database to JavaScript via `wasm-bindgen`.
//! The filesystem/mmap backends don't work in a browser, so the wasm build uses
//! [`Database::open_memory`]; export with [`Db::export`] to get a `.pvdb` image.
//!
//! Build the npm package with:
//! `wasm-pack build --target web --release -- --features wasm`

use wasm_bindgen::prelude::*;

use crate::json::result_to_json;
use crate::Database;

/// An in-memory PicoVolt database usable from JavaScript.
#[wasm_bindgen]
pub struct Db {
    inner: Database,
}

#[wasm_bindgen]
impl Db {
    /// Create a new, empty in-memory database.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Db {
        console_error_panic_hook::set_once();
        Db {
            inner: Database::open_memory(),
        }
    }

    /// Run one SQL statement, optionally binding `?` placeholders to `params` (a
    /// JS array, e.g. `db.query("SELECT * FROM t WHERE id = ?", [1])`). Returns a
    /// **JSON string** (call `JSON.parse` in JS): `{"columns":[...],"rows":[[...]]}`
    /// for `SELECT`, `{"mutated":n}` for `INSERT`/`UPDATE`/`DELETE`, or
    /// `{"done":true}` otherwise. Throws the error message (a string) on failure.
    pub fn query(&mut self, sql: &str, params: JsValue) -> Result<String, JsValue> {
        let result = if params.is_undefined() || params.is_null() {
            self.inner.query(sql)
        } else {
            let arr = js_sys::Array::from(&params);
            let mut values = Vec::with_capacity(arr.length() as usize);
            for item in arr.iter() {
                values.push(js_to_value(&item)?);
            }
            self.inner.query_with(sql, &values)
        }
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&result_to_json(&result))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Export the whole database as a `.pvdb` byte image (a `Uint8Array` in JS).
    pub fn export(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .bake_to_bytes()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Load a database from a `.pvdb` byte image (e.g. one produced by
    /// [`export`](Db::export)). Writable, with full time-travel history intact.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8]) -> Result<Db, JsValue> {
        console_error_panic_hook::set_once();
        Database::import_bytes(bytes)
            .map(|inner| Db { inner })
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// The most recently committed transaction id, the upper bound for a
    /// `... BEFORE tx` time-travel query.
    #[wasm_bindgen(js_name = currentTx)]
    pub fn current_tx(&self) -> u32 {
        self.inner.current_tx() as u32
    }
}

/// Convert a JavaScript parameter into a PicoVolt `Value`: null/undefined to
/// Null, boolean to 0/1, string to Text, an integral number to Int, and a
/// fractional number to a fixed-point decimal.
fn js_to_value(v: &JsValue) -> Result<crate::Value, JsValue> {
    use crate::Value;
    if v.is_null() || v.is_undefined() {
        Ok(Value::Null)
    } else if let Some(b) = v.as_bool() {
        Ok(Value::Int(if b { 1 } else { 0 }))
    } else if let Some(s) = v.as_string() {
        Ok(Value::Text(s))
    } else if let Some(n) = v.as_f64() {
        if n.fract() == 0.0 && n.abs() < 9.007e15 {
            Ok(Value::Int(n as i64))
        } else {
            Ok(Value::Decimal((n * 1_000_000.0).round() as i128))
        }
    } else {
        Err(JsValue::from_str("unsupported SQL parameter type"))
    }
}
