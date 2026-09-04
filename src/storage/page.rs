//! Physical page structures (spec §3): the hot slotted **row** page and the
//! cold packed **columnar** page.
//!
//! A [`RowPage`] wraps a raw 4096-byte buffer and provides O(1) slotted appends:
//! the slot array grows downward from just past the header while record payloads
//! grow upward from the end of the page. [`ColumnarPage`] performs the cold-state
//! transposition, decoding a set of rows column-by-column and applying the
//! §4 compression primitives.

use crate::core::errors::{PvError, Result};
use crate::core::types::{
    ColumnarPageHeader, RecordEnvelope, RowPageHeader, NO_PAGE, PAGE_HEADER_SIZE, PAGE_SIZE,
};
use crate::core::value::{Row, Value};
use crate::storage::compress::{delta_z_decode, delta_z_encode, DictionaryColumn};
use crate::storage::record::{decode_packed_i128, encode_packed_i128};

/// Bytes per slot-array entry: `offset(u16) + len(u16)`.
pub const SLOT_SIZE: usize = 4;

/// Read slot `index`'s `(offset, len)` from a page buffer with full bounds
/// checks, so a crafted page (bad `slot_count` or a slot pointing past the
/// buffer) yields an error rather than an out-of-bounds panic.
fn read_slot(buf: &[u8], index: u16, slot_count: u16) -> Result<(usize, usize)> {
    if index >= slot_count {
        return Err(PvError::OutOfBounds {
            offset: index as usize,
            size: slot_count as usize,
        });
    }
    let slot_pos = PAGE_HEADER_SIZE + index as usize * SLOT_SIZE;
    let entry = buf
        .get(slot_pos..slot_pos + SLOT_SIZE)
        .ok_or_else(|| PvError::Corruption("slot array out of page bounds".into()))?;
    let offset = u16::from_le_bytes(entry[0..2].try_into().unwrap()) as usize;
    let len = u16::from_le_bytes(entry[2..4].try_into().unwrap()) as usize;
    Ok((offset, len))
}

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
        let header = RowPageHeader::decode(&buf[..])?; // validates type discriminant
                                                       // Validate the free-space invariant so a crafted header (from an
                                                       // untrusted .pvdb) cannot underflow the arithmetic in `insert`/`free_space`.
        let slot_array_end = PAGE_HEADER_SIZE + header.slot_count as usize * SLOT_SIZE;
        let free = header.free_space_ptr as usize;
        if free > PAGE_SIZE || slot_array_end > free {
            return Err(PvError::Corruption(format!(
                "row page header out of range: slot_count={}, free_space_ptr={}",
                header.slot_count, header.free_space_ptr
            )));
        }
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
        read_slot(&self.buf[..], index, self.slot_count())
    }

    /// Borrow the record payload stored in slot `index`.
    pub fn record(&self, index: u16) -> Result<&[u8]> {
        let (offset, len) = self.slot(index)?;
        self.buf
            .get(offset..offset + len)
            .ok_or_else(|| PvError::Corruption("row record out of page bounds".into()))
    }

    /// Overwrite the `tx_deleted` field (bytes 8..16) of the envelope in slot
    /// `index`. Used by the MVCC layer to tombstone a version in place.
    pub fn patch_envelope_deleted(&mut self, index: u16, tx_deleted: u64) -> Result<()> {
        let (offset, len) = self.slot(index)?;
        if len < RecordEnvelope::ENCODED_LEN {
            return Err(PvError::Corruption(
                "row record is shorter than its MVCC envelope".into(),
            ));
        }
        let field = self
            .buf
            .get_mut(offset + 8..offset + 16)
            .ok_or_else(|| PvError::Corruption("row envelope is out of page bounds".into()))?;
        field.copy_from_slice(&tx_deleted.to_le_bytes());
        Ok(())
    }

    /// Iterate `(slot_index, record_bytes)` for every slot.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &[u8])> {
        // `map_while` (not `.expect()`): a malformed page stops the iterator
        // rather than panicking, so these helpers can never become a panic vector.
        (0..self.slot_count()).map_while(move |i| self.record(i).ok().map(|r| (i, r)))
    }

    /// The next page in this table's chain, or `None` if this is the tail.
    pub fn next_page(&self) -> Option<u64> {
        match self.header().next_page {
            NO_PAGE => None,
            id => Some(id),
        }
    }

    /// Set (or clear) the next-page link.
    pub fn set_next_page(&mut self, next: Option<u64>) {
        let mut header = self.header();
        header.next_page = next.unwrap_or(NO_PAGE);
        self.set_header(&header);
    }
}

