// PicoVolt 2.3 persistent retrieval. See LICENSE and legal/COMPONENT-SCOPE-2.3.md.
//! Database integration for derived retrieval catalog objects.
use super::*;
use crate::persistent::{
    corrupt, validate_descriptors, CatalogEntry, Descriptor, RetrievalIndexDefinition,
    RetrievalIndexInfo, MAX_RETRIEVAL_CATALOG_BYTES, MAX_RETRIEVAL_DOCUMENTS,
    MAX_RETRIEVAL_INDEXES,
};

pub(super) fn validate_manifest(manifest: &Manifest) -> Result<()> {
    validate_descriptors(
        &manifest.retrieval_indexes,
        manifest.clock,
        manifest.cas_hashes.len(),
    )?;
    for descriptor in &manifest.retrieval_indexes {
        if !manifest.cas_dir.is_empty() {
            let id = usize::try_from(descriptor.cas_id).map_err(|_| corrupt("CAS ID overflow"))?;
            let (_, length) = manifest
                .cas_dir
                .get(id)
                .ok_or_else(|| corrupt("missing retrieval CAS extent"))?;
            if *length != descriptor.encoded_bytes as u64 {
                return Err(corrupt(format!(
                    "{} descriptor/CAS length mismatch",
                    descriptor.definition.name
                )));
            }
        }
    }
    Ok(())
}

/// Reject oversized retrieval blob files before the generic CAS loader reads
/// them into owned buffers. Hashes are validated before constructing paths.
pub(super) fn preflight_dev(root: &Path, manifest: &Manifest) -> Result<()> {
    for descriptor in &manifest.retrieval_indexes {
        let id = usize::try_from(descriptor.cas_id).map_err(|_| corrupt("CAS ID overflow"))?;
        let hash = manifest
            .cas_hashes
            .get(id)
            .ok_or_else(|| corrupt("missing retrieval CAS hash"))?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(corrupt("invalid retrieval CAS hash"));
        }
        let metadata = fs::symlink_metadata(root.join("blobs").join(&hash[..2]).join(hash))
            .map_err(|error| corrupt(format!("{}: {error}", descriptor.definition.name)))?;
        if !metadata.is_file() || metadata.len() != descriptor.encoded_bytes as u64 {
            return Err(corrupt(format!(
                "{} blob/descriptor size mismatch",
                descriptor.definition.name
            )));
        }
    }
    Ok(())
}

fn build_entry(
    cache: &mut PageCache,
    cas: &CasStore,
    tables: &BTreeMap<String, Table>,
    definition: RetrievalIndexDefinition,
    clock: u64,
) -> Result<CatalogEntry> {
    definition.validate()?;
    let table = tables
        .get(&definition.table)
        .ok_or_else(|| PvError::TableNotFound(definition.table.clone()))?;
    for column in std::iter::once(definition.id_column.as_str()).chain(definition.columns()) {
        column_index(table, column)?;
    }
    let mut entry = CatalogEntry::new(definition, clock)?;
    let snapshot = Snapshot::as_of(clock);
    let mut budget = QueryBudget::new_cancellable(
        QueryLimits::new(100_000, 16 * 1024 * 1024, MAX_RETRIEVAL_DOCUMENTS, None),
        None,
    );
    scan(cache, table, cas, |_, envelope, row| {
        budget.scan_row()?;
        if snapshot.sees(envelope) {
            entry.add_row(&table.columns, row)?;
        }
        Ok(())
    })?;
    budget.checkpoint()?;
    Ok(entry)
}

pub(super) fn load_catalog(
    cache: &mut PageCache,
    cas: &CasStore,
    tables: &BTreeMap<String, Table>,
    manifest: &Manifest,
) -> Result<BTreeMap<String, CatalogEntry>> {
    let mut catalog = BTreeMap::new();
    for descriptor in &manifest.retrieval_indexes {
        let bytes = cas
            .get(descriptor.cas_id)
            .map_err(|error| corrupt(error.to_string()))?;
        // Structural validation comes before authoritative-source verification.
        crate::persistent::validate_retrieval_index_bytes(bytes)?;
        let entry = build_entry(
            cache,
            cas,
            tables,
            descriptor.definition.clone(),
            manifest.clock,
        )
        .and_then(|entry| entry.verify_loaded(descriptor, bytes))
        .map_err(|error| corrupt(format!("{}: {error}", descriptor.definition.name)))?;
        catalog.insert(descriptor.definition.name.clone(), entry);
    }
    Ok(catalog)
}

