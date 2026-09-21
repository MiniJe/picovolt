"""PV-2.3-M-001: reviewed snapshot, model and crash qualification patch."""
from pathlib import Path
import subprocess

changes = {}
def replace(path, old, new):
    value = changes.get(path, Path(path).read_text())
    if new in value:
        return
    if value.count(old) != 1:
        raise SystemExit(f'Unexpected source anchor in {path}: {old[:140]}')
    changes[path] = value.replace(old, new, 1)

def append(path, marker, content):
    value = changes.get(path, Path(path).read_text() if Path(path).exists() else '')
    if marker not in value:
        changes[path] = value + content

# Native retained read sessions execute retrieval on their existing private
# snapshot worker, never on the mutable writer or a newly opened snapshot.
replace('src/concurrent.rs', 'enum ReadCommand {\n    Query {', '''enum ReadCommand {
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    Retrieve { request: String, reply: Reply<String> },
    Query {''')
replace('src/concurrent.rs', 'impl SharedDatabase {\n', '''impl SharedDatabase {
    /// Retrieve from one bounded immutable committed snapshot. For repeated
    /// queries without repeated snapshot admission, retain a ReadTransaction.
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub fn retrieve_json(&self, request: &str) -> Result<String> {
        if request.len() > 131072 { return Err(PvError::Query("Retrieval request exceeds 128 KiB".into())); }
        self.begin_read()?.retrieve_json(request)
    }
''')
replace('src/concurrent.rs', 'impl ReadTransaction {\n', '''impl ReadTransaction {
    /// Retrieve from this reader's pinned table/index generation. Existing
    /// session scan/materialization/deadline/cancellation limits remain active.
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub fn retrieve_json(&mut self, request: &str) -> Result<String> {
        self.options.check()?;
        if request.len() > 131072 { return Err(PvError::Query("Retrieval request exceeds 128 KiB".into())); }
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let sender = self.sender.as_ref().ok_or(PvError::DatabaseClosed)?;
        sender.send(ReadCommand::Retrieve { request: request.to_owned(), reply: reply_sender })
            .map_err(|_| self.session_error())?;
        reply_receiver.recv().map_err(|_| self.session_error())?
    }
''')
replace('src/concurrent.rs', '            Ok(ReadCommand::Close { reply }) => {', '''            #[cfg(any(feature = "full-text", feature = "vector-search"))]
            Ok(ReadCommand::Retrieve { request, reply }) => {
                let outcome = call_database(|| {
                    options.check()?;
                    database.retrieve_json_controlled(&request, options.limits,
                        Some(options.cancellation.clone()), Some(snapshot_tx))
                });
                let (result, safe) = match outcome {
                    DatabaseCall::Ok(value) => (Ok(value), true),
                    DatabaseCall::Error(error) => (Err(error), true),
                    DatabaseCall::Panicked(error) => (Err(error), false),
                };
                let _ = reply.send(result);
                if !safe { return None; }
            }
            Ok(ReadCommand::Close { reply }) => {''')
replace('src/concurrent.rs', '    /// Begin a snapshot-stable, read-only transaction. No writer or other read\n    /// transaction executes until this handle closes or is dropped.', '    /// Begin a snapshot-stable, read-only transaction. After its private image\n    /// is admitted, later writers and other readers may execute concurrently.')

replace('src/retrieval.rs', 'fn snapshot(\n', '''struct RetrievalControl {
    limits: QueryLimits,
    cancellation: Option<crate::CancellationToken>,
    pinned: Option<crate::TxId>,
}

impl RetrievalControl {
    fn check(&self) -> crate::Result<()> {
        if let Some(token) = &self.cancellation { token.check()?; }
        if self.limits.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(PvError::ResourceLimit("deadline expired".into()));
        }
        Ok(())
    }
}

fn snapshot(
''')
replace('src/retrieval.rs', '    params: &[serde_json::Value],\n) -> crate::Result<Selection> {', '    params: &[serde_json::Value],\n    control: &RetrievalControl,\n) -> crate::Result<Selection> {\n    control.check()?;')
replace('src/retrieval.rs', '    // Never substitute current postings for historical data, or raw source', '''    if before.zip(control.pinned).is_some_and(|(requested, pinned)| requested > pinned) {
        return Err(PvError::Transaction("retrieval requested a snapshot newer than its pinned reader".into()));
    }
    // Never substitute current postings for historical data, or raw source''')
