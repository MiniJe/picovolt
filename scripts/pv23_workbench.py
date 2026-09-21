"""Temporary PV-2.3-M-001 hosted patch driver; not a product build dependency."""
from pathlib import Path
import re

changes = {}

def edit(path):
    return changes.get(path, Path(path).read_text())

def put(path, value):
    changes[path] = value

def one(text, old, new):
    if text.count(old) != 1:
        raise RuntimeError(f'Expected one exact patch anchor, found {text.count(old)}: {old[:160]!r}')
    return text.replace(old, new, 1)

s = edit('src/lib.rs')
if 'pub mod persistent;' not in s:
    s = one(s, 'pub mod journal;', 'pub mod journal;\n/// Named persistent retrieval catalog and bounded binary codecs.\npub mod persistent;')
    put('src/lib.rs', s)

s = edit('src/core/types.rs')
if 'FORMAT_VERSION_RETRIEVAL' not in s:
    s = one(s, 'pub const FORMAT_VERSION: u16 = 7;', '/// - Version 8: named, validated persistent retrieval descriptors and CAS payloads.\npub const FORMAT_VERSION: u16 = 8;\n\n/// Minimum format for named persistent retrieval catalog objects.\npub const FORMAT_VERSION_RETRIEVAL: u16 = 8;')
    put('src/core/types.rs', s)

