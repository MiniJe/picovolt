//! Hyper-compression primitives (spec §4).
//!
//! Three independent, individually-tested codecs used by the columnar page
//! layout:
//!
//! * **Delta-Z**, delta encoding + ZigZag + LEB128 varints for monotonic-ish
//!   integer sequences (timestamps, auto-increment ids).
//! * **Dictionary bit-packing**, low-cardinality text columns become a symbol
//!   table plus an array of `ceil(log2(card))`-bit codes.
//! * **LEB128 varints**, the shared variable-width integer substrate.

use crate::core::errors::{PvError, Result};

// ---------------------------------------------------------------------------
// LEB128 unsigned varints
// ---------------------------------------------------------------------------

/// Append `value` to `out` as an unsigned LEB128 varint (1 byte for values < 128).
pub fn write_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Read an unsigned LEB128 varint from `buf` starting at `*pos`, advancing `*pos`.
pub fn read_uvarint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf
            .get(*pos)
            .ok_or_else(|| PvError::Corruption("truncated varint".into()))?;
        *pos += 1;
        if shift >= 64 {
            return Err(PvError::Corruption("varint overflow".into()));
        }
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// ZigZag (map signed -> unsigned so small magnitudes stay small)
// ---------------------------------------------------------------------------

/// Map a signed integer onto an unsigned one such that small magnitudes (either
/// sign) map to small values.
#[inline]
pub fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// Inverse of [`zigzag_encode`].
#[inline]
pub fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// ---------------------------------------------------------------------------
// Delta-Z integer column codec
// ---------------------------------------------------------------------------

/// Encode an `i64` column as `count | base | zigzag-varint(delta)...`.
///
/// The base value `X₀` is stored verbatim (8 bytes); each subsequent entry is
/// the ZigZag varint of `Xₙ − Xₙ₋₁`, so a monotonic sequence collapses to one
/// byte per element.
pub fn delta_z_encode(values: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() + 9);
    write_uvarint(&mut out, values.len() as u64);
    if let Some(&base) = values.first() {
        out.extend_from_slice(&base.to_le_bytes());
        let mut prev = base;
        for &v in &values[1..] {
            write_uvarint(&mut out, zigzag_encode(v.wrapping_sub(prev)));
            prev = v;
        }
    }
    out
}

