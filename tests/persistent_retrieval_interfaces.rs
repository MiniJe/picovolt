// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
#![cfg(all(
    feature = "full-text",
    feature = "vector-search",
    any(
        feature = "capi",
        all(feature = "encryption", not(target_arch = "wasm32"))
    )
))]
use picovolt::Database;
use serde_json::json;

fn fixture() -> Database {
    Database::import_bytes(include_bytes!("fixtures/format_v8.pvdb")).unwrap()
}

fn request() -> String {
    json!({"kind":"hybrid","sql":"SELECT * FROM docs","id_column":"id",
        "text_columns":["title","body"],"vector_column":"embedding",
        "query":"verified snapshot","vector_query":[1,0,0],"metric":"cosine",
        "text_weight":0.5,"candidate_limit":10,"limit":10,
        "text_index":"ft","vector_index":"vx"})
    .to_string()
}

#[cfg(all(feature = "encryption", not(target_arch = "wasm32")))]
#[test]
fn encrypted_snapshot_retains_named_catalog_and_exact_retrieval() {
    use picovolt::encryption::{self, Secret};
    let secret = Secret::key([0x23; 32]);
    let mut db = fixture();
    let expected = db.retrieve_json(&request()).unwrap();
    let bytes = encryption::seal(&mut db, &secret).unwrap();
    let mut reopened = encryption::open(&bytes, &secret).unwrap();
    assert_eq!(reopened.retrieval_indexes().len(), 2);
    assert_eq!(reopened.retrieve_json(&request()).unwrap(), expected);
    for index in reopened.retrieval_indexes() {
        reopened
            .verify_retrieval_index(&index.definition.name)
            .unwrap();
    }
}

#[cfg(feature = "capi")]
#[test]
fn c_abi_import_retrieve_and_drop_preserve_pointer_ownership_and_errors() {
    use picovolt::ffi::*;
    use std::ffi::{CStr, CString};
    let mut db = fixture();
    let expected = db.retrieve_json(&request()).unwrap();
    let bytes = db.bake_to_bytes().unwrap();
    let request = CString::new(request()).unwrap();
    // All buffers live across their calls. Every non-null owned ABI allocation
    // is freed with the matching ABI allocator, never the Rust allocator.
    unsafe {
        let handle = pv_import(bytes.as_ptr(), bytes.len());
        assert!(!handle.is_null());
        let hits = pv_retrieve(handle, request.as_ptr());
        assert!(!hits.is_null());
        let actual = CStr::from_ptr(hits).to_str().unwrap().to_owned();
        pv_string_free(hits);
        assert_eq!(actual, expected);
        let sql = CString::new("DROP INDEX ft").unwrap();
        let result = pv_query(handle, sql.as_ptr());
        assert!(!result.is_null());
        pv_string_free(result);
        assert!(pv_retrieve(handle, request.as_ptr()).is_null());
        assert!(!pv_last_error().is_null());
        pv_close(handle);
    }
}
