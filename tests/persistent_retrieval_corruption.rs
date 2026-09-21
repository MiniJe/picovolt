// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
#![cfg(all(feature = "full-text", feature = "vector-search")))]
use picovolt::{persistent::validate_retrieval_index_bytes, Database};
use serde_json::{json, Value as Json};

fn image() -> Vec<u8> {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
    db.query("INSERT INTO docs VALUES (1,'apple apple banana','[1,0]'),(2,'apple banana banana','[0,1]')").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
    db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    db.bake_to_bytes().unwrap()
}

fn metadata(image: &[u8]) -> (usize, usize, Json) {
    let manifest = u64::from_le_bytes(image[8..16].try_into().unwrap()) as usize;
    let cas = u64::from_le_bytes(image[16..24].try_into().unwrap()) as usize;
    (cas, manifest, serde_json::from_slice(&image[manifest..]).unwrap())
}

fn blob(image: &[u8], name: &str) -> Vec<u8> {
    let (cas, _, metadata) = metadata(image);
    let descriptor = metadata["retrieval_indexes"].as_array().unwrap().iter().find(|value| value["definition"]["name"] == name).unwrap();
    let id = descriptor["cas_id"].as_u64().unwrap() as usize;
    let offset = metadata["cas_dir"][id][0].as_u64().unwrap() as usize;
    let length = metadata["cas_dir"][id][1].as_u64().unwrap() as usize;
    image[cas + offset..cas + offset + length].to_vec()
}

fn rehash(bytes: &mut [u8]) {
    let split = bytes.len() - 32;
    let hash = *blake3::hash(&bytes[..split]).as_bytes();
    bytes[split..].copy_from_slice(&hash);
}

fn body_start(bytes: &[u8]) -> usize {
    24 + u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize + 32 + 4
}

fn replace_blob(image: &[u8], name: &str, replacement: &[u8]) -> Vec<u8> {
    let (cas, _, mut metadata) = metadata(image);
    let descriptors = metadata["retrieval_indexes"].as_array_mut().unwrap();
    let descriptor = descriptors.iter_mut().find(|value| value["definition"]["name"] == name).unwrap();
    let target = descriptor["cas_id"].as_u64().unwrap() as usize;
    descriptor["encoded_bytes"] = json!(replacement.len());
    let original_directory = metadata["cas_dir"].as_array().unwrap().clone();
    let mut pool = Vec::new();
    for (id, extent) in original_directory.iter().enumerate() {
        let offset = extent[0].as_u64().unwrap() as usize;
        let length = extent[1].as_u64().unwrap() as usize;
        let bytes = if id == target { replacement } else { &image[cas + offset..cas + offset + length] };
        metadata["cas_dir"][id] = json!([pool.len(), bytes.len()]);
        metadata["cas_hashes"][id] = json!(blake3::hash(bytes).to_hex().to_string());
        pool.extend_from_slice(bytes);
    }
    assert!(metadata.get("index_region").is_none());
    let mut output = image[..cas].to_vec(); output.extend(pool);
    let manifest = output.len() as u64;
    output[8..16].copy_from_slice(&manifest.to_le_bytes());
    output.extend(serde_json::to_vec(&metadata).unwrap());
    output
}

fn rewrite_manifest(image: &[u8], update: impl FnOnce(&mut Json)) -> Vec<u8> {
    let (_, offset, mut metadata) = metadata(image); update(&mut metadata);
    let mut result = image[..offset].to_vec(); result.extend(serde_json::to_vec(&metadata).unwrap()); result
}

fn rejects(bytes: &[u8]) {
    let result = std::panic::catch_unwind(|| Database::import_bytes(bytes));
    assert!(result.is_ok(), "malformed retrieval index panicked");
    assert!(result.unwrap().is_err(), "malformed retrieval index was accepted");
}

#[test]
fn envelope_magic_versions_lengths_checksum_and_trailing_bytes_are_checked() {
    let image = image();
    for name in ["ft", "vx"] {
        let original = blob(&image, name); validate_retrieval_index_bytes(&original).unwrap();
        for offset in [0, 8, 10, 11, 20, 21, 22, 23, original.len() - 1] {
            let mut changed = original.clone(); changed[offset] ^= 0x7f;
            if offset != original.len() - 1 { rehash(&mut changed); }
            assert!(validate_retrieval_index_bytes(&changed).is_err(), "{name} at {offset}");
            rejects(&replace_blob(&image, name, &changed));
        }
        for length in [0, 1, 12, 91, original.len() - 1] {
            assert!(validate_retrieval_index_bytes(&original[..length]).is_err());
        }
        let mut trailing = original.clone();
        trailing.insert(trailing.len() - 32, 0); rehash(&mut trailing);
        assert!(validate_retrieval_index_bytes(&trailing).is_err());
        rejects(&replace_blob(&image, name, &trailing));
    }
}

