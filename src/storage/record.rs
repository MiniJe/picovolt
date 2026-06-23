//! Row <-> record-body serialization, with CAS interception (spec §4.A).
//!
//! A *record* is a [`RecordEnvelope`] (24 bytes, MVCC bookkeeping) followed by a
//! *body*: a field count and one tagged field per column. Text/blob payloads
//! larger than [`crate::core::types::CAS_INLINE_THRESHOLD`] are replaced by an
//! 8-byte CAS pointer; smaller ones are stored inline.

use crate::core::errors::{PvError, Result};
use crate::core::types::RecordEnvelope;
use crate::core::value::{Row, Value};
use crate::storage::cas::CasStore;

const TAG_NULL: u8 = 0x00;
const TAG_INT: u8 = 0x01;
const TAG_INLINE_TEXT: u8 = 0x02;
const TAG_INLINE_BLOB: u8 = 0x03;
const TAG_CAS_TEXT: u8 = 0x04;
const TAG_CAS_BLOB: u8 = 0x05;
const TAG_DECIMAL: u8 = 0x06;

/// Encode a row body (no envelope), interning oversized payloads into `cas`.
pub fn encode_row(values: &[Value], cas: &mut CasStore) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let count: u16 = values
        .len()
        .try_into()
        .map_err(|_| PvError::Schema("too many columns (max 65535)".into()))?;
    out.extend_from_slice(&count.to_le_bytes());
    for v in values {
        match v {
            Value::Null => out.push(TAG_NULL),
            Value::Int(i) => {
                out.push(TAG_INT);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Text(s) => {
                encode_bytes(&mut out, s.as_bytes(), TAG_INLINE_TEXT, TAG_CAS_TEXT, cas)?
            }
            Value::Blob(b) => encode_bytes(&mut out, b, TAG_INLINE_BLOB, TAG_CAS_BLOB, cas)?,
            Value::Decimal(m) => {
                out.push(TAG_DECIMAL);
                out.extend_from_slice(&m.to_le_bytes());
            }
        }
    }
    Ok(out)
}

fn encode_bytes(
    out: &mut Vec<u8>,
    bytes: &[u8],
    inline_tag: u8,
    cas_tag: u8,
    cas: &mut CasStore,
) -> Result<()> {
    if CasStore::should_intern(bytes.len()) {
        let id = cas.put(bytes)?;
        out.push(cas_tag);
        out.extend_from_slice(&id.to_le_bytes());
    } else {
        out.push(inline_tag);
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(())
}

/// Decode a row body produced by [`encode_row`], resolving CAS pointers via `cas`.
pub fn decode_row(body: &[u8], cas: &CasStore) -> Result<Row> {
    let mut pos = 0usize;
    let count = read_u16(body, &mut pos)? as usize;
    let mut row = Row::with_capacity(count);
    for _ in 0..count {
        let tag = *body
            .get(pos)
            .ok_or_else(|| PvError::Corruption("record: truncated at field tag".into()))?;
        pos += 1;
        let value = match tag {
            TAG_NULL => Value::Null,
            TAG_INT => Value::Int(read_i64(body, &mut pos)?),
            TAG_DECIMAL => Value::Decimal(read_i128(body, &mut pos)?),
            TAG_INLINE_TEXT => {
                let bytes = read_inline(body, &mut pos)?;
                Value::Text(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| PvError::Corruption("record: invalid utf-8 text".into()))?,
                )
            }
            TAG_INLINE_BLOB => Value::Blob(read_inline(body, &mut pos)?.to_vec()),
            TAG_CAS_TEXT => {
                let id = read_u64(body, &mut pos)?;
                let bytes = cas.get(id)?;
                Value::Text(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| PvError::Corruption("record: invalid utf-8 in CAS".into()))?,
                )
            }
            TAG_CAS_BLOB => {
                let id = read_u64(body, &mut pos)?;
                Value::Blob(cas.get(id)?.to_vec())
            }
            other => {
                return Err(PvError::Corruption(format!(
                    "record: bad field tag 0x{other:02X}"
                )))
            }
        };
        row.push(value);
    }
    Ok(row)
}

/// Encode a full record: envelope bytes followed by the row body.
pub fn encode_record(
    envelope: &RecordEnvelope,
    values: &[Value],
    cas: &mut CasStore,
) -> Result<Vec<u8>> {
    let mut out = envelope.encode().to_vec();
    out.extend_from_slice(&encode_row(values, cas)?);
    Ok(out)
}

/// Decode a full record into its envelope and row.
pub fn decode_record(bytes: &[u8], cas: &CasStore) -> Result<(RecordEnvelope, Row)> {
    let envelope = RecordEnvelope::decode(bytes)?;
    let row = decode_row(&bytes[RecordEnvelope::ENCODED_LEN..], cas)?;
    Ok((envelope, row))
}

fn read_inline<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let len = read_u16(buf, pos)? as usize;
    let slice = buf
        .get(*pos..*pos + len)
        .ok_or_else(|| PvError::Corruption("record: truncated inline payload".into()))?;
    *pos += len;
    Ok(slice)
}

fn read_u16(buf: &[u8], pos: &mut usize) -> Result<u16> {
    let slice = buf
        .get(*pos..*pos + 2)
        .ok_or_else(|| PvError::Corruption("record: truncated u16".into()))?;
    *pos += 2;
    Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let slice = buf
        .get(*pos..*pos + 8)
        .ok_or_else(|| PvError::Corruption("record: truncated u64".into()))?;
    *pos += 8;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn read_i64(buf: &[u8], pos: &mut usize) -> Result<i64> {
    Ok(read_u64(buf, pos)? as i64)
}

fn read_i128(buf: &[u8], pos: &mut usize) -> Result<i128> {
    let slice = buf
        .get(*pos..*pos + 16)
        .ok_or_else(|| PvError::Corruption("record: truncated i128".into()))?;
    *pos += 16;
    Ok(i128::from_le_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_stay_inline() {
        let mut cas = CasStore::new_memory();
        let row = vec![Value::Int(7), Value::Text("short".into()), Value::Null];
        let body = encode_row(&row, &mut cas).unwrap();
        assert!(cas.is_empty(), "small text must not hit CAS");
        assert_eq!(decode_row(&body, &cas).unwrap(), row);
    }

    #[test]
    fn large_text_is_interned_and_resolved() {
        let mut cas = CasStore::new_memory();
        let big = "x".repeat(64);
        let row = vec![Value::Int(1), Value::Text(big.clone())];
        let body = encode_row(&row, &mut cas).unwrap();
        assert_eq!(cas.len(), 1, "oversized text must hit CAS exactly once");
        assert_eq!(decode_row(&body, &cas).unwrap(), row);
    }

    #[test]
    fn full_record_round_trips_with_envelope() {
        let mut cas = CasStore::new_memory();
        let env = RecordEnvelope::new(5, 0);
        let row = vec![Value::Int(-42), Value::Blob(vec![9u8; 40])];
        let bytes = encode_record(&env, &row, &mut cas).unwrap();
        let (got_env, got_row) = decode_record(&bytes, &cas).unwrap();
        assert_eq!(got_env, env);
        assert_eq!(got_row, row);
    }
}
