//! Physical page structures (spec §3): the hot slotted **row** page and the
//! cold packed **columnar** page.
//!
//! A [`RowPage`] wraps a raw 4096-byte buffer and provides O(1) slotted appends:
//! the slot array grows downward from just past the header while record payloads
//! grow upward from the end of the page. [`ColumnarPage`] performs the cold-state
//! transposition — decoding a set of rows column-by-column and applying the
//! §4 compression primitives.

use crate::core::errors::{PvError, Result};
use crate::core::types::{ColumnarPageHeader, RowPageHeader, PAGE_HEADER_SIZE, PAGE_SIZE};
use crate::core::value::{Row, Value};
use crate::storage::compress::{delta_z_decode, delta_z_encode, DictionaryColumn};

/// Bytes per slot-array entry: `offset(u16) + len(u16)`.
pub const SLOT_SIZE: usize = 4;

/// A mutable, fixed-size slotted row page.
pub struct RowPage {
    buf: Box<[u8; PAGE_SIZE]>,
}

impl RowPage {
    /// A new, empty row page with the given id.
    pub fn new(page_id: u64) -> Self {
        let mut buf = Box::new([0u8; PAGE_SIZE]);
        buf[..PAGE_HEADER_SIZE].copy_from_slice(&RowPageHeader::new(page_id).encode());
        Self { buf }
    }

    /// Adopt an existing 4096-byte buffer, validating that it is a row page.
    pub fn from_bytes(buf: Box<[u8; PAGE_SIZE]>) -> Result<Self> {
        RowPageHeader::decode(&buf[..])?; // validates type discriminant
        Ok(Self { buf })
    }

    /// The raw page bytes.
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.buf
    }

    /// Consume the page, returning the raw buffer (e.g. to hand to the VLE).
    pub fn into_bytes(self) -> Box<[u8; PAGE_SIZE]> {
        self.buf
    }

    /// Decode the page header.
    pub fn header(&self) -> RowPageHeader {
        RowPageHeader::decode(&self.buf[..]).expect("row page header was validated on construction")
    }

    fn set_header(&mut self, header: &RowPageHeader) {
        self.buf[..PAGE_HEADER_SIZE].copy_from_slice(&header.encode());
    }

    /// Number of slots currently in use.
    pub fn slot_count(&self) -> u16 {
        self.header().slot_count
    }

    /// Bytes available for the next insert (record payload + one slot entry must
    /// fit within this).
    pub fn free_space(&self) -> usize {
        let header = self.header();
        let slot_array_end = PAGE_HEADER_SIZE + header.slot_count as usize * SLOT_SIZE;
        header.free_space_ptr as usize - slot_array_end
    }

    /// Append a record payload, returning its slot index. Errors with
    /// [`PvError::PageFull`] if it does not fit.
    pub fn insert(&mut self, record: &[u8]) -> Result<u16> {
        let mut header = self.header();
        let slot_array_end = PAGE_HEADER_SIZE + header.slot_count as usize * SLOT_SIZE;
        let free_ptr = header.free_space_ptr as usize;
        let need = record.len() + SLOT_SIZE;
        let available = free_ptr - slot_array_end;
        if need > available {
            return Err(PvError::PageFull {
                needed: need,
                available,
            });
        }
        let new_offset = free_ptr - record.len();
        self.buf[new_offset..free_ptr].copy_from_slice(record);

        let slot_pos = slot_array_end;
        self.buf[slot_pos..slot_pos + 2].copy_from_slice(&(new_offset as u16).to_le_bytes());
        self.buf[slot_pos + 2..slot_pos + 4].copy_from_slice(&(record.len() as u16).to_le_bytes());

        let slot_index = header.slot_count;
        header.slot_count += 1;
        header.free_space_ptr = new_offset as u16;
        self.set_header(&header);
        Ok(slot_index)
    }

    fn slot(&self, index: u16) -> Result<(usize, usize)> {
        if index >= self.slot_count() {
            return Err(PvError::OutOfBounds {
                offset: index as usize,
                size: self.slot_count() as usize,
            });
        }
        let slot_pos = PAGE_HEADER_SIZE + index as usize * SLOT_SIZE;
        let offset =
            u16::from_le_bytes(self.buf[slot_pos..slot_pos + 2].try_into().unwrap()) as usize;
        let len =
            u16::from_le_bytes(self.buf[slot_pos + 2..slot_pos + 4].try_into().unwrap()) as usize;
        Ok((offset, len))
    }

    /// Borrow the record payload stored in slot `index`.
    pub fn record(&self, index: u16) -> Result<&[u8]> {
        let (offset, len) = self.slot(index)?;
        Ok(&self.buf[offset..offset + len])
    }

    /// Overwrite the `tx_deleted` field (bytes 8..16) of the envelope in slot
    /// `index`. Used by the MVCC layer to tombstone a version in place.
    pub fn patch_envelope_deleted(&mut self, index: u16, tx_deleted: u64) -> Result<()> {
        let (offset, _len) = self.slot(index)?;
        self.buf[offset + 8..offset + 16].copy_from_slice(&tx_deleted.to_le_bytes());
        Ok(())
    }

    /// Iterate `(slot_index, record_bytes)` for every slot.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &[u8])> {
        (0..self.slot_count()).map(move |i| (i, self.record(i).expect("valid slot index")))
    }
}