#[test]
fn canonical_rehash_cannot_make_wrong_document_ids_or_vectors_healthy() {
    let image = image();
    let mut vector = blob(&image, "vx"); let start = body_start(&vector);
    // Valid finite vector, same dimensions and sorted IDs; only agreement with
    // the authoritative table detects the forged neighbor coordinates.
    vector[start + 20..start + 24].copy_from_slice(&0.5f32.to_le_bytes()); rehash(&mut vector);
    validate_retrieval_index_bytes(&vector).unwrap();
    rejects(&replace_blob(&image, "vx", &vector));
    let mut vector = blob(&image, "vx"); let start = body_start(&vector);
    vector[start + 12..start + 20].copy_from_slice(&(-7i64).to_le_bytes()); rehash(&mut vector);
    validate_retrieval_index_bytes(&vector).unwrap();
    rejects(&replace_blob(&image, "vx", &vector));
    let mut text = blob(&image, "ft"); let start = body_start(&text);
    text[start + 16..start + 24].copy_from_slice(&(-7i64).to_le_bytes()); rehash(&mut text);
    validate_retrieval_index_bytes(&text).unwrap();
    rejects(&replace_blob(&image, "ft", &text));
}

#[test]
fn dimensions_counts_postings_and_descriptor_disagreement_fail_closed() {
    let image = image();
    for name in ["ft", "vx"] {
        let original = blob(&image, name); let start = body_start(&original);
        for relative in [0, 4] {
            let mut changed = original.clone(); changed[start + relative..start + relative + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            rehash(&mut changed); rejects(&replace_blob(&image, name, &changed));
        }
    }
    let original = blob(&image, "ft"); let start = body_start(&original);
    for relative in [24, 28, 32, 36] {
        let mut changed = original.clone(); changed[start + relative..start + relative + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        rehash(&mut changed); rejects(&replace_blob(&image, "ft", &changed));
    }
    for field in ["encoded_bytes", "document_count", "generation", "cas_id"] {
        rejects(&rewrite_manifest(&image, |metadata| metadata["retrieval_indexes"][0][field] = json!(u64::MAX)));
    }
    rejects(&rewrite_manifest(&image, |metadata| metadata["retrieval_indexes"][0]["definition"]["table"] = json!("missing")));
    rejects(&rewrite_manifest(&image, |metadata| metadata["retrieval_indexes"][0]["definition"]["id_column"] = json!("body")));
    rejects(&rewrite_manifest(&image, |metadata| metadata["retrieval_indexes"][0]["unexpected"] = json!(true)));
    rejects(&rewrite_manifest(&image, |metadata| metadata["retrieval_indexes"][1]["definition"]["name"] = json!("ft")));
    let mut understated = rewrite_manifest(&image, |metadata| metadata["format_version"] = json!(7));
    understated[4..6].copy_from_slice(&7u16.to_le_bytes()); rejects(&understated);
}

#[test]
fn arbitrary_decoder_inputs_are_bounded_and_never_panic() {
    let mut random = 0x23_123456789u64;
    for length in 0..512 {
        let mut bytes = vec![0; length];
        for byte in &mut bytes { random ^= random << 13; random ^= random >> 7; random ^= random << 17; *byte = random as u8; }
        assert!(std::panic::catch_unwind(|| validate_retrieval_index_bytes(&bytes)).is_ok());
    }
    let oversized = vec![0; picovolt::persistent::MAX_RETRIEVAL_INDEX_BYTES + 1];
    assert!(validate_retrieval_index_bytes(&oversized).is_err());
}

#[test]
fn workspace_index_damage_fails_open_without_changing_source_files() {
    let temp = tempfile::tempdir().unwrap(); let root = temp.path().join("workspace");
    let mut db = Database::open_dev(&root).unwrap();
    db.query("CREATE TABLE docs (id,body)").unwrap(); db.query("INSERT INTO docs VALUES (1,'authoritative')").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap(); drop(db);
    let manifest: Json = serde_json::from_slice(&std::fs::read(root.join("pv_manifest.json")).unwrap()).unwrap();
    let id = manifest["retrieval_indexes"][0]["cas_id"].as_u64().unwrap() as usize;
    let hash = manifest["cas_hashes"][id].as_str().unwrap(); let path = root.join("blobs").join(&hash[..2]).join(hash);
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"damaged").unwrap(); assert!(Database::open_dev(&root).is_err());
    // Restoring only the independently retained derived blob restores open; the
    // corruption path did not rewrite any table or attempt a silent repair.
    std::fs::write(&path, original).unwrap(); let mut db = Database::open_dev(&root).unwrap();
    assert_eq!(db.query("SELECT body FROM docs").unwrap().rows().unwrap()[0][0], picovolt::Value::Text("authoritative".into()));
}
