//! Application-owned full-text indexes over a consistent PicoVolt query result.
//!
//! Unicode alphanumeric tokens are lowercased; there is no stemming, accent
//! folding, phrase search, SQL MATCH syntax or automatic transaction tracking.
//! Queries require all distinct terms and rank with BM25 (k1=1.2, b=0.75).
//! Rebuild from a database snapshot after commits, then replace the old index.
//! Limits bound input and cardinality, not an exact allocator memory budget.

use crate::{QueryResult, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DOCUMENTS: usize = 10_000;
const MAX_DOCUMENT_BYTES: usize = 65_536;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_TERMS: usize = 65_536;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("full-text input exceeds its configured bounds")]
    Limit,
    #[error("expected unique integer document IDs and text columns")]
    InvalidRows,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: i64,
    pub score: f64,
}

#[derive(Debug, Clone)]
struct Document {
    terms: BTreeMap<String, u32>,
    length: usize,
    bytes: usize,
}

/// A bounded in-memory inverted index. It never mutates the source database.
#[derive(Debug, Default)]
pub struct SearchIndex {
    documents: BTreeMap<i64, Document>,
    postings: BTreeMap<String, BTreeMap<i64, u32>>,
    total_tokens: usize,
    total_bytes: usize,
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty() && word.len() <= 128)
        .map(str::to_lowercase)
        .collect()
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.documents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Insert or replace one document. Validation failure preserves the old one.
    pub fn upsert(&mut self, id: i64, text: &str) -> Result<(), SearchError> {
        let previous_bytes = self.documents.get(&id).map_or(0, |d| d.bytes);
        if text.len() > MAX_DOCUMENT_BYTES
            || self.total_bytes - previous_bytes + text.len() > MAX_TOTAL_BYTES
            || (!self.documents.contains_key(&id) && self.len() >= MAX_DOCUMENTS)
        {
            return Err(SearchError::Limit);
        }
        let words = tokens(text);
        let mut terms = BTreeMap::<String, u32>::new();
        for word in &words {
            *terms.entry(word.clone()).or_default() += 1;
        }
        if terms.len() > 4096
            || self.postings.len()
                + terms
                    .keys()
                    .filter(|t| !self.postings.contains_key(*t))
                    .count()
                > MAX_TERMS
        {
            return Err(SearchError::Limit);
        }
        self.remove(id);
        for (term, frequency) in &terms {
            self.postings
                .entry(term.clone())
                .or_default()
                .insert(id, *frequency);
        }
        self.total_tokens += words.len();
        self.total_bytes += text.len();
        self.documents.insert(
            id,
            Document {
                terms,
                length: words.len(),
                bytes: text.len(),
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, id: i64) -> bool {
        let Some(document) = self.documents.remove(&id) else {
            return false;
        };
        self.total_tokens -= document.length;
        self.total_bytes -= document.bytes;
        for term in document.terms.keys() {
            if let Some(posting) = self.postings.get_mut(term) {
                posting.remove(&id);
                if posting.is_empty() {
                    self.postings.remove(term);
                }
            }
        }
        true
    }

    /// All-term matching, deterministic ranking; equal scores use ascending IDs.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        if query.len() > 1024 || limit > 100 {
            return Err(SearchError::Limit);
        }
        let terms: BTreeSet<_> = tokens(query).into_iter().collect();
        if terms.len() > 32 {
            return Err(SearchError::Limit);
        }
        if terms.is_empty() || limit == 0 || self.total_tokens == 0 {
            return Ok(vec![]);
        }
        let mut lists = Vec::new();
        for term in terms {
            let Some(posting) = self.postings.get(&term) else {
                return Ok(vec![]);
            };
            lists.push(posting);
        }
        lists.sort_by_key(|posting| posting.len());
        let n = self.len() as f64;
        let average = self.total_tokens as f64 / n;
        let mut hits = Vec::new();
        for id in lists[0].keys() {
            let mut score = 0.0;
            let mut matches = true;
            for posting in &lists {
                let Some(frequency) = posting.get(id) else {
                    matches = false;
                    break;
                };
                let df = posting.len() as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let tf = f64::from(*frequency);
                let length = self.documents[id].length as f64;
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length / average));
            }
            if matches {
                hits.push(SearchHit { id: *id, score });
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        Ok(hits)
    }

    /// Build from one SELECT result, using explicit column names.
    pub fn from_rows(
        result: &QueryResult,
        id_column: &str,
        text_columns: &[&str],
    ) -> Result<Self, SearchError> {
        let QueryResult::Rows { columns, rows } = result else {
            return Err(SearchError::InvalidRows);
        };
        if rows.len() > MAX_DOCUMENTS || text_columns.is_empty() || text_columns.len() > 16 {
            return Err(SearchError::Limit);
        }
        let id_index = columns
            .iter()
            .position(|c| c == id_column)
            .ok_or(SearchError::InvalidRows)?;
        let positions: Vec<_> = text_columns
            .iter()
            .map(|name| {
                columns
                    .iter()
                    .position(|c| c == name)
                    .ok_or(SearchError::InvalidRows)
            })
            .collect::<Result<_, _>>()?;
        let mut index = Self::new();
        for row in rows {
            let Some(Value::Int(id)) = row.get(id_index) else {
                return Err(SearchError::InvalidRows);
            };
            if index.documents.contains_key(id) {
                return Err(SearchError::InvalidRows);
            }
            let mut text = String::new();
            for position in &positions {
                let field = match row.get(*position) {
                    Some(Value::Text(value)) => value.as_str(),
                    Some(Value::Null) => "",
                    _ => return Err(SearchError::InvalidRows),
                };
                if text.len() + field.len() + 1 > MAX_DOCUMENT_BYTES {
                    return Err(SearchError::Limit);
                }
                text.push_str(field);
                text.push(' ');
            }
            index.upsert(*id, &text)?;
        }
        Ok(index)
    }
}