replace('src/retrieval.rs', '''    let rows = db.query_with_limits(
        sql,
        &values,
        QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None),
    )?;''', '''    let rows = db.query_with_limits_cancellable(
        sql, &values, control.limits, control.cancellation.clone(),
    )?;''')
replace('src/retrieval.rs', '''    pub fn retrieve_json(&mut self, request: &str) -> crate::Result<String> {
        if request.len() > 131072 {''', '''    pub fn retrieve_json(&mut self, request: &str) -> crate::Result<String> {
        self.retrieve_json_controlled(request,
            QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None), None, None)
    }

    pub(crate) fn retrieve_json_controlled(&mut self, request: &str, limits: QueryLimits,
        cancellation: Option<crate::CancellationToken>, pinned: Option<crate::TxId>) -> crate::Result<String> {
        let control = RetrievalControl {
            limits: QueryLimits::new(limits.max_rows_scanned.min(100_000),
                limits.max_materialized_bytes.min(16 * 1024 * 1024),
                limits.max_result_rows.min(10_000), limits.deadline),
            cancellation, pinned,
        };
        control.check()?;
        if request.len() > 131072 {''')
path = 'src/retrieval.rs'
value = changes[path]
old = 'snapshot(self, &sql, &params)?'
if value.count(old) != 3:
    raise SystemExit('Expected three retrieval snapshot call sites')
changes[path] = value.replace(old, 'snapshot(self, &sql, &params, &control)?')
replace('src/retrieval.rs', '        serde_json::to_string(&hits).map_err(|error| PvError::Query(error.to_string()))', '        control.check()?;\n        serde_json::to_string(&hits).map_err(|error| PvError::Query(error.to_string()))')