s = edit('src/db.rs')
if '// PV23 storage integration' not in s:
    s = one(s, 'mod explain;', 'mod explain;\n// PV23 storage integration: derived CAS state shares the table commit boundary.\nmod persistent;')
    s = one(s, '    index_region: Option<(u64, u64)>,\n}', '    index_region: Option<(u64, u64)>,\n    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::persistent::descriptors")]\n    retrieval_indexes: Vec<crate::persistent::Descriptor>,\n}')
    s = one(s, 'fn check_manifest_version(m: &Manifest) -> Result<()> {', 'fn check_manifest_version(m: &Manifest) -> Result<()> {\n    persistent::validate_manifest(m)?;')
    s = one(s, '    required\n}\n\n#[derive(Serialize, Deserialize)]', '    if !manifest.retrieval_indexes.is_empty() {\n        required = required.max(crate::FORMAT_VERSION_RETRIEVAL);\n    }\n    required\n}\n\n#[derive(Serialize, Deserialize)]')
    s = one(s, '    tables: BTreeMap<String, Table>,\n    compliance:', '    tables: BTreeMap<String, Table>,\n    retrieval_indexes: BTreeMap<String, crate::persistent::CatalogEntry>,\n    compliance:')
    s = one(s, '            let cas = CasStore::load_dev(&root, &manifest.cas_hashes)?;', '            persistent::preflight_dev(&root, &manifest)?;\n            let cas = CasStore::load_dev(&root, &manifest.cas_hashes)?;')
    pattern = r'(?m)^( +)let tables = build_tables\([^\n]+\)\?;'
    if len(re.findall(pattern, s)) != 4:
        raise RuntimeError('Expected four database-loading constructors')
    s = re.sub(pattern, lambda m: m.group(0) + '\n' + m.group(1) + 'let retrieval_indexes = persistent::load_catalog(&mut cache, &cas, &tables, &manifest)?;', s)
    pattern = r'(?m)^( +)tables,\n( +)compliance:'
    if len(re.findall(pattern, s)) != 4:
        raise RuntimeError('Expected four loaded catalog assignments')
    s = re.sub(pattern, lambda m: m.group(1) + 'tables,\n' + m.group(1) + 'retrieval_indexes,\n' + m.group(2) + 'compliance:', s)
    pattern = r'(?m)^( +)tables: BTreeMap::new\(\),\n( +)compliance:'
    if len(re.findall(pattern, s)) != 2:
        raise RuntimeError('Expected two empty catalog constructors')
    s = re.sub(pattern, lambda m: m.group(1) + 'tables: BTreeMap::new(),\n' + m.group(1) + 'retrieval_indexes: BTreeMap::new(),\n' + m.group(2) + 'compliance:', s)
    s = one(s, '            Statement::CreateIndex {\n                table,', '''            Statement::CreateRetrievalIndex { definition, if_not_exists } => {
                if let Some(existing) = self.retrieval_indexes.get(&definition.name) {
                    if !if_not_exists || existing.definition != definition {
                        return Err(PvError::Schema("conflicting named retrieval index".into()));
                    }
                } else {
                    self.create_retrieval_index(definition)?;
                }
                Ok(QueryResult::Done)
            }
            Statement::DropRetrievalIndex { name, if_exists } => {
                if !if_exists || self.retrieval_indexes.contains_key(&name) {
                    self.drop_retrieval_index(&name)?;
                }
                Ok(QueryResult::Done)
            }
            Statement::CreateIndex {
                table,''')
    s = one(s, '    fn insert_validated(&mut self, table_name: &str, values: Vec<Value>) -> Result<()> {', '''    fn insert_validated(&mut self, table_name: &str, values: Vec<Value>) -> Result<()> {
        if !self.has_retrieval_indexes(table_name) {
            return self.insert_validated_base(table_name, values);
        }
        self.atomic_mutation(move |database| {
            database.insert_validated_base(table_name, values.clone())?;
            database.maintain_retrieval(table_name, &[], &[values])?;
            database.maybe_flush()
        })
    }

    fn insert_validated_base(&mut self, table_name: &str, values: Vec<Value>) -> Result<()> {''')
    s = one(s, '    fn apply_deletes(&mut self, table_name: &str, matches: &[(RecordAddr, Row)]) -> Result<usize> {', '''    fn apply_deletes(&mut self, table_name: &str, matches: &[(RecordAddr, Row)]) -> Result<usize> {
        if !self.has_retrieval_indexes(table_name) {
            return self.apply_deletes_base(table_name, matches);
        }
        self.atomic_mutation(|database| {
            let count = database.apply_deletes_base(table_name, matches)?;
            let removed: Vec<_> = matches.iter().map(|(_, row)| row.clone()).collect();
            database.maintain_retrieval(table_name, &removed, &[])?;
            database.maybe_flush()?;
            Ok(count)
        })
    }

    fn apply_deletes_base(&mut self, table_name: &str, matches: &[(RecordAddr, Row)]) -> Result<usize> {''')
    s = one(s, '''            for (_, mut row) in matches {
                row[set_ix] = set_value.clone();
                database.insert_validated(&table_name, row)?;
            }
            database.maybe_flush()?;''', '''            let indexed = database.has_retrieval_indexes(&table_name);
            let mut removed = Vec::new();
            let mut added = Vec::new();
            for (_, mut row) in matches {
                if indexed { removed.push(row.clone()); }
                row[set_ix] = set_value.clone();
                if indexed { added.push(row.clone()); }
                database.insert_validated_base(&table_name, row)?;
            }
            database.maintain_retrieval(&table_name, &removed, &added)?;
            database.maybe_flush()?;''')
    s = one(s, '    pub fn drop_table(&mut self, name: &str) -> Result<()> {', '''    pub fn drop_table(&mut self, name: &str) -> Result<()> {
        if self.has_retrieval_indexes(name) {
            return self.atomic_mutation(|database| database.drop_table_base(name));
        }
        self.drop_table_base(name)
    }

    fn drop_table_base(&mut self, name: &str) -> Result<()> {''')
    s = one(s, '''        if self.tables.remove(name).is_none() {
            return Err(PvError::TableNotFound(name.into()));
        }
        self.maybe_flush()''', '''        if self.tables.remove(name).is_none() {
            return Err(PvError::TableNotFound(name.into()));
        }
        self.retrieval_indexes.retain(|_, entry| entry.definition.table != name);
        self.maybe_flush()''')
    s = one(s, '''        // fsync only makes sense for a filesystem-backed (dev) database.
        let durable''', '''        // Encode before page/manifest publication; a private transaction owns
        // every indexed mutation and restores both catalogs on failure.
        self.persist_retrieval()?;
        // fsync only makes sense for a filesystem-backed (dev) database.
        let durable''')
    s = one(s, '        schema_version = schema_version.max(self.format_version_floor);', '        schema_version = schema_version.max(self.retrieval_format_floor());')
    s = one(s, '            index_region,\n        })\n    }\n}', '            index_region,\n            retrieval_indexes: self.retrieval_descriptors()?,\n        })\n    }\n\n    fn retrieval_format_floor(&self) -> u16 {\n        self.format_version_floor.max(if self.retrieval_indexes.is_empty() { FORMAT_VERSION_BASE } else { crate::FORMAT_VERSION_RETRIEVAL })\n    }\n}')
    s = re.sub(r'effective_format_version\(&self.tables,\s*self.format_version_floor\)', 'effective_format_version(&self.tables, self.retrieval_format_floor())', s)
    s = one(s, '    pub tables: Vec<TableStats>,\n}', '    pub tables: Vec<TableStats>,\n    /// Named retrieval index health and last persisted generation.\n    pub retrieval_indexes: Vec<crate::persistent::RetrievalIndexInfo>,\n}')
    # The inventory constructor is identified structurally to avoid touching the manifest.
    start = s.index('Ok(DatabaseStats {')
    end = s.index('\n        })', start)
    block = s[start:end]
    block = one(block, '            tables,', '            tables,\n            retrieval_indexes: self.retrieval_indexes(),')
    s = s[:start] + block + s[end:]
    s = one(s, '        Ok(hasher.finalize().to_hex().to_string())', '''        for entry in self.retrieval_indexes.values() {
            hasher.update(b"persistent-retrieval-v1\\0");
            hash_len_prefixed(&mut hasher, &entry.encode()?);
        }
        Ok(hasher.finalize().to_hex().to_string())''')
    # Nested mutation helpers may already have performed the one full rollback.
    start = s.index('    fn atomic_mutation<')
    end = s.index('\n    fn ', start + 8)
    block = s[start:end]
    anchor = '                self.rollback_transaction().map_err(|rollback_error| {'
    if anchor in block:
        begin = block.index(anchor)
        finish = block.index('                })?;', begin) + len('                })?;')
        block = block[:begin] + '                if self.in_transaction() {\n' + block[begin:finish] + '\n                }' + block[finish:]
    s = s[:start] + block + s[end:]
    put('src/db.rs', s)