impl Database {
    /// Create a named, bounded persistent retrieval index atomically. SQL DDL
    /// exposes the same operation. On a logged low-level handle, call this from
    /// an explicit transaction, just like existing low-level mutation methods.
    pub fn create_retrieval_index(&mut self, definition: RetrievalIndexDefinition) -> Result<()> {
        self.ensure_writable()?;
        definition.validate()?;
        if self.retrieval_indexes.contains_key(&definition.name) {
            return Err(PvError::Schema(format!(
                "retrieval index `{}` already exists",
                definition.name
            )));
        }
        if self.retrieval_indexes.len() >= MAX_RETRIEVAL_INDEXES {
            return Err(PvError::ResourceLimit(
                "at most 16 persistent retrieval indexes per database".into(),
            ));
        }
        let entry = build_entry(
            &mut self.cache.borrow_mut(),
            &self.cas,
            &self.tables,
            definition,
            self.current_tx(),
        )?;
        self.atomic_mutation(move |database| {
            database
                .retrieval_indexes
                .insert(entry.definition.name.clone(), entry);
            database.synced.set(false);
            database.maybe_flush()
        })
    }

    /// Drop a named retrieval index without changing source rows or MVCC history.
    /// Already written, unreachable CAS generations follow the existing CAS
    /// retention policy; dropping an index does not reclaim those bytes.
    pub fn drop_retrieval_index(&mut self, name: &str) -> Result<()> {
        self.ensure_writable()?;
        if !self.retrieval_indexes.contains_key(name) {
            return Err(PvError::Schema(format!(
                "retrieval index `{name}` does not exist"
            )));
        }
        self.atomic_mutation(|database| {
            database.retrieval_indexes.remove(name);
            database.synced.set(false);
            database.maybe_flush()
        })
    }

    /// Inspect current index definitions, size, health and generation. Pending
    /// state is visible only to the private writer, never another reader.
    pub fn retrieval_indexes(&self) -> Vec<RetrievalIndexInfo> {
        self.retrieval_indexes
            .values()
            .map(|entry| entry.info(self.current_tx()))
            .collect()
    }

    /// Compare a live named index to current authoritative rows without repairs.
    /// On-disk corruption prevents opening: restore a verified database/backup rather
    /// than bypassing open validation to invoke this method.
    pub fn verify_retrieval_index(&self, name: &str) -> Result<RetrievalIndexInfo> {
        let entry = self
            .retrieval_indexes
            .get(name)
            .ok_or_else(|| PvError::Schema(format!("retrieval index `{name}` does not exist")))?;
        let mut expected = build_entry(
            &mut self.cache.borrow_mut(),
            &self.cas,
            &self.tables,
            entry.definition.clone(),
            self.current_tx(),
        )?;
        expected.generation = entry.generation;
        if entry.encode()? != expected.encode()? {
            return Err(corrupt(format!(
                "{name} differs from its authoritative table"
            )));
        }
        Ok(entry.info(self.current_tx()))
    }

    /// Explicitly rebuild a healthy/open catalog object from current rows inside
    /// a transaction. No implicit repair occurs, and rebuild_count is observable.
    pub fn rebuild_retrieval_index(&mut self, name: &str) -> Result<()> {
        self.ensure_writable()?;
        let existing = self
            .retrieval_indexes
            .get(name)
            .ok_or_else(|| PvError::Schema(format!("retrieval index `{name}` does not exist")))?;
        let definition = existing.definition.clone();
        let count = existing
            .rebuild_count
            .checked_add(1)
            .ok_or_else(|| corrupt("rebuild counter overflow"))?;
        let mut entry = build_entry(
            &mut self.cache.borrow_mut(),
            &self.cas,
            &self.tables,
            definition,
            self.current_tx(),
        )?;
        entry.rebuild_count = count;
        self.atomic_mutation(move |database| {
            database
                .retrieval_indexes
                .insert(entry.definition.name.clone(), entry);
            database.synced.set(false);
            database.maybe_flush()
        })
    }

    pub(super) fn has_retrieval_indexes(&self, table: &str) -> bool {
        self.retrieval_indexes
            .values()
            .any(|entry| entry.definition.table == table)
    }

    pub(super) fn maintain_retrieval(
        &mut self,
        table: &str,
        removed: &[Row],
        added: &[Row],
    ) -> Result<()> {
        if !self.has_retrieval_indexes(table) {
            return Ok(());
        }
        #[cfg(test)]
        crate::persistent::crash_point("before_index_update");
        if !self.in_transaction() {
            return Err(corrupt(
                "retrieval maintenance requires a rollback boundary",
            ));
        }
        let source = self
            .tables
            .get(table)
            .ok_or_else(|| PvError::TableNotFound(table.into()))?;
        // Admission and reopen use the same version-scan bound; a successfully
        // committed mutation cannot create an index that its next open rejects.
        if source.row_versions > 100_000 {
            return Err(PvError::ResourceLimit(
                "indexed tables are limited to 100,000 retained row versions".into(),
            ));
        }
        let clock = self.current_tx();
        for entry in self
            .retrieval_indexes
            .values_mut()
            .filter(|entry| entry.definition.table == table)
        {
            entry.apply(&source.columns, removed, added, clock)?;
        }
        Ok(())
    }