// ---------------------------------------------------------------------------
// Columnar (cold) page: transposition + compression
// ---------------------------------------------------------------------------

const COL_ENC_DELTA_Z: u8 = 1;
const COL_ENC_DICTIONARY: u8 = 2;
const COL_ENC_RAW: u8 = 3;

/// Cold columnar page codec. Operates on fully-resolved [`Row`]s (CAS pointers
/// already dereferenced) — see the module note in the README about CAS-in-cold
/// pages being a future refinement.
pub struct ColumnarPage;

impl ColumnarPage {
    /// Transpose `rows` into the columnar byte layout (header + per-column blocks).
    ///
    /// All rows must share the same arity. Integer columns use Delta-Z; text
    /// columns of low cardinality use dictionary bit-packing; anything else falls
    /// back to a raw tagged encoding.
    pub fn from_rows(page_id: u64, rows: &[Row]) -> Result<Vec<u8>> {
        let arity = rows.first().map(|r| r.len()).unwrap_or(0);
        if rows.iter().any(|r| r.len() != arity) {
            return Err(PvError::Schema(
                "columnar transposition requires uniform row arity".into(),
            ));
        }
        let row_count: u16 = rows
            .len()
            .try_into()
            .map_err(|_| PvError::Schema("too many rows for one columnar page".into()))?;

        let mut out = ColumnarPageHeader { page_id, row_count }.encode().to_vec();
        out.extend_from_slice(&(arity as u16).to_le_bytes());

        for c in 0..arity {
            let column: Vec<&Value> = rows.iter().map(|r| &r[c]).collect();
            let (tag, payload) = encode_column(&column);
            out.push(tag);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&payload);
        }
        Ok(out)
    }

    /// Inverse of [`from_rows`]: recover the header and row set.
    pub fn to_rows(bytes: &[u8]) -> Result<(ColumnarPageHeader, Vec<Row>)> {
        let header = ColumnarPageHeader::decode(bytes)?;
        let mut pos = PAGE_HEADER_SIZE;
        let arity = read_u16(bytes, &mut pos)? as usize;
        let row_count = header.row_count as usize;

        let mut columns: Vec<Vec<Value>> = Vec::with_capacity(arity);
        for _ in 0..arity {
            let tag = *bytes
                .get(pos)
                .ok_or_else(|| PvError::Corruption("columnar: truncated column tag".into()))?;
            pos += 1;
            let len = read_u32(bytes, &mut pos)? as usize;
            let payload = bytes
                .get(pos..pos + len)
                .ok_or_else(|| PvError::Corruption("columnar: truncated column payload".into()))?;
            pos += len;
            let column = decode_column(tag, payload, row_count)?;
            columns.push(column);
        }

        let mut rows = vec![Row::with_capacity(arity); row_count];
        for column in columns {
            for (r, value) in column.into_iter().enumerate() {
                rows[r].push(value);
            }
        }
        Ok((header, rows))
    }

    /// Pad a serialized columnar page to a full [`PAGE_SIZE`] buffer, or error if
    /// it does not fit.
    pub fn pad_to_page(bytes: &[u8]) -> Result<Box<[u8; PAGE_SIZE]>> {
        if bytes.len() > PAGE_SIZE {
            return Err(PvError::PageFull {
                needed: bytes.len(),
                available: PAGE_SIZE,
            });
        }
        let mut page = Box::new([0u8; PAGE_SIZE]);
        page[..bytes.len()].copy_from_slice(bytes);
        Ok(page)
    }
}

