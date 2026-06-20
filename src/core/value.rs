//! Dynamically-typed column values.
//!
//! PicoVolt is schema-light: a table declares column *names*, and each cell holds
//! one [`Value`]. Encoding into records (with CAS interception for large
//! payloads) lives in [`crate::storage::record`]; this module is the pure data
//! model with no storage dependencies.

use serde::{Deserialize, Serialize};

/// A single cell value.
///
/// The derived total order is `Null` &lt; `Int` &lt; `Text` &lt; `Blob`, numeric
/// within `Int` and lexicographic within `Text`/`Blob` — the ordering used by
/// `ORDER BY`, `MIN`/`MAX`, range comparisons, and the ordered secondary index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Value {
    /// SQL `NULL` / absent value.
    Null,
    /// A 64-bit signed integer (also used for timestamps and auto-increment ids).
    Int(i64),
    /// UTF-8 text.
    Text(String),
    /// Opaque binary payload.
    Blob(Vec<u8>),
}

/// A logical row: an ordered list of cell values, one per column.
pub type Row = Vec<Value>;

impl Value {
    /// A short, human-readable name for the value's type.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Int(_) => "int",
            Value::Text(_) => "text",
            Value::Blob(_) => "blob",
        }
    }

    /// Returns the integer payload, if this value is an [`Value::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the text payload, if this value is [`Value::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Blob(b) => write!(f, "<blob {} bytes>", b.len()),
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}