    pub(super) fn persist_retrieval(&mut self) -> Result<()> {
        let mut pending = Vec::new();
        let mut total = 0usize;
        for (name, entry) in &self.retrieval_indexes {
            let length = if entry.dirty {
                let bytes = entry.encode()?;
                let length = bytes.len();
                pending.push((name.clone(), bytes));
                length
            } else {
                entry
                    .descriptor
                    .as_ref()
                    .ok_or_else(|| corrupt("missing published descriptor"))?
                    .encoded_bytes
            };
            total = total
                .checked_add(length)
                .ok_or_else(|| corrupt("catalog size overflow"))?;
            if total > MAX_RETRIEVAL_CATALOG_BYTES {
                return Err(PvError::ResourceLimit(
                    "active retrieval payloads exceed 64 MiB".into(),
                ));
            }
        }
        // All lengths are checked before installing descriptors. New CAS bytes
        // precede manifest publication and are covered by existing transaction
        // rollback and logged-commit new-blob capture, not an external sidecar.
        for (name, bytes) in pending {
            let cas_id = self.cas.put(&bytes)?;
            #[cfg(test)]
            crate::persistent::crash_point("after_index_bytes");
            let entry = self
                .retrieval_indexes
                .get_mut(&name)
                .ok_or_else(|| corrupt("catalog changed while encoding"))?;
            entry.descriptor = Some(Descriptor {
                definition: entry.definition.clone(),
                cas_id,
                encoded_bytes: bytes.len(),
                document_count: entry.len(),
                generation: entry.generation,
                rebuild_count: entry.rebuild_count,
            });
            entry.dirty = false;
        }
        Ok(())
    }

    pub(super) fn retrieval_descriptors(&self) -> Result<Vec<Descriptor>> {
        self.retrieval_indexes
            .values()
            .map(|entry| {
                if entry.dirty {
                    return Err(corrupt("manifest cannot publish unencoded retrieval state"));
                }
                entry
                    .descriptor
                    .clone()
                    .ok_or_else(|| corrupt("missing retrieval descriptor"))
            })
            .collect()
    }

    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub(crate) fn named_retrieval_entry(&self, name: &str) -> Result<&CatalogEntry> {
        self.retrieval_indexes
            .get(name)
            .ok_or_else(|| PvError::Query(format!("retrieval index `{name}` does not exist")))
    }
}

#[cfg(all(
    test,
    feature = "full-text",
    feature = "vector-search",
    not(target_arch = "wasm32")
))]
mod tests {
    use crate::Database;
    use std::process::Command;

    #[test]
    fn publication_crash_child() {
        let Ok(root) = std::env::var("PV23_UNIT_CRASH_ROOT") else {
            return;
        };
        let mut db = Database::open_dev(root).unwrap();
        db.begin_transaction().unwrap();
        std::env::set_var("PV23_UNIT_CRASH_ARMED", "yes");
        db.query("UPDATE docs SET body='changed document' WHERE id=1")
            .unwrap();
        db.query("UPDATE docs SET embedding='[0,1]' WHERE id=1")
            .unwrap();
        db.query("INSERT INTO docs VALUES (2,'changed inserted','[1,1]')")
            .unwrap();
        db.commit_transaction().unwrap();
        panic!("selected publication crash point was not reached");
    }

    #[test]
    fn publication_boundaries_restore_one_complete_generation() {
        let temp = tempfile::tempdir().unwrap();
        for logged in [false, true] {
            for point in [
                "before_index_update",
                "during_encoding",
                "after_index_bytes",
                "before_manifest",
                "after_manifest",
            ] {
                let root = temp.path().join(format!("{logged}-{point}"));
                let signal = temp.path().join(format!("signal-{logged}-{point}"));
                let mut db = Database::open_dev(&root).unwrap();
                db.query("CREATE TABLE docs (id,body,embedding)").unwrap();
                db.query("INSERT INTO docs VALUES (1,'original document','[1,0]')")
                    .unwrap();
                if logged {
                    db.enable_commit_log(crate::CommitLogOptions::default())
                        .unwrap();
                }
                db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
                    .unwrap();
                db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)").unwrap();
                let before = db.verification_hash().unwrap();
                drop(db);
                let output = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "db::persistent::tests::publication_crash_child",
                        "--test-threads=1",
                    ])
                    .env("PV23_UNIT_CRASH_ROOT", &root)
                    .env("PV23_UNIT_CRASH_POINT", point)
                    .env("PV23_UNIT_CRASH_SIGNAL", &signal)
                    .output()
                    .unwrap();
                assert!(
                    signal.is_file(),
                    "{point} child did not reach crash: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.status.code(), Some(88));
                let mut db = Database::open_dev(&root).unwrap();
                assert_eq!(db.verification_hash().unwrap(), before, "{logged} {point}");
                assert_eq!(db.row_count("docs", None).unwrap(), 1);
                for name in ["ft", "vx"] {
                    db.verify_retrieval_index(name).unwrap();
                }
                let mut request = serde_json::json!({"kind":"full_text","sql":"SELECT * FROM docs","id_column":"id","text_columns":["body"],"query":"original","limit":10});
                let reference = db.retrieve_json(&request.to_string()).unwrap();
                request["index"] = serde_json::json!("ft");
                assert_eq!(db.retrieve_json(&request.to_string()).unwrap(), reference);
            }
        }
    }
}