fn encode_column(values: &[&Value]) -> (u8, Vec<u8>) {
    // Delta-Z if the whole column is integers.
    if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Int(_))) {
        let ints: Vec<i64> = values.iter().map(|v| v.as_int().unwrap()).collect();
        return (COL_ENC_DELTA_Z, delta_z_encode(&ints));
    }
    // Dictionary if the whole column is low-cardinality text.
    if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Text(_))) {
        let texts: Vec<String> = values
            .iter()
            .map(|v| v.as_text().unwrap().to_owned())
            .collect();
        if let Some(dict) = DictionaryColumn::build(&texts) {
            return (COL_ENC_DICTIONARY, serialize_dict(&dict));
        }
    }
    // Fallback: raw tagged values.
    (COL_ENC_RAW, encode_raw_column(values))
}

fn decode_column(tag: u8, payload: &[u8], row_count: usize) -> Result<Vec<Value>> {
    let column = match tag {
        COL_ENC_DELTA_Z => delta_z_decode(payload)?
            .into_iter()
            .map(Value::Int)
            .collect(),
        COL_ENC_DICTIONARY => deserialize_dict(payload)?
            .decode()?
            .into_iter()
            .map(Value::Text)
            .collect(),
        COL_ENC_RAW => decode_raw_column(payload)?,
        other => {
            return Err(PvError::Corruption(format!(
                "columnar: unknown column encoding 0x{other:02X}"
            )))
        }
    };
    if column.len() != row_count {
        return Err(PvError::Corruption(format!(
            "columnar: column length {} != row count {row_count}",
            column.len()
        )));
    }
    Ok(column)
}

fn encode_raw_column(values: &[&Value]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for v in values {
        match v {
            Value::Null => out.push(0),
            Value::Int(i) => {
                out.push(1);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Text(s) => {
                out.push(2);
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            Value::Blob(b) => {
                out.push(3);
                out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                out.extend_from_slice(b);
            }
        }
    }
    out
}

fn decode_raw_column(payload: &[u8]) -> Result<Vec<Value>> {
    let mut pos = 0usize;
    let count = read_u32(payload, &mut pos)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = *payload
            .get(pos)
            .ok_or_else(|| PvError::Corruption("columnar raw: truncated tag".into()))?;
        pos += 1;
        let value = match tag {
            0 => Value::Null,
            1 => Value::Int(read_i64(payload, &mut pos)?),
            2 => {
                let len = read_u32(payload, &mut pos)? as usize;
                let bytes = take(payload, &mut pos, len)?;
                Value::Text(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| PvError::Corruption("columnar raw: bad utf-8".into()))?,
                )
            }
            3 => {
                let len = read_u32(payload, &mut pos)? as usize;
                Value::Blob(take(payload, &mut pos, len)?.to_vec())
            }
            other => {
                return Err(PvError::Corruption(format!(
                    "columnar raw: bad value tag 0x{other:02X}"
                )))
            }
        };
        out.push(value);
    }
    Ok(out)
}

fn serialize_dict(dict: &DictionaryColumn) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(dict.symbols.len() as u16).to_le_bytes());
    for s in &dict.symbols {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out.push(dict.bits);
    out.extend_from_slice(&(dict.count as u32).to_le_bytes());
    out.extend_from_slice(&(dict.codes.len() as u32).to_le_bytes());
    out.extend_from_slice(&dict.codes);
    out
}