# Reject retrieval CAS symlinks rather than following them during preflight.
replace('src/db/persistent.rs', 'let metadata = fs::metadata(', 'let metadata = fs::symlink_metadata(')
replace('src/db/persistent.rs', '        if !self.in_transaction() {', '        #[cfg(test)]\n        crate::persistent::crash_point("before_index_update");\n        if !self.in_transaction() {')
replace('src/db/persistent.rs', '            let cas_id = self.cas.put(&bytes)?;', '            let cas_id = self.cas.put(&bytes)?;\n            #[cfg(test)]\n            crate::persistent::crash_point("after_index_bytes");')
replace('src/persistent.rs', '        output.put(MAGIC)?;', '        output.put(MAGIC)?;\n        #[cfg(test)]\n        crash_point("during_encoding");')
path = 'src/db.rs'
value = Path(path).read_text()
start = value.index('        self.persist_retrieval()?;')
end = value.index('    fn write_manifest_fast', start)
block = value[start:end]
old = '        let json = serde_json::to_vec(&manifest)?;'
assert block.count(old) == 1
block = block.replace(old, '        #[cfg(test)]\n        crate::persistent::crash_point("before_manifest");\n' + old)
old = '            self.write_manifest_atomic(&root, &json)?;'
assert block.count(old) == 1
block = block.replace(old, old + '\n            #[cfg(test)]\n            crate::persistent::crash_point("after_manifest");')
changes[path] = value[:start] + block + value[end:]
append('src/persistent.rs', 'fn crash_point(', r'''

// Entirely absent from ordinary library, CLI, bindings and release builds.
#[cfg(test)]
pub(crate) fn crash_point(point: &str) {
    if std::env::var("PV23_UNIT_CRASH_ARMED").as_deref() != Ok("yes")
        || std::env::var("PV23_UNIT_CRASH_POINT").as_deref() != Ok(point) {
        return;
    }
    let signal = std::env::var("PV23_UNIT_CRASH_SIGNAL").expect("test child signal path");
    let mut file = std::fs::File::create(signal).expect("test child signal file");
    std::io::Write::write_all(&mut file, point.as_bytes()).expect("test child signal write");
    file.sync_all().expect("test child signal fsync");
    std::process::exit(88);
}
''')
append('src/db/persistent.rs', 'fn publication_crash_child()', r'''

#[cfg(all(test, feature = "full-text", feature = "vector-search", not(target_arch = "wasm32")))]
mod tests {
    use crate::Database;
    use std::process::Command;

    #[test]
    fn publication_crash_child() {
        let Ok(root) = std::env::var("PV23_UNIT_CRASH_ROOT") else { return; };
        let mut db = Database::open_dev(root).unwrap();
        db.begin_transaction().unwrap();
        std::env::set_var("PV23_UNIT_CRASH_ARMED", "yes");
        db.query("UPDATE docs SET body='changed document' WHERE id=1").unwrap();
        db.query("UPDATE docs SET embedding='[0,1]' WHERE id=1").unwrap();
        db.query("INSERT INTO docs VALUES (2,'changed inserted','[1,1]')").unwrap();
        db.commit_transaction().unwrap();
        panic!("selected publication crash point was not reached");
    }

    #[test]
    fn publication_boundaries_restore_one_complete_generation() {
        let temp = tempfile::tempdir().unwrap();
        for logged in [false, true] {
            for point in ["before_index_update", "during_encoding", "after_index_bytes", "before_manifest", "after_manifest"] {
                let root = temp.path().join(format!("{logged}-{point}"));
                let signal = temp.path().join(format!("signal-{logged}-{point}"));
                let mut db = Database::open_dev(&root).unwrap();
                db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
                db.query("INSERT INTO docs VALUES (1,'original document','[1,0]')").unwrap();
                if logged { db.enable_commit_log(crate::CommitLogOptions::default()).unwrap(); }
                db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
                db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
                let before = db.verification_hash().unwrap();
                drop(db);
                let output = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "db::persistent::tests::publication_crash_child", "--test-threads=1"])
                    .env("PV23_UNIT_CRASH_ROOT", &root).env("PV23_UNIT_CRASH_POINT", point)
                    .env("PV23_UNIT_CRASH_SIGNAL", &signal).output().unwrap();
                assert!(signal.is_file(), "{point} child did not reach crash: {} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                assert_eq!(output.status.code(), Some(88));
                let mut db = Database::open_dev(&root).unwrap();
                assert_eq!(db.verification_hash().unwrap(), before, "{logged} {point}");
                assert_eq!(db.row_count("docs", None).unwrap(), 1);
                for name in ["ft", "vx"] { db.verify_retrieval_index(name).unwrap(); }
                let mut request = serde_json::json!({"kind":"full_text","sql":"SELECT * FROM docs","id_column":"id","text_columns":["body"],"query":"original","limit":10});
                let reference = db.retrieve_json(&request.to_string()).unwrap();
                request["index"] = serde_json::json!("ft");
                assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), reference);
            }
        }
    }
}
''')