s = edit('src/db/persistent.rs')
s = s.replace('QueryBudget::new(QueryLimits::new(100_000, 16 * 1024 * 1024, MAX_RETRIEVAL_DOCUMENTS, None))', 'QueryBudget::new_cancellable(QueryLimits::new(100_000, 16 * 1024 * 1024, MAX_RETRIEVAL_DOCUMENTS, None), None)')
start = s.index('        let expected = build_entry(', s.index('pub fn verify_retrieval_index'))
end = s.index('        expected.generation = entry.generation;', start)
s = s[:start] + '        let mut expected = build_entry(&mut self.cache.borrow_mut(), &self.cas, &self.tables, entry.definition.clone(), self.current_tx())?;\n' + s[end:]
s = s.replace('On-disk corruption is fail-open:', 'On-disk corruption prevents opening:')
put('src/db/persistent.rs', s)

s = edit('src/persistent.rs')
s = s.replace('    pub(crate) fn contains(&self, id: i64)', '    #[cfg(any(feature = "full-text", feature = "vector-search"))]\n    pub(crate) fn contains(&self, id: i64)') if '#[cfg(any(feature = "full-text", feature = "vector-search"))]\n    pub(crate) fn contains' not in s else s
put('src/persistent.rs', s)

s = edit('src/engine/query.rs')
if '// PV23 named retrieval grammar' not in s:
    s = one(s, '    CreateIndex {', '''    // PV23 named retrieval grammar; legacy anonymous secondary indexes remain.
    CreateRetrievalIndex {
        definition: crate::persistent::RetrievalIndexDefinition,
        if_not_exists: bool,
    },
    DropRetrievalIndex { name: String, if_exists: bool },
    CreateIndex {''')
    s = one(s, '''        Tok::Word(w) if w.eq_ignore_ascii_case("index") => {
            cur.keyword("on")?;''', '''        Tok::Word(w) if w.eq_ignore_ascii_case("index") => {
            if !peek_kw(cur, "on") { return parse_retrieval_index(cur); }
            cur.keyword("on")?;''')
    s = one(s, 'fn parse_drop(cur: &mut Cursor) -> Result<Statement> {\n    cur.keyword("table")?;', '''fn parse_drop(cur: &mut Cursor) -> Result<Statement> {
    if peek_kw(cur, "index") {
        cur.next()?;
        let if_exists = if peek_kw(cur, "if") { cur.next()?; cur.keyword("exists")?; true } else { false };
        return Ok(Statement::DropRetrievalIndex { name: cur.ident()?, if_exists });
    }
    cur.keyword("table")?;''')
    s += r'''

fn parse_retrieval_index(cur: &mut Cursor) -> Result<Statement> {
    use crate::persistent::{RetrievalIndexDefinition, RetrievalIndexKind, RetrievalMetric};
    let at = cur.here();
    let if_not_exists = if peek_kw(cur, "if") {
        cur.next()?; cur.keyword("not")?; cur.keyword("exists")?; true
    } else { false };
    let name = cur.ident()?;
    cur.keyword("on")?;
    let table = cur.ident()?;
    cur.keyword("using")?;
    let kind_at = cur.here();
    let kind = cur.ident()?;
    if !kind.eq_ignore_ascii_case("fulltext") && !kind.eq_ignore_ascii_case("vector") {
        return Err(cur.err_at(kind_at, "expected FULLTEXT or VECTOR index kind"));
    }
    cur.expect(Tok::LParen)?;
    let mut columns = Vec::new();
    loop {
        if columns.len() >= 16 { return Err(cur.err("at most 16 retrieval columns")); }
        columns.push(cur.ident()?);
        match cur.next()? {
            Tok::RParen => break,
            Tok::Comma => {},
            other => return Err(cur.err(format!("expected `,` or `)`, found {other:?}"))),
        }
    }
    cur.keyword("with")?;
    cur.expect(Tok::LParen)?;
    let mut options = std::collections::BTreeMap::new();
    loop {
        let option_at = cur.here();
        let key = cur.ident()?.to_ascii_lowercase();
        if !matches!(key.as_str(), "id_column" | "metric" | "dimensions") {
            return Err(cur.err_at(option_at, format!("unknown retrieval option `{key}`")));
        }
        cur.expect(Tok::Eq)?;
        let value = cur.value()?;
        if options.insert(key.clone(), value).is_some() {
            return Err(cur.err_at(option_at, format!("duplicate retrieval option `{key}`")));
        }
        match cur.next()? {
            Tok::RParen => break,
            Tok::Comma => {},
            other => return Err(cur.err(format!("expected `,` or `)`, found {other:?}"))),
        }
    }
    let Some(Value::Text(id_column)) = options.remove("id_column") else {
        return Err(cur.err_at(at, "id_column must be an explicit string option"));
    };
    let index = if kind.eq_ignore_ascii_case("fulltext") {
        if !options.is_empty() { return Err(cur.err_at(at, "FULLTEXT only accepts id_column")); }
        RetrievalIndexKind::FullText { text_columns: columns }
    } else {
        if columns.len() != 1 { return Err(cur.err_at(at, "VECTOR requires exactly one column")); }
        let metric = match options.remove("metric") {
            Some(Value::Text(value)) if value == "cosine" => RetrievalMetric::Cosine,
            Some(Value::Text(value)) if value == "squared_euclidean" => RetrievalMetric::SquaredEuclidean,
            _ => return Err(cur.err_at(at, "metric must be 'cosine' or 'squared_euclidean'")),
        };
        let dimensions = match options.remove("dimensions") {
            Some(Value::Int(value)) if (1..=4096).contains(&value) => value as usize,
            _ => return Err(cur.err_at(at, "dimensions must be an integer in 1–4096")),
        };
        RetrievalIndexKind::Vector { vector_column: columns.remove(0), metric, dimensions }
    };
    let definition = RetrievalIndexDefinition { name, table, id_column, index };
    definition.validate().map_err(|error| cur.err_at(at, error.to_string()))?;
    Ok(Statement::CreateRetrievalIndex { definition, if_not_exists })
}
'''
    put('src/engine/query.rs', s)