// PV23 binary codec and filtered-corpus execution. Legacy public search is unchanged.
impl SearchIndex {
    pub(crate) fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        ids: &BTreeSet<i64>,
    ) -> Result<Vec<SearchHit>, SearchError> {
        if ids.len() > MAX_DOCUMENTS || ids.iter().any(|id| !self.documents.contains_key(id)) {
            return Err(SearchError::InvalidRows);
        }
        if ids.len() == self.len() {
            return self.search(query, limit);
        }
        if query.len() > 1024 || limit > 100 {
            return Err(SearchError::Limit);
        }
        let terms: BTreeSet<_> = tokens(query).into_iter().collect();
        if terms.len() > 32 {
            return Err(SearchError::Limit);
        }
        let total_tokens: usize = ids.iter().map(|id| self.documents[id].length).sum();
        if terms.is_empty() || limit == 0 || total_tokens == 0 {
            return Ok(Vec::new());
        }
        let mut lists = Vec::new();
        for term in terms {
            let Some(posting) = self.postings.get(&term) else {
                return Ok(Vec::new());
            };
            let frequency = posting.keys().filter(|id| ids.contains(id)).count();
            if frequency == 0 {
                return Ok(Vec::new());
            }
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
                let Some(tf) = posting.get(id) else {
                    matches = false;
                    break;
                };
                let df = *frequency as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let tf = f64::from(*tf);
                let length = self.documents[id].length as f64;
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length / average));
            }
            if matches {
                hits.push(SearchHit { id: *id, score });
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        Ok(hits)
    }

    pub(crate) fn encode_persistent(&self) -> crate::Result<Vec<u8>> {
        use crate::persistent::Encoder;
        let entries: usize = self
            .documents
            .values()
            .map(|document| document.terms.len())
            .sum();
        if entries > 1_048_576 {
            return Err(crate::PvError::ResourceLimit(
                "persistent text term-entry limit exceeded".into(),
            ));
        }
        let mut out = Encoder::new();
        out.count(self.len())?;
        out.count(self.postings.len())?;
        out.count(self.total_tokens)?;
        out.count(self.total_bytes)?;
        for (id, document) in &self.documents {
            out.put(&id.to_le_bytes())?;
            out.count(document.length)?;
            out.count(document.bytes)?;
            out.count(document.terms.len())?;
            for (term, frequency) in &document.terms {
                out.count(term.len())?;
                out.put(term.as_bytes())?;
                out.put(&frequency.to_le_bytes())?;
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
            if last_id.is_some_and(|previous| previous >= id) {
                return Err(corrupt("text IDs are not strictly ordered"));
            }
            last_id = Some(id);
            let length = input.count(MAX_DOCUMENT_BYTES)?;
            let source_bytes = input.count(MAX_DOCUMENT_BYTES)?;
            let count = input.count(4096)?;
            entries = entries
                .checked_add(count)
                .ok_or_else(|| corrupt("text term count overflow"))?;
            if entries > 1_048_576 || length > source_bytes {
                return Err(corrupt("text document budget or length mismatch"));
            }
            let mut terms = BTreeMap::new();
            let mut last_term = String::new();
            let mut total = 0usize;
            for _ in 0..count {
                let len = input.count(384)?;
                if len == 0 {
                    return Err(corrupt("empty encoded token"));
                }
                let term = std::str::from_utf8(input.take(len)?)
                    .map_err(|_| corrupt("token is not UTF-8"))?;
                if term <= last_term.as_str() {
                    return Err(corrupt("tokens are not strictly ordered"));
                }
                let frequency = input.count(MAX_DOCUMENT_BYTES)?;
                total = total
                    .checked_add(frequency)
                    .ok_or_else(|| corrupt("token frequency overflow"))?;
                if frequency == 0 || total > length {
                    return Err(corrupt("invalid token frequency"));
                }
                if !index.postings.contains_key(term) && index.postings.len() >= MAX_TERMS {
                    return Err(corrupt("corpus term limit"));
                }
                index
                    .postings
                    .entry(term.to_owned())
                    .or_default()
                    .insert(id, frequency as u32);
                terms.insert(term.to_owned(), frequency as u32);
                last_term = term.to_owned();
            }
            if total != length {
                return Err(corrupt("document token total mismatch"));
            }
            index.total_tokens = index
                .total_tokens
                .checked_add(length)
                .ok_or_else(|| corrupt("token total overflow"))?;
            index.total_bytes = index
                .total_bytes
                .checked_add(source_bytes)
                .ok_or_else(|| corrupt("source total overflow"))?;
            if index.total_tokens > expected_tokens || index.total_bytes > expected_bytes {
                return Err(corrupt("text corpus total mismatch"));
            }
            index.documents.insert(
                id,
                Document {
                    terms,
                    length,
                    bytes: source_bytes,
                },
            );
        }
        input.finish()?;
        if index.postings.len() != expected_terms
            || index.total_tokens != expected_tokens
            || index.total_bytes != expected_bytes
        {
            return Err(corrupt("text corpus metadata mismatch"));
        }
        Ok(index)
    }
}
