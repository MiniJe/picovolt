// PicoVolt 2.3 persistent retrieval. See LICENSE and legal/COMPONENT-SCOPE-2.3.md.
//! Named, optional retrieval indexes. Source rows, not these derived bytes, are
//! authoritative. The binary representation is specified in `docs/FORMAT.md`.

use crate::{PvError, Result, Row, Value};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum named retrieval indexes in one database.
pub const MAX_RETRIEVAL_INDEXES: usize = 16;
/// Maximum bytes in one complete encoded index envelope.
pub const MAX_RETRIEVAL_INDEX_BYTES: usize = 32 * 1024 * 1024;
/// Maximum bytes in the currently referenced retrieval envelopes together.
pub const MAX_RETRIEVAL_CATALOG_BYTES: usize = 64 * 1024 * 1024;
/// Maximum documents per persistent index and materialized filter-ID set.
pub const MAX_RETRIEVAL_DOCUMENTS: usize = 10_000;
/// Maximum source bytes for a vector index; full text retains its 8 MiB cap.
pub const MAX_RETRIEVAL_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEFINITION_BYTES: usize = 8192;
const MAGIC: &[u8; 8] = b"PVRIDX\0\0";

/// Exact distance metric. No approximate search or model provider is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMetric {
    Cosine,
    SquaredEuclidean,
}

#[cfg(feature = "vector-search")]
impl From<RetrievalMetric> for crate::vector::Metric {
    fn from(metric: RetrievalMetric) -> Self {
        match metric {
            RetrievalMetric::Cosine => Self::Cosine,
            RetrievalMetric::SquaredEuclidean => Self::SquaredEuclidean,
        }
    }
}

/// The indexed columns and immutable index options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetrievalIndexKind {
    FullText {
        #[serde(deserialize_with = "column_names")]
        text_columns: Vec<String>,
    },
    Vector {
        #[serde(deserialize_with = "identifier")]
        vector_column: String,
        metric: RetrievalMetric,
        dimensions: usize,
    },
}

/// First-class catalog definition, also accepted by
/// [`crate::Database::create_retrieval_index`]. Names are case-sensitive, as are
/// existing PicoVolt table/column identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalIndexDefinition {
    #[serde(deserialize_with = "identifier")]
    pub name: String,
    #[serde(deserialize_with = "identifier")]
    pub table: String,
    #[serde(deserialize_with = "identifier")]
    pub id_column: String,
    pub index: RetrievalIndexKind,
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn identifier<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<String, D::Error> {
    struct Identifier;
    impl serde::de::Visitor<'_> for Identifier {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a nonempty identifier of at most 256 UTF-8 bytes")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<String, E> {
            if !valid_identifier(value) {
                return Err(E::custom("invalid persistent retrieval identifier"));
            }
            Ok(value.to_owned())
        }
    }
    deserializer.deserialize_str(Identifier)
}

fn bounded_vec<'de, D, T, const LIMIT: usize>(
    deserializer: D,
) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Bounded<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "a sequence containing at most {N} entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Vec<T>, A::Error> {
            let mut values = Vec::new();
            while values.len() < N {
                match seq.next_element()? {
                    Some(value) => values.push(value),
                    None => return Ok(values),
                }
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom(
                    "persistent retrieval sequence exceeds its limit",
                ));
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Bounded::<T, LIMIT>(std::marker::PhantomData))
}

fn column_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct Name(#[serde(deserialize_with = "identifier")] String);
    let names = bounded_vec::<D, Name, 16>(deserializer)?;
    Ok(names.into_iter().map(|name| name.0).collect())
}