/// Inverse of [`delta_z_encode`].
pub fn delta_z_decode(buf: &[u8]) -> Result<Vec<i64>> {
    let mut pos = 0usize;
    let count = read_uvarint(buf, &mut pos)? as usize;
    // Each value past the base is at least a 1-byte varint, so cap the
    // pre-allocation by the input length to avoid OOM from a crafted count.
    let mut out = Vec::with_capacity(count.min(buf.len()));
    if count == 0 {
        return Ok(out);
    }
    let base_bytes = buf
        .get(pos..pos + 8)
        .ok_or_else(|| PvError::Corruption("delta-z: missing base".into()))?;
    let mut prev = i64::from_le_bytes(base_bytes.try_into().unwrap());
    pos += 8;
    out.push(prev);
    for _ in 1..count {
        let delta = zigzag_decode(read_uvarint(buf, &mut pos)?);
        prev = prev.wrapping_add(delta);
        out.push(prev);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Bit-packing of small fixed-width codes (1..=8 bits)
// ---------------------------------------------------------------------------

/// Minimum number of bits needed to represent `cardinality` distinct codes
/// (`0` maps to a single-bit code; the range is clamped to `1..=8`).
pub fn bits_for_cardinality(cardinality: usize) -> u8 {
    if cardinality <= 1 {
        return 1;
    }
    let bits = (usize::BITS - (cardinality - 1).leading_zeros()) as u8;
    bits.clamp(1, 8)
}

/// Pack `codes` (each `< 2^bits`, `bits` in `1..=8`) LSB-first into bytes.
pub fn pack_codes(codes: &[u8], bits: u8) -> Vec<u8> {
    debug_assert!((1..=8).contains(&bits));
    let mut out = Vec::with_capacity((codes.len() * bits as usize).div_ceil(8));
    let mut acc: u16 = 0;
    let mut filled: u8 = 0;
    for &code in codes {
        acc |= u16::from(code) << filled;
        filled += bits;
        while filled >= 8 {
            out.push((acc & 0xFF) as u8);
            acc >>= 8;
            filled -= 8;
        }
    }
    if filled > 0 {
        out.push((acc & 0xFF) as u8);
    }
    out
}

/// Inverse of [`pack_codes`]; recovers exactly `count` codes.
pub fn unpack_codes(packed: &[u8], bits: u8, count: usize) -> Result<Vec<u8>> {
    // SECURITY: `bits` comes from a decoded (possibly malicious) dictionary, so
    // validate it at runtime, a `debug_assert` would panic in debug builds and
    // `1u16 << bits` would shift-overflow in release for `bits >= 16`.
    if !(1..=8).contains(&bits) {
        return Err(PvError::Corruption(format!(
            "bit-pack: invalid bit width {bits} (must be 1..=8)"
        )));
    }
    // Each code consumes at least one bit, so the input bounds the code count.
    let mut out = Vec::with_capacity(count.min(packed.len().saturating_mul(8)));
    let mask = ((1u16 << bits) - 1) as u8;
    let mut acc: u16 = 0;
    let mut filled: u8 = 0;
    let mut byte_iter = packed.iter();
    for _ in 0..count {
        while filled < bits {
            let byte = *byte_iter
                .next()
                .ok_or_else(|| PvError::Corruption("bit-pack: truncated stream".into()))?;
            acc |= u16::from(byte) << filled;
            filled += 8;
        }
        out.push((acc & u16::from(mask)) as u8);
        acc >>= bits;
        filled -= bits;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dictionary codec for low-cardinality text columns
// ---------------------------------------------------------------------------

/// Largest distinct-symbol count the dictionary codec will accept (8-bit codes).
pub const MAX_DICTIONARY_SYMBOLS: usize = 256;

/// A localized symbol table plus its bit-packed code stream.
#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryColumn {
    /// Distinct symbols, indexed by code.
    pub symbols: Vec<String>,
    /// Bits per code.
    pub bits: u8,
    /// Number of encoded rows.
    pub count: usize,
    /// Bit-packed codes.
    pub codes: Vec<u8>,
}

impl DictionaryColumn {
    /// Build a dictionary column from text values, or `None` if the cardinality
    /// exceeds [`MAX_DICTIONARY_SYMBOLS`] (caller should fall back to raw storage).
    pub fn build(values: &[String]) -> Option<Self> {
        let mut symbols: Vec<String> = Vec::new();
        let mut codes: Vec<u8> = Vec::with_capacity(values.len());
        for v in values {
            let code = match symbols.iter().position(|s| s == v) {
                Some(i) => i,
                None => {
                    if symbols.len() >= MAX_DICTIONARY_SYMBOLS {
                        return None;
                    }
                    symbols.push(v.clone());
                    symbols.len() - 1
                }
            };
            codes.push(code as u8);
        }
        let bits = bits_for_cardinality(symbols.len());
        Some(Self {
            bits,
            count: values.len(),
            codes: pack_codes(&codes, bits),
            symbols,
        })
    }

    /// Decode back to the original text values.
    pub fn decode(&self) -> Result<Vec<String>> {
        let codes = unpack_codes(&self.codes, self.bits, self.count)?;
        codes
            .into_iter()
            .map(|c| {
                self.symbols
                    .get(c as usize)
                    .cloned()
                    .ok_or_else(|| PvError::Corruption("dictionary: code out of range".into()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvarint_round_trips() {
        for v in [0u64, 1, 127, 128, 300, 16_384, u64::MAX] {
            let mut buf = Vec::new();
            write_uvarint(&mut buf, v);
            let mut pos = 0;
            assert_eq!(read_uvarint(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
        }
        assert_eq!(
            {
                let mut b = Vec::new();
                write_uvarint(&mut b, 127);
                b.len()
            },
            1
        );
    }

    #[test]
    fn zigzag_round_trips() {
        for v in [0i64, -1, 1, -64, 63, i64::MIN, i64::MAX] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v);
        }
        // Small magnitudes encode to small varints.
        assert!(zigzag_encode(-1) < 4);
    }

    #[test]
    fn delta_z_round_trips_and_compresses_monotonic() {
        let timestamps: Vec<i64> = (1_700_000_000..1_700_000_050).collect();
        let encoded = delta_z_encode(&timestamps);
        assert_eq!(delta_z_decode(&encoded).unwrap(), timestamps);
        // 50 ints * 8 bytes = 400 raw; delta-z should be far smaller.
        assert!(encoded.len() < 70, "got {} bytes", encoded.len());
    }

    #[test]
    fn delta_z_handles_empty_and_single() {
        assert_eq!(
            delta_z_decode(&delta_z_encode(&[])).unwrap(),
            Vec::<i64>::new()
        );
        assert_eq!(delta_z_decode(&delta_z_encode(&[42])).unwrap(), vec![42]);
    }

    #[test]
    fn bits_for_cardinality_matches_spec_example() {
        // 3 distinct states -> 2 bits -> 4 records per byte.
        assert_eq!(bits_for_cardinality(3), 2);
        assert_eq!(bits_for_cardinality(1), 1);
        assert_eq!(bits_for_cardinality(2), 1);
        assert_eq!(bits_for_cardinality(4), 2);
        assert_eq!(bits_for_cardinality(5), 3);
        assert_eq!(bits_for_cardinality(256), 8);
    }

    #[test]
    fn bit_pack_round_trips() {
        let codes = [0u8, 1, 2, 0, 1, 2, 2, 1, 0];
        let packed = pack_codes(&codes, 2);
        // 9 two-bit codes -> ceil(18/8) = 3 bytes.
        assert_eq!(packed.len(), 3);
        assert_eq!(unpack_codes(&packed, 2, codes.len()).unwrap(), codes);
    }

    #[test]
    fn dictionary_packs_low_cardinality() {
        let values: Vec<String> = (0..1000)
            .map(|i| {
                match i % 3 {
                    0 => "Pending",
                    1 => "Active",
                    _ => "Archived",
                }
                .to_string()
            })
            .collect();
        let dict = DictionaryColumn::build(&values).unwrap();
        assert_eq!(dict.symbols.len(), 3);
        assert_eq!(dict.bits, 2);
        // 1000 * 2 bits = 250 bytes of codes vs raw text.
        assert_eq!(dict.codes.len(), 250);
        assert_eq!(dict.decode().unwrap(), values);
    }

    #[test]
    fn dictionary_bails_on_high_cardinality() {
        let values: Vec<String> = (0..300).map(|i| format!("sym{i}")).collect();
        assert!(DictionaryColumn::build(&values).is_none());
    }
}
