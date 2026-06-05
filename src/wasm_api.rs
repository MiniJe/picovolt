//! WebAssembly / npm bindings (enabled by the `wasm` feature).
//!
//! Exposes an **in-memory** PicoVolt database to JavaScript via `wasm-bindgen`.
//! The filesystem/mmap backends don't work in a browser, so the wasm build uses
//! [`Database::open_memory`]; export with [`Db::export`] to get a `.pvdb` image.
//!
//! Build the npm package with:
//! `wasm-pack build --target web --release -- --features wasm`

use wasm_bindgen::prelude::*;

use crate::core::value::Value;
use crate::{Database, QueryResult};

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

    /// Run one SQL statement. Returns a plain JS object:
    /// `{ columns, rows }` for `SELECT`, `{ mutated: n }` for
    /// `INSERT`/`UPDATE`/`DELETE`, or `{ done: true }` otherwise. Throws the
    /// error message (a string) on failure.
    pub fn query(&mut self, sql: &str) -> Result<JsValue, JsValue> {
        let result = self
            .inner
            .query(sql)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_wasm_bindgen::to_value(&result_to_json(&result))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Export the whole database as a `.pvdb` byte image (a `Uint8Array` in JS).
    pub fn export(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .bake_to_bytes()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

fn result_to_json(result: &QueryResult) -> serde_json::Value {
    match result {
        QueryResult::Rows { columns, rows } => {
            let rows: Vec<Vec<serde_json::Value>> = rows
                .iter()
                .map(|row| row.iter().map(value_to_json).collect())
                .collect();
            serde_json::json!({ "columns": columns, "rows": rows })
        }
        QueryResult::Mutated(n) => serde_json::json!({ "mutated": n }),
        QueryResult::Done => serde_json::json!({ "done": true }),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Int(i) => serde_json::Value::from(*i),
        Value::Text(s) => serde_json::Value::from(s.as_str()),
        Value::Blob(b) => serde_json::Value::from(b.clone()),
    }
}