fn deserialize_dict(payload: &[u8]) -> Result<DictionaryColumn> {
    let mut pos = 0usize;
    let symbol_count = read_u16(payload, &mut pos)? as usize;
    let mut symbols = Vec::with_capacity(symbol_count);
    for _ in 0..symbol_count {
        let len = read_u32(payload, &mut pos)? as usize;
        let bytes = take(payload, &mut pos, len)?;
        symbols.push(
            String::from_utf8(bytes.to_vec())
                .map_err(|_| PvError::Corruption("dictionary: bad utf-8 symbol".into()))?,
        );
    }
    let bits = *payload
        .get(pos)
        .ok_or_else(|| PvError::Corruption("dictionary: missing bit width".into()))?;
    pos += 1;
    let count = read_u32(payload, &mut pos)? as usize;
    let codes_len = read_u32(payload, &mut pos)? as usize;
    let codes = take(payload, &mut pos, codes_len)?.to_vec();
    Ok(DictionaryColumn {
        symbols,
        bits,
        count,
        codes,
    })
}

// --- little local readers (bounds-checked) ---------------------------------

fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    let slice = buf
        .get(*pos..*pos + len)
        .ok_or_else(|| PvError::Corruption("columnar: unexpected end of buffer".into()))?;
    *pos += len;
    Ok(slice)
}

fn read_u16(buf: &[u8], pos: &mut usize) -> Result<u16> {
    Ok(u16::from_le_bytes(take(buf, pos, 2)?.try_into().unwrap()))
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    Ok(u32::from_le_bytes(take(buf, pos, 4)?.try_into().unwrap()))
}

fn read_i64(buf: &[u8], pos: &mut usize) -> Result<i64> {
    Ok(i64::from_le_bytes(take(buf, pos, 8)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_page_inserts_and_reads_back() {
        let mut page = RowPage::new(1);
        let s0 = page.insert(b"hello").unwrap();
        let s1 = page.insert(b"world!!").unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(page.record(s0).unwrap(), b"hello");
        assert_eq!(page.record(s1).unwrap(), b"world!!");
        assert_eq!(page.slot_count(), 2);
    }

    #[test]
    fn row_page_survives_buffer_round_trip() {
        let mut page = RowPage::new(7);
        page.insert(b"persistent record").unwrap();
        let bytes = page.into_bytes();
        let reopened = RowPage::from_bytes(bytes).unwrap();
        assert_eq!(reopened.header().page_id, 7);
        assert_eq!(reopened.record(0).unwrap(), b"persistent record");
    }

    #[test]
    fn row_page_reports_full() {
        let mut page = RowPage::new(1);
        let big = vec![0u8; PAGE_SIZE]; // cannot possibly fit with header + slot
        assert!(matches!(page.insert(&big), Err(PvError::PageFull { .. })));
    }

    #[test]
    fn envelope_patch_targets_correct_bytes() {
        let mut page = RowPage::new(1);
        // 24-byte envelope (zeroed tx_deleted) + a marker byte.
        let mut record = vec![0u8; 25];
        record[24] = 0xAB;
        let slot = page.insert(&record).unwrap();
        page.patch_envelope_deleted(slot, 0x99).unwrap();
        let stored = page.record(slot).unwrap();
        assert_eq!(stored[8], 0x99); // tx_deleted low byte
        assert_eq!(stored[24], 0xAB); // body untouched
    }

    #[test]
    fn columnar_round_trips_mixed_columns() {
        // Column 0: monotonic ints (delta-z). Column 1: low-card text (dict).
        // Column 2: mixed (raw fallback).
        let rows: Vec<Row> = (0..8)
            .map(|i| {
                vec![
                    Value::Int(1000 + i),
                    Value::Text(if i % 2 == 0 { "Active" } else { "Pending" }.into()),
                    if i == 3 { Value::Null } else { Value::Int(i) },
                ]
            })
            .collect();
        let bytes = ColumnarPage::from_rows(42, &rows).unwrap();
        let (header, decoded) = ColumnarPage::to_rows(&bytes).unwrap();
        assert_eq!(header.page_id, 42);
        assert_eq!(header.row_count as usize, rows.len());
        assert_eq!(decoded, rows);
    }

    #[test]
    fn columnar_handles_empty() {
        let bytes = ColumnarPage::from_rows(1, &[]).unwrap();
        let (header, rows) = ColumnarPage::to_rows(&bytes).unwrap();
        assert_eq!(header.row_count, 0);
        assert!(rows.is_empty());
    }
}