/// A read-only, **borrowing** view over a row-page buffer, lets the buffer pool
/// hand out pages for scanning without copying the 4096 bytes.
pub struct RowPageRef<'a> {
    buf: &'a [u8],
}

impl<'a> RowPageRef<'a> {
    /// Wrap a buffer, validating it is a row page.
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        RowPageHeader::decode(buf)?;
        Ok(Self { buf })
    }

    /// Decode the header.
    pub fn header(&self) -> RowPageHeader {
        RowPageHeader::decode(self.buf).expect("row page header validated on construction")
    }

    /// Number of occupied slots.
    pub fn slot_count(&self) -> u16 {
        self.header().slot_count
    }

    /// The next page in the chain, or `None` at the tail.
    pub fn next_page(&self) -> Option<u64> {
        match self.header().next_page {
            NO_PAGE => None,
            id => Some(id),
        }
    }

    fn slot(&self, index: u16) -> Result<(usize, usize)> {
        read_slot(self.buf, index, self.slot_count())
    }

    /// Borrow the record payload in slot `index`.
    pub fn record(&self, index: u16) -> Result<&'a [u8]> {
        let (offset, len) = self.slot(index)?;
        self.buf
            .get(offset..offset + len)
            .ok_or_else(|| PvError::Corruption("row record out of page bounds".into()))
    }

    /// Iterate `(slot_index, record_bytes)` for every slot.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &'a [u8])> + '_ {
        // `map_while` (not `.expect()`): a malformed page stops the iterator
        // rather than panicking, so these helpers can never become a panic vector.
        (0..self.slot_count()).map_while(move |i| self.record(i).ok().map(|r| (i, r)))
    }
}

// ---------------------------------------------------------------------------
// Columnar (cold) page: transposition + compression
// ---------------------------------------------------------------------------

const COL_ENC_DELTA_Z: u8 = 1;
const COL_ENC_DICTIONARY: u8 = 2;
const COL_ENC_RAW: u8 = 3;
const COL_ENC_PACKED_DECIMAL: u8 = 4;
const COLD_LAYOUT_VERSION: u8 = 1;