changes['tests/persistent_retrieval_model.rs'] = r'''// PicoVolt 2.3 qualification; see legal/COMPONENT-SCOPE-2.3.md.
#![cfg(all(feature = "full-text", feature = "vector-search"))]
use picovolt::{Database, Value};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

fn requests(sql: &str) -> Vec<Json> {
    vec![
        json!({"kind":"full_text","sql":sql,"id_column":"id","text_columns":["body"],"query":"apple","limit":100}),
        json!({"kind":"vector","sql":sql,"id_column":"id","vector_column":"embedding","query":[1,0],"metric":"cosine","limit":100}),
        json!({"kind":"hybrid","sql":sql,"id_column":"id","text_columns":["body"],"vector_column":"embedding","query":"apple","vector_query":[1,0],"metric":"cosine","text_weight":0.6,"candidate_limit":100,"limit":100}),
    ]
}

fn named(mut request: Json) -> Json {
    match request["kind"].as_str().unwrap() {
        "full_text" => request["index"] = json!("ft"),
        "vector" => request["index"] = json!("vx"),
        _ => { request["text_index"] = json!("ft"); request["vector_index"] = json!("vx"); },
    }
    request
}

#[test]
fn seeded_transaction_mutation_model_matches_rows_and_both_retrieval_oracles() {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE docs (id,tenant,body,embedding)").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
    db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    let mut model = BTreeMap::<i64, (String, String, String)>::new();
    let mut seed = 0x23_001_123_456u64;
    for step in 0..160 {
        let before = model.clone();
        db.begin_transaction().unwrap();
        for offset in 0..2 {
            seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
            let id = (seed % 24) as i64 - 12;
            let body = format!("apple {} step {step} mutation {offset}", if id % 2 == 0 { "apple 東京" } else { "banana Știință" });
            let vector = format!("[{},{}]", (seed % 5) + 1, (seed % 7) + 1);
            match (seed >> 12) % 4 {
                0 => {
                    db.query_with("DELETE FROM docs WHERE id=?", &[Value::Int(id)]).unwrap();
                    model.remove(&id);
                }
                1 if model.contains_key(&id) => {
                    db.query_with("UPDATE docs SET body=? WHERE id=?", &[Value::Text(body.clone()),Value::Int(id)]).unwrap();
                    model.get_mut(&id).unwrap().1 = body;
                }
                2 if model.contains_key(&id) => {
                    db.query_with("UPDATE docs SET embedding=? WHERE id=?", &[Value::Text(vector.clone()),Value::Int(id)]).unwrap();
                    model.get_mut(&id).unwrap().2 = vector;
                }
                _ if !model.contains_key(&id) => {
                    let tenant = if id % 2 == 0 { "a" } else { "b" }.to_owned();
                    db.query_with("INSERT INTO docs VALUES (?,?,?,?)", &[
                        Value::Int(id),Value::Text(tenant.clone()),Value::Text(body.clone()),Value::Text(vector.clone()),
                    ]).unwrap();
                    model.insert(id, (tenant, body, vector));
                }
                _ => {},
            }
        }
        if step % 11 == 0 { db.rollback_transaction().unwrap(); model = before; }
        else { db.commit_transaction().unwrap(); }
        if step % 7 == 0 { db = Database::import_bytes(&db.bake_to_bytes().unwrap()).unwrap(); }
        let rows = db.query("SELECT * FROM docs ORDER BY id").unwrap();
        let expected: Vec<_> = model.iter().map(|(id,(tenant,body,vector))| vec![
            Value::Int(*id),Value::Text(tenant.clone()),Value::Text(body.clone()),Value::Text(vector.clone()),
        ]).collect();
        assert_eq!(rows.rows().unwrap(), expected.as_slice(), "model rows at step {step}");
        for sql in ["SELECT * FROM docs", "SELECT * FROM docs WHERE tenant='a'", "SELECT * FROM docs WHERE tenant='b'"] {
            for request in requests(sql) {
                let reference = db.retrieve_json(&request.to_string()).unwrap();
                assert_eq!(db.retrieve_json(&named(request).to_string()).unwrap(), reference, "model ranks at step {step}");
            }
        }
        db.verify_retrieval_index("ft").unwrap(); db.verify_retrieval_index("vx").unwrap();
    }
}

#[test]
fn format_eight_golden_reopens_both_persisted_index_kinds() {
    let bytes = include_bytes!("fixtures/format_v8.pvdb");
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 8);
    let mut db = Database::import_bytes(bytes).unwrap();
    assert_eq!(db.retrieval_indexes().len(), 2);
    for index in db.retrieval_indexes() {
        assert_eq!(index.document_count, 3); assert!(!index.pending_write);
        db.verify_retrieval_index(&index.definition.name).unwrap();
    }
    let mut request = json!({"kind":"full_text","sql":"SELECT * FROM docs","id_column":"id","text_columns":["title","body"],"query":"verified snapshot","limit":10});
    let expected = db.retrieve_json(&request.to_string()).unwrap();
    request["index"] = json!("ft"); assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), expected);
    let mut request = json!({"kind":"vector","sql":"SELECT * FROM docs","id_column":"id","vector_column":"embedding","query":[1,1,1],"metric":"cosine","limit":10});
    let expected = db.retrieve_json(&request.to_string()).unwrap();
    request["index"] = json!("vx"); assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), expected);
}

#[test]
fn invalid_initial_corpora_fail_without_registering_partial_indexes() {
    for value in [Value::Null, Value::Text("not json".into()), Value::Text("[1]".into()), Value::Text("[0,0]".into()), Value::Text("[1e100,0]".into())] {
        let mut db = Database::open_memory(); db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
        db.insert("docs", vec![Value::Int(1),Value::Text("apple".into()),value]).unwrap();
        let before = db.verification_hash().unwrap();
        assert!(db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").is_err());
        assert!(db.retrieval_indexes().is_empty()); assert_eq!(db.verification_hash().unwrap(), before);
    }
    let mut db = Database::open_memory(); db.query("CREATE TABLE docs (id,body)").unwrap();
    db.query("INSERT INTO docs VALUES (1,'a'),(1,'b')").unwrap();
    assert!(db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").is_err());
    assert!(db.retrieval_indexes().is_empty());
    db.query("DELETE FROM docs WHERE id=1").unwrap();
    db.insert("docs", vec![Value::Text("1".into()),Value::Text("a".into())]).unwrap();
    assert!(db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").is_err());
}

#[test]
fn creation_and_indexed_mutation_enforce_document_budgets_atomically() {
    let mut db = Database::open_memory(); db.query("CREATE TABLE docs (id,body)").unwrap();
    db.transaction(|db| {
        for id in 0..=picovolt::persistent::MAX_RETRIEVAL_DOCUMENTS {
            db.insert("docs", vec![Value::Int(id as i64),Value::Text("apple".into())])?;
        }
        Ok(())
    }).unwrap();
    assert!(db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").is_err());
    assert!(db.retrieval_indexes().is_empty());
    db.query("DELETE FROM docs WHERE id=10000").unwrap();
    db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
    let before = db.verification_hash().unwrap();
    assert!(db.query("INSERT INTO docs VALUES (10000,'extra')").is_err());
    assert_eq!(db.verification_hash().unwrap(), before);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn retained_readers_keep_their_index_generation_across_later_commits() {
    use picovolt::{CancellationToken, QueryLimits, RequestOptions, SharedDatabase};
    let shared = SharedDatabase::open_memory().unwrap();
    shared.query("CREATE TABLE docs (id,tenant,body,embedding)").unwrap();
    shared.query("INSERT INTO docs VALUES (1,'a','apple old','[1,0]')").unwrap();
    shared.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')").unwrap();
    shared.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
    let mut old = shared.begin_read().unwrap();
    let inputs: Vec<_> = requests("SELECT * FROM docs").into_iter().map(named).collect();
    let old_results: Vec<_> = inputs.iter().map(|request| old.retrieve_json(&request.to_string()).unwrap()).collect();
    let mut writer = shared.begin_write().unwrap();
    writer.query("UPDATE docs SET body='banana new' WHERE id=1").unwrap();
    writer.query("UPDATE docs SET embedding='[0,1]' WHERE id=1").unwrap();
    writer.query("INSERT INTO docs VALUES (2,'a','apple new','[1,0]')").unwrap(); writer.commit().unwrap();
    let mut new = shared.begin_read().unwrap();
    for (request, expected) in inputs.iter().zip(&old_results) {
        assert_eq!(old.retrieve_json(&request.to_string()).unwrap(), *expected);
        assert_ne!(new.retrieve_json(&request.to_string()).unwrap(), *expected);
    }
    let mut future = inputs[0].clone(); future["sql"] = json!(format!("SELECT * FROM docs BEFORE {}", old.snapshot_tx()+1));
    assert!(old.retrieve_json(&future.to_string()).is_err());
    let limits = QueryLimits::new(0, 16 * 1024 * 1024, 10_000, None);
    let mut limited = shared.begin_read_with_options(RequestOptions::new(limits)).unwrap();
    assert!(limited.retrieve_json(&inputs[0].to_string()).is_err());
    let token = CancellationToken::new();
    let mut cancelled = shared.begin_read_with_options(RequestOptions::default().with_cancellation(token.clone())).unwrap();
    token.cancel(); assert!(cancelled.retrieve_json(&inputs[0].to_string()).is_err());
    assert_eq!(shared.retrieve_json(&inputs[0].to_string()).unwrap(), new.retrieve_json(&inputs[0].to_string()).unwrap());
}
'''

for path, content in changes.items():
    Path(path).write_text(content)
    print('PATCHED', path)

# Refresh only the fuzz harness lock graph after explicitly adding BLAKE3,
# already present transitively through the product dependency.
subprocess.run(['cargo', 'metadata', '--manifest-path', 'fuzz/Cargo.toml', '--format-version', '1'], stdout=subprocess.DEVNULL, check=True)
