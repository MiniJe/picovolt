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
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

/// Serialize by borrowing the result instead of cloning rows/strings into a
/// second JSON tree. Shared by C and WASM language bindings.
pub(crate) fn result_to_string(result: &QueryResult) -> serde_json::Result<String> {
    struct Cell<'a>(&'a Value);
    impl Serialize for Cell<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            match self.0 {
                Value::Null => s.serialize_none(),
                Value::Int(n) => s.serialize_i64(*n),
                Value::Decimal(_) => s.collect_str(self.0),
                Value::Text(text) => s.serialize_str(text),
                Value::Blob(bytes) => bytes.serialize(s),
            }
        }
    }
    struct Row<'a>(&'a [Value]);
    impl Serialize for Row<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut seq = s.serialize_seq(Some(self.0.len()))?;
            for value in self.0 {
                seq.serialize_element(&Cell(value))?;
            }
            seq.end()
        }
    }
    struct Rows<'a>(&'a [Vec<Value>]);
    impl Serialize for Rows<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut seq = s.serialize_seq(Some(self.0.len()))?;
            for row in self.0 {
                seq.serialize_element(&Row(row))?;
            }
            seq.end()
        }
    }
    struct Output<'a>(&'a QueryResult);
    impl Serialize for Output<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(None)?;
            match self.0 {
                QueryResult::Rows { columns, rows } => {
                    map.serialize_entry("columns", columns)?;
                    map.serialize_entry("rows", &Rows(rows))?;
                }
                QueryResult::Mutated(n) => map.serialize_entry("mutated", n)?,
                QueryResult::Done => map.serialize_entry("done", &true)?,
            }
            map.end()
        }
    }
    serde_json::to_string(&Output(result))
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_json_preserves_every_value_and_result_shape() {
        for result in [
            QueryResult::Done,
            QueryResult::Mutated(123),
            QueryResult::Rows {
                columns: vec!["x".into()],
                rows: vec![vec![
                    Value::Null,
                    Value::Int(i64::MIN),
                    Value::Decimal(i128::MIN),
                    Value::Text("quote\"\\\n\0".into()),
                    Value::Blob(vec![0, 255]),
                ]],
            },
        ] {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&result_to_string(&result).unwrap())
                    .unwrap(),
                result_to_json(&result)
            );
        }
    }
}