/// Cold columnar page codec. Operates on fully-resolved [`Row`]s (CAS pointers
/// already dereferenced), see the module note in the README about CAS-in-cold
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
        let encoded_arity: u16 = arity
            .try_into()
            .map_err(|_| PvError::Schema("too many columns for one columnar page".into()))?;

        let mut out = ColumnarPageHeader { page_id, row_count }.encode().to_vec();
        out.extend_from_slice(&encoded_arity.to_le_bytes());

        for c in 0..arity {
            let column: Vec<&Value> = rows.iter().map(|r| &r[c]).collect();
            let (tag, payload) = encode_column(&column)?;
            out.push(tag);
            let payload_len = u32::try_from(payload.len()).map_err(|_| PvError::PageFull {
                needed: payload.len(),
                available: PAGE_SIZE,
            })?;
            out.extend_from_slice(&payload_len.to_le_bytes());
            out.extend_from_slice(&payload);
        }
        Ok(out)
    }

    /// Inverse of [`Self::from_rows`]: recover the header and row set.
    pub fn to_rows(bytes: &[u8]) -> Result<(ColumnarPageHeader, Vec<Row>)> {
        let header = ColumnarPageHeader::decode(bytes)?;
        if bytes.get(23).copied().unwrap_or_default() != 0 {
            return Err(PvError::Corruption(
                "columnar: maintenance page requires the cold-page decoder".into(),
            ));
        }
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
            let payload = take(bytes, &mut pos, len)
                .map_err(|_| PvError::Corruption("columnar: truncated column payload".into()))?;
            let column = decode_column(tag, payload, row_count)?;
            columns.push(column);
        }
        if bytes[pos..].iter().any(|byte| *byte != 0) {
            return Err(PvError::Corruption(
                "columnar: nonzero bytes after column payloads".into(),
            ));
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

/// A version-5 maintenance page used inside a table page chain. Record addresses
/// stay stable because one source row-page slot maps to the same slot in the
/// transposed page; MVCC envelopes remain a fixed-width block so tombstones can
/// be patched without thawing the page.
pub(crate) struct ColdPage;

impl ColdPage {
    /// Transpose one non-tail row page into a checksummed page-sized buffer.
    pub(crate) fn from_records(
        page_id: u64,
        next_page: Option<u64>,
        original_used_bytes: u32,
        records: &[(RecordEnvelope, Row)],
    ) -> Result<(Box<[u8; PAGE_SIZE]>, usize)> {
        let arity = records.first().map(|(_, row)| row.len()).unwrap_or(0);
        if records.iter().any(|(_, row)| row.len() != arity) {
            return Err(PvError::Schema(
                "cold-page transposition requires uniform row arity".into(),
            ));
        }
        let row_count: u16 = records
            .len()
            .try_into()
            .map_err(|_| PvError::Schema("too many rows for one cold page".into()))?;
        let encoded_arity: u16 = arity
            .try_into()
            .map_err(|_| PvError::Schema("too many columns for one cold page".into()))?;
        if original_used_bytes as usize > PAGE_SIZE {
            return Err(PvError::Schema(
                "cold-page source size exceeds one physical page".into(),
            ));
        }
        let mut header = ColumnarPageHeader { page_id, row_count }.encode();
        header[11..19].copy_from_slice(&next_page.unwrap_or(NO_PAGE).to_le_bytes());
        header[19..23].copy_from_slice(&original_used_bytes.to_le_bytes());
        header[23] = COLD_LAYOUT_VERSION;

        let mut encoded = header.to_vec();
        encoded.extend_from_slice(&encoded_arity.to_le_bytes());
        for (envelope, _) in records {
            encoded.extend_from_slice(&envelope.encode());
        }
        for column in 0..arity {
            let values: Vec<&Value> = records.iter().map(|(_, row)| &row[column]).collect();
            let (tag, payload) = encode_column(&values)?;
            encoded.push(tag);
            let payload_len = u32::try_from(payload.len()).map_err(|_| PvError::PageFull {
                needed: payload.len(),
                available: PAGE_SIZE,
            })?;
            encoded.extend_from_slice(&payload_len.to_le_bytes());
            encoded.extend_from_slice(&payload);
        }
        let encoded_len = encoded.len();
        let page = ColumnarPage::pad_to_page(&encoded)?;
        Self::validate(page.as_ref())?;
        Ok((page, encoded_len))
    }

    pub(crate) fn next_page(bytes: &[u8]) -> Result<Option<u64>> {
        Self::validate(bytes)?;
        let next = u64::from_le_bytes(bytes[11..19].try_into().unwrap());
        Ok((next != NO_PAGE).then_some(next))
    }

    pub(crate) fn original_used_bytes(bytes: &[u8]) -> Result<u32> {
        Self::validate(bytes)?;
        Ok(u32::from_le_bytes(bytes[19..23].try_into().unwrap()))
    }

    pub(crate) fn encoded_len(bytes: &[u8]) -> Result<usize> {
        let (_, _, end) = Self::decode(bytes)?;
        Ok(end)
    }

    pub(crate) fn records(bytes: &[u8]) -> Result<Vec<(RecordEnvelope, Row)>> {
        let (envelopes, rows, _) = Self::decode(bytes)?;
        Ok(envelopes.into_iter().zip(rows).collect())
    }

    pub(crate) fn record(bytes: &[u8], slot: u16) -> Result<(RecordEnvelope, Row)> {
        let records = Self::records(bytes)?;
        records
            .into_iter()
            .nth(slot as usize)
            .ok_or(PvError::OutOfBounds {
                offset: slot as usize,
                size: ColumnarPageHeader::decode(bytes)?.row_count as usize,
            })
    }

    pub(crate) fn envelopes(bytes: &[u8]) -> Result<(Vec<RecordEnvelope>, Option<u64>)> {
        let header = Self::validate(bytes)?;
        let mut pos = PAGE_HEADER_SIZE;
        let _arity = read_u16(bytes, &mut pos)?;
        let mut envelopes = Vec::with_capacity(header.row_count as usize);
        for _ in 0..header.row_count {
            let raw = take(bytes, &mut pos, RecordEnvelope::ENCODED_LEN)?;
            envelopes.push(RecordEnvelope::decode(raw)?);
        }
        Ok((envelopes, Self::next_page(bytes)?))
    }

    pub(crate) fn patch_envelope_deleted(
        bytes: &mut [u8; PAGE_SIZE],
        slot: u16,
        tx_deleted: u64,
    ) -> Result<()> {
        let header = Self::validate(bytes)?;
        if slot >= header.row_count {
            return Err(PvError::OutOfBounds {
                offset: slot as usize,
                size: header.row_count as usize,
            });
        }
        let start = PAGE_HEADER_SIZE + 2 + slot as usize * RecordEnvelope::ENCODED_LEN + 8;
        bytes[start..start + 8].copy_from_slice(&tx_deleted.to_le_bytes());
        Ok(())
    }

    fn validate(bytes: &[u8]) -> Result<ColumnarPageHeader> {
        let header = ColumnarPageHeader::decode(bytes)?;
        if bytes.get(23).copied() != Some(COLD_LAYOUT_VERSION) {
            return Err(PvError::Corruption(
                "columnar: unsupported maintenance-page layout".into(),
            ));
        }
        let original = u32::from_le_bytes(bytes[19..23].try_into().unwrap()) as usize;
        if !(PAGE_HEADER_SIZE..=PAGE_SIZE).contains(&original) {
            return Err(PvError::Corruption(
                "columnar: original page size is out of bounds".into(),
            ));
        }
        let arity = u16::from_le_bytes(
            bytes
                .get(PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 2)
                .ok_or_else(|| PvError::Corruption("columnar: missing row arity".into()))?
                .try_into()
                .unwrap(),
        ) as usize;
        if header.row_count == 0 && arity != 0 {
            return Err(PvError::Corruption(
                "columnar: empty cold page has nonzero row arity".into(),
            ));
        }
        // Every source row occupied one four-byte slot, one fixed MVCC
        // envelope, a two-byte field count, and at least one tag per field.
        // Binding the claimed logical shape to the recorded source-page usage
        // prevents a tiny hostile page from requesting huge decode allocations.
        let per_row = SLOT_SIZE
            .checked_add(RecordEnvelope::ENCODED_LEN)
            .and_then(|size| size.checked_add(2))
            .and_then(|size| size.checked_add(arity))
            .ok_or_else(|| PvError::Corruption("columnar: source row size overflowed".into()))?;
        let minimum_original = (header.row_count as usize)
            .checked_mul(per_row)
            .and_then(|size| size.checked_add(PAGE_HEADER_SIZE))
            .ok_or_else(|| PvError::Corruption("columnar: source page size overflowed".into()))?;
        if minimum_original > original {
            return Err(PvError::Corruption(
                "columnar: row shape cannot fit the recorded source page".into(),
            ));
        }
        Ok(header)
    }

    fn decode(bytes: &[u8]) -> Result<(Vec<RecordEnvelope>, Vec<Row>, usize)> {
        let header = Self::validate(bytes)?;
        let mut pos = PAGE_HEADER_SIZE;
        let arity = read_u16(bytes, &mut pos)? as usize;
        let mut envelopes = Vec::with_capacity(header.row_count as usize);
        for _ in 0..header.row_count {
            let raw = take(bytes, &mut pos, RecordEnvelope::ENCODED_LEN)?;
            envelopes.push(RecordEnvelope::decode(raw)?);
        }
        let mut columns = Vec::with_capacity(arity);
        for _ in 0..arity {
            let tag = *bytes
                .get(pos)
                .ok_or_else(|| PvError::Corruption("columnar: truncated column tag".into()))?;
            pos += 1;
            let len = read_u32(bytes, &mut pos)? as usize;
            let payload = take(bytes, &mut pos, len)?;
            columns.push(decode_column(tag, payload, header.row_count as usize)?);
        }
        let mut rows = vec![Row::with_capacity(arity); header.row_count as usize];
        for column in columns {
            for (row, value) in column.into_iter().enumerate() {
                rows[row].push(value);
            }
        }
        Ok((envelopes, rows, pos))
    }
}

fn encode_column(values: &[&Value]) -> Result<(u8, Vec<u8>)> {
    // Delta-Z if the whole column is integers.
    if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Int(_))) {
        let ints: Vec<i64> = values.iter().map(|v| v.as_int().unwrap()).collect();
        return Ok((COL_ENC_DELTA_Z, delta_z_encode(&ints)));
    }
    // Dictionary if the whole column is low-cardinality text.
    if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Text(_))) {
        let texts: Vec<String> = values
            .iter()
            .map(|v| v.as_text().unwrap().to_owned())
            .collect();
        if let Some(dict) = DictionaryColumn::build(&texts) {
            return Ok((COL_ENC_DICTIONARY, serialize_dict(&dict)));
        }
    }
    if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Decimal(_))) {
        let mut packed = Vec::new();
        for value in values {
            let Value::Decimal(mantissa) = value else {
                unreachable!("column type checked above")
            };
            encode_packed_i128(&mut packed, *mantissa);
        }
        return Ok((COL_ENC_PACKED_DECIMAL, packed));
    }
    // Fallback: raw tagged values.
    Ok((COL_ENC_RAW, encode_raw_column(values)?))
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
        COL_ENC_PACKED_DECIMAL => {
            let mut pos = 0usize;
            let mut decimals = Vec::with_capacity(row_count.min(payload.len()));
            for _ in 0..row_count {
                decimals.push(Value::Decimal(decode_packed_i128(payload, &mut pos)?));
            }
            if pos != payload.len() {
                return Err(PvError::Corruption(
                    "packed decimal column has trailing bytes".into(),
                ));
            }
            decimals
        }
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

