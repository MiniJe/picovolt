//! Query-result JSON serialization, shared by the WebAssembly binding
//! ([`crate::wasm_api`]) and the C ABI ([`crate::ffi`]) so every language binding
//! emits byte-for-byte the same shape:
//!
//! - `SELECT`            -> `{"columns":[...],"rows":[[...]]}`
//! - `INSERT`/`UPDATE`/`DELETE` -> `{"mutated":n}`
//! - everything else     -> `{"done":true}`
//!
//! Values map as: NULL -> `null`, integer -> number, decimal -> its fixed-point
//! text (no exact JSON number form), text -> string, blob -> array of byte values.

use crate::core::value::Value;
use crate::QueryResult;

pub(crate) fn result_to_json(result: &QueryResult) -> serde_json::Value {
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
        // A decimal has no exact JSON number form, so emit its text rendering.
        Value::Decimal(_) => serde_json::Value::from(v.to_string()),
        Value::Text(s) => serde_json::Value::from(s.as_str()),
        Value::Blob(b) => serde_json::Value::from(b.clone()),
    }
}