s = edit('src/search.rs')
if '// PV23 binary codec' not in s:
    s += r'''

// PV23 binary codec and filtered-corpus execution. Legacy public search is unchanged.
impl SearchIndex {
    pub(crate) fn search_filtered(&self, query: &str, limit: usize, ids: &BTreeSet<i64>) -> Result<Vec<SearchHit>, SearchError> {
        if ids.len() > MAX_DOCUMENTS || ids.iter().any(|id| !self.documents.contains_key(id)) {
            return Err(SearchError::InvalidRows);
        }
        if ids.len() == self.len() { return self.search(query, limit); }
        if query.len() > 1024 || limit > 100 { return Err(SearchError::Limit); }
        let terms: BTreeSet<_> = tokens(query).into_iter().collect();
        if terms.len() > 32 { return Err(SearchError::Limit); }
        let total_tokens: usize = ids.iter().map(|id| self.documents[id].length).sum();
        if terms.is_empty() || limit == 0 || total_tokens == 0 { return Ok(Vec::new()); }
        let mut lists = Vec::new();
        for term in terms {
            let Some(posting) = self.postings.get(&term) else { return Ok(Vec::new()); };
            let frequency = posting.keys().filter(|id| ids.contains(id)).count();
            if frequency == 0 { return Ok(Vec::new()); }
            lists.push((posting, frequency));
        }
        // Match the reference's stable frequency ordering, but over the SELECT
        // corpus, not global tenants. This also preserves floating addition order.
        lists.sort_by_key(|(_, frequency)| *frequency);
        let n = ids.len() as f64;
        let average = total_tokens as f64 / n;
        let mut hits = Vec::new();
        for id in lists[0].0.keys().filter(|id| ids.contains(id)) {
            let mut score = 0.0;
            let mut matches = true;
            for (posting, frequency) in &lists {
                let Some(tf) = posting.get(id) else { matches = false; break; };
                let df = *frequency as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let tf = f64::from(*tf);
                let length = self.documents[id].length as f64;
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length / average));
            }
            if matches { hits.push(SearchHit { id: *id, score }); }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        Ok(hits)
    }

    pub(crate) fn encode_persistent(&self) -> crate::Result<Vec<u8>> {
        use crate::persistent::Encoder;
        let entries: usize = self.documents.values().map(|document| document.terms.len()).sum();
        if entries > 1_048_576 { return Err(crate::PvError::ResourceLimit("persistent text term-entry limit exceeded".into())); }
        let mut out = Encoder::new();
        out.count(self.len())?; out.count(self.postings.len())?;
        out.count(self.total_tokens)?; out.count(self.total_bytes)?;
        for (id, document) in &self.documents {
            out.put(&id.to_le_bytes())?;
            out.count(document.length)?; out.count(document.bytes)?; out.count(document.terms.len())?;
            for (term, frequency) in &document.terms {
                out.count(term.len())?; out.put(term.as_bytes())?; out.put(&frequency.to_le_bytes())?;
            }
        }
        Ok(out.finish())
    }

    pub(crate) fn decode_persistent(bytes: &[u8]) -> crate::Result<Self> {
        use crate::persistent::{corrupt, Decoder};
        let mut input = Decoder::new(bytes)?;
        let count = input.count(MAX_DOCUMENTS)?;
        let expected_terms = input.count(MAX_TERMS)?;
        let expected_tokens = input.count(MAX_TOTAL_BYTES)?;
        let expected_bytes = input.count(MAX_TOTAL_BYTES)?;
        let mut index = Self::new();
        let mut last_id = None;
        let mut entries = 0usize;
        for _ in 0..count {
            let id = input.i64()?;
            if last_id.is_some_and(|previous| previous >= id) { return Err(corrupt("text IDs are not strictly ordered")); }
            last_id = Some(id);
            let length = input.count(MAX_DOCUMENT_BYTES)?;
            let source_bytes = input.count(MAX_DOCUMENT_BYTES)?;
            let count = input.count(4096)?;
            entries = entries.checked_add(count).ok_or_else(|| corrupt("text term count overflow"))?;
            if entries > 1_048_576 || length > source_bytes { return Err(corrupt("text document budget or length mismatch")); }
            let mut terms = BTreeMap::new();
            let mut last_term = String::new();
            let mut total = 0usize;
            for _ in 0..count {
                let len = input.count(384)?;
                if len == 0 { return Err(corrupt("empty encoded token")); }
                let term = std::str::from_utf8(input.take(len)?).map_err(|_| corrupt("token is not UTF-8"))?;
                if term <= last_term.as_str() { return Err(corrupt("tokens are not strictly ordered")); }
                let frequency = input.count(MAX_DOCUMENT_BYTES)?;
                total = total.checked_add(frequency).ok_or_else(|| corrupt("token frequency overflow"))?;
                if frequency == 0 || total > length { return Err(corrupt("invalid token frequency")); }
                if !index.postings.contains_key(term) && index.postings.len() >= MAX_TERMS { return Err(corrupt("corpus term limit")); }
                index.postings.entry(term.to_owned()).or_default().insert(id, frequency as u32);
                terms.insert(term.to_owned(), frequency as u32);
                last_term = term.to_owned();
            }
            if total != length { return Err(corrupt("document token total mismatch")); }
            index.total_tokens = index.total_tokens.checked_add(length).ok_or_else(|| corrupt("token total overflow"))?;
            index.total_bytes = index.total_bytes.checked_add(source_bytes).ok_or_else(|| corrupt("source total overflow"))?;
            if index.total_tokens > expected_tokens || index.total_bytes > expected_bytes { return Err(corrupt("text corpus total mismatch")); }
            index.documents.insert(id, Document { terms, length, bytes: source_bytes });
        }
        input.finish()?;
        if index.postings.len() != expected_terms || index.total_tokens != expected_tokens || index.total_bytes != expected_bytes {
            return Err(corrupt("text corpus metadata mismatch"));
        }
        Ok(index)
    }
}
'''
    put('src/search.rs', s)