fn encode_raw_column(values: &[&Value]) -> Result<Vec<u8>> {
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
            Value::Decimal(mantissa) => {
                out.push(4);
                encode_packed_i128(&mut out, *mantissa);
            }
        }
    }
    Ok(out)
}

fn decode_raw_column(payload: &[u8]) -> Result<Vec<Value>> {
    let mut pos = 0usize;
    let count = read_u32(payload, &mut pos)? as usize;
    // Each entry is at least one tag byte, so cap the pre-allocation by the
    // remaining payload to avoid an OOM from a crafted count.
    let mut out = Vec::with_capacity(count.min(payload.len()));
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
            4 => Value::Decimal(decode_packed_i128(payload, &mut pos)?),
            other => {
                return Err(PvError::Corruption(format!(
                    "columnar raw: bad value tag 0x{other:02X}"
                )))
            }
        };
        out.push(value);
    }
    if pos != payload.len() {
        return Err(PvError::Corruption("columnar raw: trailing bytes".into()));
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
    if symbol_count > crate::storage::compress::MAX_DICTIONARY_SYMBOLS {
        return Err(PvError::Corruption(format!(
            "dictionary: {symbol_count} symbols exceeds the codec limit"
        )));
    }
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
    if pos != payload.len() {
        return Err(PvError::Corruption(
            "dictionary: trailing bytes after code stream".into(),
        ));
    }
    Ok(DictionaryColumn {
        symbols,
        bits,
        count,
        codes,
    })
}