impl RetrievalIndexDefinition {
    pub(crate) fn validate(&self) -> Result<()> {
        if !valid_identifier(&self.name)
            || !valid_identifier(&self.table)
            || !valid_identifier(&self.id_column)
        {
            return Err(PvError::Schema(
                "persistent retrieval identifiers must contain 1–256 non-control UTF-8 bytes"
                    .into(),
            ));
        }
        match &self.index {
            RetrievalIndexKind::FullText { text_columns } => {
                let unique: BTreeSet<_> = text_columns.iter().collect();
                if text_columns.is_empty()
                    || text_columns.len() > 16
                    || unique.len() != text_columns.len()
                    || text_columns.iter().any(|name| !valid_identifier(name))
                {
                    return Err(PvError::Schema(
                        "full-text indexes require 1–16 distinct text columns".into(),
                    ));
                }
            }
            RetrievalIndexKind::Vector {
                vector_column,
                dimensions,
                ..
            } => {
                if !valid_identifier(vector_column) || *dimensions == 0 || *dimensions > 4096 {
                    return Err(PvError::Schema(
                        "vector indexes require a column and dimensions in 1–4096".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn columns(&self) -> Vec<&str> {
        match &self.index {
            RetrievalIndexKind::FullText { text_columns } => {
                text_columns.iter().map(String::as_str).collect()
            }
            RetrievalIndexKind::Vector { vector_column, .. } => vec![vector_column.as_str()],
        }
    }

    pub(crate) fn tag(&self) -> u8 {
        match self.index {
            RetrievalIndexKind::FullText { .. } => 1,
            RetrievalIndexKind::Vector { .. } => 2,
        }
    }
}

/// Current logical state plus the last persisted generation. `pending_write`
/// means the current private writer state has not yet been encoded; it is not
/// externally committed. Persisted bytes exclude unreachable older CAS blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrievalIndexInfo {
    pub definition: RetrievalIndexDefinition,
    pub document_count: usize,
    pub source_bytes: usize,
    pub persisted_bytes: usize,
    pub generation: u64,
    pub snapshot_transaction: u64,
    pub health: String,
    pub pending_write: bool,
    pub rebuild_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Descriptor {
    pub definition: RetrievalIndexDefinition,
    pub cas_id: u64,
    pub encoded_bytes: usize,
    pub document_count: usize,
    pub generation: u64,
    pub rebuild_count: u64,
}

pub(crate) fn descriptors<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<Descriptor>, D::Error> {
    bounded_vec::<D, Descriptor, MAX_RETRIEVAL_INDEXES>(deserializer)
}

pub(crate) fn validate_descriptors(
    values: &[Descriptor],
    clock: u64,
    cas_count: usize,
) -> Result<()> {
    if values.len() > MAX_RETRIEVAL_INDEXES {
        return Err(corrupt("too many catalog definitions"));
    }
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for value in values {
        value
            .definition
            .validate()
            .map_err(|error| corrupt(error.to_string()))?;
        total = total
            .checked_add(value.encoded_bytes)
            .ok_or_else(|| corrupt("catalog byte overflow"))?;
        if !names.insert(&value.definition.name)
            || value.generation > clock
            || value.document_count > MAX_RETRIEVAL_DOCUMENTS
            || value.encoded_bytes < 92
            || value.encoded_bytes > MAX_RETRIEVAL_INDEX_BYTES
            || total > MAX_RETRIEVAL_CATALOG_BYTES
            || usize::try_from(value.cas_id).map_or(true, |id| id >= cas_count)
        {
            return Err(corrupt(format!(
                "invalid descriptor for {}",
                value.definition.name
            )));
        }
    }
    Ok(())
}

pub(crate) fn corrupt(message: impl std::fmt::Display) -> PvError {
    PvError::Corruption(format!("persistent retrieval: {message}"))
}

/// Checked, allocation-free cursor. Every length is checked before its slice is
/// returned; callers separately cap cardinality before constructing containers.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_RETRIEVAL_INDEX_BYTES {
            return Err(corrupt("encoded index exceeds 32 MiB"));
        }
        Ok(Self { bytes, offset: 0 })
    }
    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| corrupt("truncated or overflowing index extent"))?;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| corrupt("u16"))?,
        ))
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| corrupt("u32"))?,
        ))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| corrupt("u64"))?,
        ))
    }
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub(crate) fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| corrupt("i64"))?,
        ))
    }
    pub(crate) fn count(&mut self, max: usize) -> Result<usize> {
        let count = usize::try_from(self.u32()?).map_err(|_| corrupt("count conversion"))?;
        if count > max {
            return Err(corrupt("encoded count exceeds limit"));
        }
        Ok(count)
    }
    pub(crate) fn finish(&self) -> Result<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(corrupt("trailing index bytes"))
        }
    }
}

