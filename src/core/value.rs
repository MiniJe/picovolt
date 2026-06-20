//! Dynamically-typed column values.
//!
//! PicoVolt is schema-light: a table declares column *names*, and each cell holds
//! one [`Value`]. Encoding into records (with CAS interception for large
//! payloads) lives in [`crate::storage::record`]; this module is the pure data
//! model with no storage dependencies.

use serde::{Deserialize, Serialize};

/// Number of fractional decimal digits in a [`Value::Decimal`].
pub const DECIMAL_SCALE: u32 = 6;
/// `10^DECIMAL_SCALE`: a decimal's mantissa divided by this is its numeric value.
pub const DECIMAL_DEN: i128 = 1_000_000;

/// A single cell value.
///
/// The derived total order is `Null` &lt; `Int` &lt; `Decimal` &lt; `Text` &lt;
/// `Blob` (numeric within `Int`/`Decimal`, lexicographic within `Text`/`Blob`),
/// the ordering used by `ORDER BY`, `MIN`/`MAX`, range comparisons, and the
/// ordered secondary index. Because the variant has a single `i128` field at a
/// fixed scale, the derived `Ord`/`Eq` is exact numeric order and safe as a
/// `BTreeMap` key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Value {
    /// SQL `NULL` / absent value.
    Null,
    /// A 64-bit signed integer (also used for timestamps and auto-increment ids).
    Int(i64),
    /// A fixed-point decimal: the numeric value is `mantissa / 10^DECIMAL_SCALE`.
    /// Currently produced only by `AVG`; it is comparable and displayable but not
    /// yet storable on disk or constructible from a SQL literal.
    Decimal(i128),
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
            Value::Decimal(_) => "decimal",
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
            Value::Decimal(m) => {
                let mag = m.unsigned_abs();
                let sign = if *m < 0 { "-" } else { "" };
                let den = DECIMAL_DEN as u128;
                write!(
                    f,
                    "{sign}{}.{:0w$}",
                    mag / den,
                    mag % den,
                    w = DECIMAL_SCALE as usize
                )
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn decimal_is_totally_ordered() {
        // Cross-variant rank: Null < Int < Decimal < Text < Blob.
        assert!(Value::Null < Value::Int(0));
        assert!(Value::Int(i64::MAX) < Value::Decimal(0));
        assert!(Value::Decimal(i128::MAX) < Value::Text(String::new()));
        assert!(Value::Text("z".into()) < Value::Blob(vec![]));

        // Within Decimal: exact numeric order, negatives below positives.
        assert!(Value::Decimal(1_500_000) < Value::Decimal(1_500_001));
        assert!(Value::Decimal(-1) < Value::Decimal(0));

        // Distinct variants never compare equal, even at the same numeric value.
        assert_ne!(Value::Int(2), Value::Decimal(2_000_000));

        // Usable as a BTreeSet key (total, panic-free), staying in numeric order.
        let set: BTreeSet<Value> = [
            Value::Decimal(2_500_000),
            Value::Decimal(-500_000),
            Value::Decimal(0),
        ]
        .into_iter()
        .collect();
        let ordered: Vec<_> = set.into_iter().collect();
        assert_eq!(
            ordered,
            vec![
                Value::Decimal(-500_000),
                Value::Decimal(0),
                Value::Decimal(2_500_000),
            ]
        );
    }

    #[test]
    fn decimal_display_is_fixed_point() {
        assert_eq!(Value::Decimal(1_500_000).to_string(), "1.500000");
        assert_eq!(Value::Decimal(-1_500_000).to_string(), "-1.500000");
        assert_eq!(Value::Decimal(0).to_string(), "0.000000");
        assert_eq!(Value::Decimal(625_000).to_string(), "0.625000");
        assert_eq!(Value::Decimal(2_000_000).to_string(), "2.000000");
        assert_eq!(Value::Decimal(-1).to_string(), "-0.000001");
        assert_eq!(Value::Decimal(7813).to_string(), "0.007813");
    }
}