// --- little local readers (bounds-checked) ---------------------------------

fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .filter(|end| *end <= buf.len())
        .ok_or_else(|| PvError::Corruption("columnar: unexpected end of buffer".into()))?;
    let slice = buf
        .get(*pos..end)
        .ok_or_else(|| PvError::Corruption("columnar: unexpected end of buffer".into()))?;
    *pos = end;
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
    fn envelope_patch_rejects_a_short_record() {
        let mut page = RowPage::new(1);
        let slot = page.insert(b"not an envelope").unwrap();
        let err = page.patch_envelope_deleted(slot, 1).unwrap_err();
        assert!(matches!(err, PvError::Corruption(_)), "{err:?}");
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

    #[test]
    fn columnar_packs_decimal_columns_and_mixed_decimal_values() {
        let rows = vec![
            vec![Value::Decimal(1_500_000), Value::Null],
            vec![Value::Decimal(-1), Value::Decimal(i128::MAX)],
        ];
        let bytes = ColumnarPage::from_rows(0, &rows).unwrap();
        assert!(bytes.len() < 90, "small decimals should be packed");
        assert_eq!(ColumnarPage::to_rows(&bytes).unwrap().1, rows);
    }

    #[test]
    fn cold_page_preserves_slots_envelopes_and_links() {
        let records = vec![
            (
                RecordEnvelope::new(1, 0),
                vec![Value::Int(7), Value::Decimal(42)],
            ),
            (
                RecordEnvelope::new(2, 0),
                vec![Value::Int(8), Value::Decimal(-1)],
            ),
        ];
        let (mut bytes, encoded) = ColdPage::from_records(4, Some(9), 300, &records).unwrap();
        assert!(encoded < 300);
        assert_eq!(ColdPage::next_page(bytes.as_ref()).unwrap(), Some(9));
        assert_eq!(ColdPage::original_used_bytes(bytes.as_ref()).unwrap(), 300);
        assert_eq!(ColdPage::records(bytes.as_ref()).unwrap(), records);
        ColdPage::patch_envelope_deleted(&mut bytes, 1, 17).unwrap();
        assert_eq!(
            ColdPage::record(bytes.as_ref(), 1).unwrap().0.tx_deleted,
            17
        );
    }

    #[test]
    fn cold_page_rejects_an_impossible_compressed_shape() {
        let records = vec![(RecordEnvelope::new(1, 0), vec![Value::Int(7)])];
        let (mut bytes, _) = ColdPage::from_records(4, None, 100, &records).unwrap();

        // A source row page cannot contain 65,535 record envelopes. Reject the
        // header before allocating or decoding the claimed logical row set.
        bytes[9..11].copy_from_slice(&u16::MAX.to_le_bytes());
        let error = ColdPage::records(bytes.as_ref()).unwrap_err();
        assert!(error.to_string().contains("cannot fit"), "{error}");
    }

    #[test]
    fn dictionary_payload_rejects_excess_symbols_and_trailing_bytes() {
        let dictionary = DictionaryColumn::build(&["same".into(), "same".into()]).unwrap();
        let mut encoded = serialize_dict(&dictionary);
        encoded.push(0);
        assert!(deserialize_dict(&encoded).is_err());

        let too_many = (crate::storage::compress::MAX_DICTIONARY_SYMBOLS as u16 + 1).to_le_bytes();
        assert!(deserialize_dict(&too_many).is_err());
    }

    #[test]
    fn columnar_rejects_wrong_row_counts_and_trailing_bytes() {
        let rows = vec![vec![Value::Null], vec![Value::Text("value".into())]];
        let mut wrong_count = ColumnarPage::from_rows(7, &rows).unwrap();
        wrong_count[9..11].copy_from_slice(&1u16.to_le_bytes());
        assert!(ColumnarPage::to_rows(&wrong_count).is_err());

        let mut trailing = ColumnarPage::from_rows(7, &rows).unwrap();
        trailing.push(1);
        assert!(ColumnarPage::to_rows(&trailing).is_err());

        let padded =
            ColumnarPage::pad_to_page(&ColumnarPage::from_rows(7, &rows).unwrap()).unwrap();
        assert_eq!(ColumnarPage::to_rows(padded.as_ref()).unwrap().1, rows);
    }
}