s = edit('src/vector.rs')
if '// PV23 binary codec' not in s:
    s = one(s, '    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<VectorHit>, VectorError> {', '''    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<VectorHit>, VectorError> {
        self.search_matching(query, limit, None)
    }
    fn search_matching(&self, query: &[f32], limit: usize, allowed: Option<&std::collections::BTreeSet<i64>>) -> Result<Vec<VectorHit>, VectorError> {''')
    s = one(s, '''            .documents
            .iter()
            .map(|(id, vector)| {''', '''            .documents
            .iter()
            .filter(|(id, _)| allowed.map_or(true, |ids| ids.contains(id)))
            .map(|(id, vector)| {''')
    s += r'''

// PV23 binary codec and exact filtered candidate execution.
impl VectorIndex {
    pub(crate) fn search_filtered(&self, query: &[f32], limit: usize, ids: &std::collections::BTreeSet<i64>) -> Result<Vec<VectorHit>, VectorError> {
        if ids.len() > 10_000 || ids.iter().any(|id| !self.documents.contains_key(id)) { return Err(VectorError::InvalidVector); }
        self.search_matching(query, limit, Some(ids))
    }
    pub(crate) fn encode_persistent(&self) -> crate::Result<Vec<u8>> {
        let mut out = crate::persistent::Encoder::new();
        out.count(self.dimensions)?; out.count(self.len())?;
        let metric = match self.metric { Metric::Cosine => 1, Metric::SquaredEuclidean => 2 };
        out.put(&[metric, 0, 0, 0])?;
        for (id, vector) in &self.documents {
            out.put(&id.to_le_bytes())?;
            for value in vector { out.put(&value.to_le_bytes())?; }
        }
        Ok(out.finish())
    }
    pub(crate) fn decode_persistent(bytes: &[u8], dimensions: usize, metric: Metric) -> crate::Result<Self> {
        use crate::persistent::{corrupt, Decoder};
        let mut input = Decoder::new(bytes)?;
        let actual_dimensions = input.count(4096)?;
        let count = input.count(10_000)?;
        let actual_metric = input.u8()?;
        let expected_metric = match metric { Metric::Cosine => 1, Metric::SquaredEuclidean => 2 };
        if actual_dimensions == 0 || actual_dimensions != dimensions || actual_metric != expected_metric || input.take(3)? != [0, 0, 0] {
            return Err(corrupt("vector header/definition mismatch"));
        }
        let scalars = count.checked_mul(dimensions).filter(|count| *count <= 4_194_304).ok_or_else(|| corrupt("vector scalar budget"))?;
        let expected_bytes = scalars.checked_mul(4).and_then(|value| value.checked_add(count * 8)).and_then(|value| value.checked_add(12)).ok_or_else(|| corrupt("vector extent overflow"))?;
        if expected_bytes != bytes.len() { return Err(corrupt("vector extent/count mismatch")); }
        let mut index = Self::new(dimensions, metric).map_err(|error| corrupt(error.to_string()))?;
        let mut last_id = None;
        for _ in 0..count {
            let id = input.i64()?;
            if last_id.is_some_and(|previous| previous >= id) { return Err(corrupt("vector IDs are not strictly ordered")); }
            last_id = Some(id);
            let mut vector = Vec::with_capacity(dimensions);
            for _ in 0..dimensions { vector.push(f32::from_le_bytes(input.take(4)?.try_into().map_err(|_| corrupt("vector scalar"))?)); }
            index.upsert(id, &vector).map_err(|error| corrupt(error.to_string()))?;
        }
        input.finish()?;
        Ok(index)
    }
}
'''
    put('src/vector.rs', s)

# Write only after all exact baseline anchors have been checked.
for path, content in changes.items():
    Path(path).write_text(content)
    print('PATCHED', path)