pub(crate) struct Encoder {
    bytes: Vec<u8>,
}
impl Encoder {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }
    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<()> {
        let len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| corrupt("encoded size overflow"))?;
        if len > MAX_RETRIEVAL_INDEX_BYTES {
            return Err(PvError::ResourceLimit(
                "persistent retrieval encoding exceeds 32 MiB".into(),
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub(crate) fn count(&mut self, count: usize) -> Result<()> {
        let value = u32::try_from(count).map_err(|_| corrupt("encoded count overflow"))?;
        self.put(&value.to_le_bytes())
    }
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct IndexData {
    #[cfg(feature = "full-text")]
    pub text: Option<crate::search::SearchIndex>,
    #[cfg(feature = "vector-search")]
    pub vector: Option<crate::vector::VectorIndex>,
}

impl IndexData {
    pub(crate) fn new(definition: &RetrievalIndexDefinition) -> Result<Self> {
        definition.validate()?;
        match &definition.index {
            RetrievalIndexKind::FullText { .. } => {
                #[cfg(feature = "full-text")]
                {
                    Ok(Self {
                        text: Some(crate::search::SearchIndex::new()),
                        #[cfg(feature = "vector-search")]
                        vector: None,
                    })
                }
                #[cfg(not(feature = "full-text"))]
                {
                    Err(PvError::Query(
                        "persistent full-text indexes require the full-text feature".into(),
                    ))
                }
            }
            RetrievalIndexKind::Vector {
                metric, dimensions, ..
            } => {
                let _ = (metric, dimensions);
                #[cfg(feature = "vector-search")]
                {
                    Ok(Self {
                        #[cfg(feature = "full-text")]
                        text: None,
                        vector: Some(
                            crate::vector::VectorIndex::new(*dimensions, (*metric).into())
                                .map_err(|error| PvError::Schema(error.to_string()))?,
                        ),
                    })
                }
                #[cfg(not(feature = "vector-search"))]
                {
                    Err(PvError::Query(
                        "persistent vector indexes require the vector-search feature".into(),
                    ))
                }
            }
        }
    }
    pub(crate) fn body(&self) -> Result<Vec<u8>> {
        #[cfg(feature = "full-text")]
        if let Some(index) = &self.text {
            return index.encode_persistent();
        }
        #[cfg(feature = "vector-search")]
        if let Some(index) = &self.vector {
            return index.encode_persistent();
        }
        Err(corrupt("index data is unavailable"))
    }
    fn decode(definition: &RetrievalIndexDefinition, body: &[u8]) -> Result<Self> {
        let mut data = Self::new(definition)?;
        let _ = (&mut data, body);
        #[cfg(feature = "full-text")]
        if data.text.is_some() {
            data.text = Some(crate::search::SearchIndex::decode_persistent(body)?);
        }
        #[cfg(feature = "vector-search")]
        if let RetrievalIndexKind::Vector {
            metric, dimensions, ..
        } = &definition.index
        {
            data.vector = Some(crate::vector::VectorIndex::decode_persistent(
                body,
                *dimensions,
                (*metric).into(),
            )?);
        }
        Ok(data)
    }
    fn insert(&mut self, value: &PreparedDocument) -> Result<()> {
        let _ = value;
        #[cfg(feature = "full-text")]
        if let (Some(index), Some(text)) = (&mut self.text, &value.text) {
            return index
                .upsert(value.id, text)
                .map_err(|error| PvError::Schema(error.to_string()));
        }
        #[cfg(feature = "vector-search")]
        if let (Some(index), Some(vector)) = (&mut self.vector, &value.vector) {
            return index
                .upsert(value.id, vector)
                .map_err(|error| PvError::Schema(error.to_string()));
        }
        Err(corrupt("index data and document kind disagree"))
    }
    fn remove(&mut self, id: i64) -> Result<()> {
        let _ = id;
        #[cfg(feature = "full-text")]
        if let Some(index) = &mut self.text {
            return if index.remove(id) {
                Ok(())
            } else {
                Err(corrupt("missing text document"))
            };
        }
        #[cfg(feature = "vector-search")]
        if let Some(index) = &mut self.vector {
            return if index.remove(id) {
                Ok(())
            } else {
                Err(corrupt("missing vector document"))
            };
        }
        Err(corrupt("index data is unavailable"))
    }
}

pub(crate) struct PreparedDocument {
    id: i64,
    fingerprint: [u8; 32],
    bytes: usize,
    #[cfg(feature = "full-text")]
    text: Option<String>,
    #[cfg(feature = "vector-search")]
    vector: Option<Vec<f32>>,
}

fn prepare(
    definition: &RetrievalIndexDefinition,
    columns: &[String],
    row: &Row,
) -> Result<PreparedDocument> {
    let position = |name: &str| {
        columns
            .iter()
            .position(|column| column == name)
            .ok_or_else(|| PvError::Schema(format!("missing retrieval column `{name}`")))
    };
    let id_position = position(&definition.id_column)?;
    let Some(Value::Int(id)) = row.get(id_position) else {
        return Err(PvError::Schema(
            "persistent retrieval IDs must be unique signed integers".into(),
        ));
    };
    let fields = definition
        .columns()
        .iter()
        .map(|name| {
            row.get(position(name)?)
                .ok_or_else(|| PvError::Schema("incomplete retrieval row".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut prepared = PreparedDocument {
        id: *id,
        fingerprint: [0; 32],
        bytes: 0,
        #[cfg(feature = "full-text")]
        text: None,
        #[cfg(feature = "vector-search")]
        vector: None,
    };
    match &definition.index {
        RetrievalIndexKind::FullText { .. } => {
            let mut text = String::new();
            for field in &fields {
                let value = match field {
                    Value::Text(value) => value.as_str(),
                    Value::Null => "",
                    _ => {
                        return Err(PvError::Schema(
                            "full-text columns must be TEXT or NULL".into(),
                        ))
                    }
                };
                if text.len().saturating_add(value.len()).saturating_add(1) > 65_536 {
                    return Err(PvError::ResourceLimit(
                        "full-text document exceeds 64 KiB".into(),
                    ));
                }
                text.push_str(value);
                text.push(' ');
            }
            prepared.bytes = text.len();
            #[cfg(feature = "full-text")]
            {
                prepared.text = Some(text);
            }
        }
        RetrievalIndexKind::Vector {
            dimensions, metric, ..
        } => {
            let Some(Value::Text(encoded)) = fields.first().copied() else {
                return Err(PvError::Schema(
                    "vectors must be JSON arrays in TEXT".into(),
                ));
            };
            if encoded.len() > 65_536 {
                return Err(PvError::ResourceLimit(
                    "encoded vector exceeds 64 KiB".into(),
                ));
            }
            let values: Vec<f32> = bounded_vector(encoded, *dimensions)?;
            if values.iter().any(|value| !value.is_finite())
                || (*metric == RetrievalMetric::Cosine && values.iter().all(|value| *value == 0.0))
            {
                return Err(PvError::Schema(
                    "vectors must be finite and cosine vectors nonzero".into(),
                ));
            }
            prepared.bytes = encoded.len();
            #[cfg(feature = "vector-search")]
            {
                prepared.vector = Some(values);
            }
        }
    }
    // Every field was capped before serialization. The digest binds the exact
    // projection (including NULL and JSON spelling) to its stable signed ID.
    prepared.fingerprint = *blake3::hash(&serde_json::to_vec(&(*id, fields))?).as_bytes();
    Ok(prepared)
}

fn bounded_vector(encoded: &str, dimensions: usize) -> Result<Vec<f32>> {
    let mut deserializer = serde_json::Deserializer::from_str(encoded);
    let values = bounded_vec::<_, f32, 4096>(&mut deserializer)
        .map_err(|error| PvError::Schema(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| PvError::Schema(error.to_string()))?;
    if values.len() != dimensions {
        return Err(PvError::Schema(
            "vector dimensions differ from the index declaration".into(),
        ));
    }
    Ok(values)
}

#[derive(Clone, PartialEq, Eq)]
struct SourceDocument {
    fingerprint: [u8; 32],
    bytes: usize,
}

pub(crate) struct CatalogEntry {
    pub definition: RetrievalIndexDefinition,
    pub generation: u64,
    pub rebuild_count: u64,
    pub descriptor: Option<Descriptor>,
    pub data: IndexData,
    sources: BTreeMap<i64, SourceDocument>,
    pub source_bytes: usize,
    pub dirty: bool,
}

impl CatalogEntry {
    pub(crate) fn new(definition: RetrievalIndexDefinition, generation: u64) -> Result<Self> {
        let data = IndexData::new(&definition)?;
        Ok(Self {
            definition,
            generation,
            rebuild_count: 0,
            descriptor: None,
            data,
            sources: BTreeMap::new(),
            source_bytes: 0,
            dirty: true,
        })
    }
    pub(crate) fn len(&self) -> usize {
        self.sources.len()
    }
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub(crate) fn contains(&self, id: i64) -> bool {
        self.sources.contains_key(&id)
    }
    pub(crate) fn add_row(&mut self, columns: &[String], row: &Row) -> Result<()> {
        let prepared = prepare(&self.definition, columns, row)?;
        self.add(prepared)
    }
    fn add(&mut self, value: PreparedDocument) -> Result<()> {
        if self.sources.contains_key(&value.id) {
            return Err(PvError::Schema("duplicate persistent retrieval ID".into()));
        }
        let total = self
            .source_bytes
            .checked_add(value.bytes)
            .ok_or_else(|| corrupt("source byte overflow"))?;
        let source_limit = if self.definition.tag() == 1 {
            8 * 1024 * 1024
        } else {
            MAX_RETRIEVAL_SOURCE_BYTES
        };
        if self.len() >= MAX_RETRIEVAL_DOCUMENTS || total > source_limit {
            return Err(PvError::ResourceLimit(
                "persistent retrieval source budget exceeded".into(),
            ));
        }
        self.data.insert(&value)?;
        self.sources.insert(
            value.id,
            SourceDocument {
                fingerprint: value.fingerprint,
                bytes: value.bytes,
            },
        );
        self.source_bytes = total;
        self.dirty = true;
        Ok(())
    }
    pub(crate) fn apply(
        &mut self,
        columns: &[String],
        removed: &[Row],
        added: &[Row],
        clock: u64,
    ) -> Result<()> {
        // All callers own a rollback boundary. Validate inputs before changing
        // this entry; a later corpus-limit error aborts the complete transaction.
        let mut old = BTreeMap::new();
        let mut new = BTreeMap::new();
        for row in removed {
            let value = prepare(&self.definition, columns, row)?;
            let expected = SourceDocument {
                fingerprint: value.fingerprint,
                bytes: value.bytes,
            };
            if self.sources.get(&value.id) != Some(&expected)
                || old.insert(value.id, value).is_some()
            {
                return Err(corrupt("mutation source does not match the current index"));
            }
        }
        for row in added {
            let value = prepare(&self.definition, columns, row)?;
            if new.insert(value.id, value).is_some() {
                return Err(PvError::Schema("duplicate persistent retrieval ID".into()));
            }
        }
        let unchanged: Vec<_> = old
            .iter()
            .filter_map(|(id, value)| {
                new.get(id)
                    .filter(|other| other.fingerprint == value.fingerprint)
                    .map(|_| *id)
            })
            .collect();
        for id in unchanged {
            old.remove(&id);
            new.remove(&id);
        }
        if old.is_empty() && new.is_empty() {
            return Ok(());
        }
        for id in old.keys() {
            self.data.remove(*id)?;
            let removed = self
                .sources
                .remove(id)
                .ok_or_else(|| corrupt("missing mutation source"))?;
            self.source_bytes = self
                .source_bytes
                .checked_sub(removed.bytes)
                .ok_or_else(|| corrupt("source byte underflow"))?;
        }
        for value in new.into_values() {
            self.add(value)?;
        }
        self.generation = clock;
        self.dirty = true;
        Ok(())
    }
    fn source_digest(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"picovolt-retrieval-source-v1\0");
        for (id, value) in &self.sources {
            hash.update(&id.to_le_bytes());
            hash.update(&value.fingerprint);
            hash.update(&(value.bytes as u64).to_le_bytes());
        }
        *hash.finalize().as_bytes()
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let definition = serde_json::to_vec(&self.definition)?;
        if definition.len() > MAX_DEFINITION_BYTES {
            return Err(corrupt("definition exceeds 8 KiB"));
        }
        let body = self.data.body()?;
        let mut output = Encoder::new();
        output.put(MAGIC)?;
        #[cfg(test)]
        crash_point("during_encoding");
        output.put(&1u16.to_le_bytes())?;
        output.put(&[self.definition.tag(), 0])?;
        output.put(&self.generation.to_le_bytes())?;
        output.count(definition.len())?;
        output.put(&definition)?;
        output.put(&self.source_digest())?;
        output.count(body.len())?;
        output.put(&body)?;
        let mut bytes = output.finish();
        let hash = *blake3::hash(&bytes).as_bytes();
        if bytes.len().saturating_add(32) > MAX_RETRIEVAL_INDEX_BYTES {
            return Err(PvError::ResourceLimit(
                "persistent retrieval envelope exceeds 32 MiB".into(),
            ));
        }
        bytes.extend_from_slice(&hash);
        Ok(bytes)
    }
    pub(crate) fn verify_loaded(mut self, descriptor: &Descriptor, bytes: &[u8]) -> Result<Self> {
        let decoded = decode_envelope(bytes)?;
        if decoded.definition != self.definition
            || decoded.generation != descriptor.generation
            || descriptor.document_count != self.len()
            || bytes.len() != descriptor.encoded_bytes
            || decoded.source_digest != self.source_digest()
            || decoded.data.body()? != self.data.body()?
        {
            return Err(corrupt(format!(
                "{} is stale or disagrees with its authoritative table",
                self.definition.name
            )));
        }
        // Retain decoded, validated state. The authoritative rebuild is an open
        // verification cost, never a per-query operation or silent repair.
        self.data = decoded.data;
        self.generation = descriptor.generation;
        self.rebuild_count = descriptor.rebuild_count;
        self.descriptor = Some(descriptor.clone());
        self.dirty = false;
        Ok(self)
    }
    pub(crate) fn info(&self, clock: u64) -> RetrievalIndexInfo {
        RetrievalIndexInfo {
            definition: self.definition.clone(),
            document_count: self.len(),
            source_bytes: self.source_bytes,
            persisted_bytes: self
                .descriptor
                .as_ref()
                .map_or(0, |value| value.encoded_bytes),
            generation: self.generation,
            snapshot_transaction: clock,
            health: "healthy".into(),
            pending_write: self.dirty,
            rebuild_count: self.rebuild_count,
        }
    }
}

struct DecodedEnvelope {
    definition: RetrievalIndexDefinition,
    generation: u64,
    source_digest: [u8; 32],
    data: IndexData,
}

fn decode_envelope(bytes: &[u8]) -> Result<DecodedEnvelope> {
    if bytes.len() < 92 || bytes.len() > MAX_RETRIEVAL_INDEX_BYTES {
        return Err(corrupt("invalid envelope length"));
    }
    let split = bytes.len() - 32;
    if blake3::hash(&bytes[..split]).as_bytes() != &bytes[split..] {
        return Err(corrupt("envelope checksum mismatch"));
    }
    let mut input = Decoder::new(&bytes[..split])?;
    if input.take(8)? != MAGIC || input.u16()? != 1 {
        return Err(corrupt("unsupported index magic or version"));
    }
    let tag = input.u8()?;
    if input.u8()? != 0 {
        return Err(corrupt("nonzero reserved header byte"));
    }
    let generation = input.u64()?;
    let definition_len = input.count(MAX_DEFINITION_BYTES)?;
    let encoded_definition = input.take(definition_len)?;
    let definition: RetrievalIndexDefinition =
        serde_json::from_slice(encoded_definition).map_err(|error| corrupt(error.to_string()))?;
    definition
        .validate()
        .map_err(|error| corrupt(error.to_string()))?;
    if tag != definition.tag() || serde_json::to_vec(&definition)? != encoded_definition {
        return Err(corrupt("noncanonical or mismatched definition"));
    }
    let source_digest = input
        .take(32)?
        .try_into()
        .map_err(|_| corrupt("source digest length"))?;
    let body_len = input.count(MAX_RETRIEVAL_INDEX_BYTES)?;
    let body = input.take(body_len)?;
    input.finish()?;
    let data = IndexData::decode(&definition, body)?;
    Ok(DecodedEnvelope {
        definition,
        generation,
        source_digest,
        data,
    })
}

/// Decoder-only robustness seam for fuzzing. Structural validation is not proof
/// of agreement with a source table: database open performs that second check.
#[doc(hidden)]
pub fn validate_retrieval_index_bytes(bytes: &[u8]) -> Result<()> {
    decode_envelope(bytes).map(|_| ())
}

// Entirely absent from ordinary library, CLI, bindings and release builds.
#[cfg(test)]
pub(crate) fn crash_point(point: &str) {
    if std::env::var("PV23_UNIT_CRASH_ARMED").as_deref() != Ok("yes")
        || std::env::var("PV23_UNIT_CRASH_POINT").as_deref() != Ok(point)
    {
        return;
    }
    let signal = std::env::var("PV23_UNIT_CRASH_SIGNAL").expect("test child signal path");
    let mut file = std::fs::File::create(signal).expect("test child signal file");
    std::io::Write::write_all(&mut file, point.as_bytes()).expect("test child signal write");
    file.sync_all().expect("test child signal fsync");
    std::process::exit(88);
}
