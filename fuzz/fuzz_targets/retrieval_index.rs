//! Fuzz both persistent index kinds, including payloads with valid checksums.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > picovolt::persistent::MAX_RETRIEVAL_INDEX_BYTES {
        return;
    }
    let _ = picovolt::persistent::validate_retrieval_index_bytes(data);
    if data.len() >= 92 {
        let mut resealed = data.to_vec();
        let boundary = resealed.len() - 32;
        let checksum = *blake3::hash(&resealed[..boundary]).as_bytes();
        resealed[boundary..].copy_from_slice(&checksum);
        let _ = picovolt::persistent::validate_retrieval_index_bytes(&resealed);
    }
});
