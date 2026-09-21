// Modified for PicoVolt 2.3 persistent retrieval; see legal/COMPONENT-SCOPE-2.3.md.
// Modified for PicoVolt 2.2.0 encryption/hybrid retrieval; see legal/COMPONENT-SCOPE-2.2.md.
// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Retrieval over one bounded SELECT snapshot. Mutations are rejected before execution.
use crate::engine::query::{bind_params, parse, Projection, Statement};
use crate::persistent::{CatalogEntry, RetrievalIndexKind};
use crate::{Database, PvError, QueryLimits, QueryResult, Value};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetrievalRequest {
    #[cfg(all(feature = "full-text", feature = "vector-search"))]
    Hybrid {
        sql: String,
        #[serde(default)] params: Vec<serde_json::Value>,
        id_column: String,
        text_columns: Vec<String>,
        vector_column: String,
        query: String,
        vector_query: Vec<f32>,
        metric: crate::vector::Metric,
        text_weight: f64,
        candidate_limit: usize,
        limit: usize,
        #[serde(default)] text_index: Option<String>,
        #[serde(default)] vector_index: Option<String>,
    },
    #[cfg(feature = "full-text")]
    FullText {
        sql: String,
        #[serde(default)] params: Vec<serde_json::Value>,
        id_column: String,
        text_columns: Vec<String>,
        query: String,
        limit: usize,
        #[serde(default)] index: Option<String>,
    },
    #[cfg(feature = "vector-search")]
    Vector {
        sql: String,
        #[serde(default)] params: Vec<serde_json::Value>,
        id_column: String,
        vector_column: String,
        query: Vec<f32>,
        metric: crate::vector::Metric,
        limit: usize,
        #[serde(default)] index: Option<String>,
    },
}

fn parameters(values: &[serde_json::Value]) -> crate::Result<Vec<Value>> {
    if values.len() > 256 { return Err(PvError::Query("At most 256 retrieval parameters".into())); }
    values.iter().map(|value| match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::String(value) => Ok(Value::Text(value.clone())),
        serde_json::Value::Bool(value) => Ok(Value::Int(i64::from(*value))),
        serde_json::Value::Number(value) if value.as_i64().is_some() => Ok(Value::Int(value.as_i64().unwrap())),
        _ => Err(PvError::Query("Retrieval parameters support null, text, bool and signed integers".into())),
    }).collect()
}

struct Selection {
    rows: QueryResult,
    table: String,
    accelerate: bool,
}

fn snapshot(db: &mut Database, sql: &str, params: &[serde_json::Value]) -> crate::Result<Selection> {
    if sql.len() > 65536 { return Err(PvError::Query("Retrieval SQL exceeds 64 KiB".into())); }
    let values = parameters(params)?;
    let Statement::Select { table, projection, before, distinct, group_by, having, .. } = parse(&bind_params(sql, &values)?)? else {
        return Err(PvError::Query("Retrieval requires a single SELECT statement".into()));
    };
    // Never substitute current postings for historical data, or raw source
    // columns for expressions/aggregates/aliases that changed the projection.
    let accelerate = before.is_none() && !distinct && group_by.is_empty() && having.is_none()
        && matches!(projection, Projection::All | Projection::Columns(_));
    let rows = db.query_with_limits(sql, &values, QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None))?;
    Ok(Selection { rows, table, accelerate })
}

fn named_entry<'a>(db: &'a Database, name: Option<&str>, selection: &Selection, id_column: &str) -> crate::Result<Option<&'a CatalogEntry>> {
    let Some(name) = name else { return Ok(None); };
    if name.is_empty() || name.len() > 256 { return Err(PvError::Query("Invalid persistent index name".into())); }
    let entry = db.named_retrieval_entry(name)?;
    if entry.definition.table != selection.table || entry.definition.id_column != id_column {
        return Err(PvError::Query("Named retrieval index table/ID definition mismatch".into()));
    }
    Ok(Some(entry))
}

fn selected_ids(selection: &Selection, entry: &CatalogEntry, required_columns: &[&str]) -> crate::Result<BTreeSet<i64>> {
    let QueryResult::Rows { columns, rows } = &selection.rows else {
        return Err(PvError::Query("Retrieval requires rows".into()));
    };
    for column in required_columns {
        if !columns.iter().any(|actual| actual == column) {
            return Err(PvError::Query(format!("Missing retrieval column `{column}`")));
        }
    }
    let id = columns.iter().position(|column| column == &entry.definition.id_column)
        .ok_or_else(|| PvError::Query("Missing ID column".into()))?;
    if rows.len() > crate::persistent::MAX_RETRIEVAL_DOCUMENTS { return Err(PvError::Query("Retrieval filter-ID limit exceeded".into())); }
    let mut ids = BTreeSet::new();
    for row in rows {
        let Some(Value::Int(value)) = row.get(id) else { return Err(PvError::Query("Integer IDs required".into())); };
        if !ids.insert(*value) { return Err(PvError::Query("Duplicate retrieval ID".into())); }
        if !entry.contains(*value) { return Err(crate::persistent::corrupt("current SELECT and index IDs disagree")); }
    }
    Ok(ids)
}

#[cfg(feature = "full-text")]
fn text_hits(db: &Database, selection: &Selection, id_column: &str, columns: &[String], query: &str, limit: usize, name: Option<&str>) -> crate::Result<Vec<crate::search::SearchHit>> {
    let entry = named_entry(db, name, selection, id_column)?;
    let text_columns: Vec<_> = columns.iter().map(String::as_str).collect();
    if let Some(entry) = entry {
        if !matches!(&entry.definition.index, RetrievalIndexKind::FullText { text_columns } if text_columns == columns) {
            return Err(PvError::Query("Named full-text index column/kind definition mismatch".into()));
        }
        if selection.accelerate {
            let ids = selected_ids(selection, entry, &text_columns)?;
            let index = entry.data.text.as_ref().ok_or_else(|| crate::persistent::corrupt("missing admitted text data"))?;
            return index.search_filtered(query, limit, &ids).map_err(|error| PvError::Query(error.to_string()));
        }
    }
    crate::search::SearchIndex::from_rows(&selection.rows, id_column, &text_columns)
        .and_then(|index| index.search(query, limit)).map_err(|error| PvError::Query(error.to_string()))
}

#[cfg(feature = "vector-search")]
#[allow(clippy::too_many_arguments)]
fn vector_hits(db: &Database, selection: &Selection, id_column: &str, column: &str, query: &[f32], metric: crate::vector::Metric, limit: usize, name: Option<&str>) -> crate::Result<Vec<crate::vector::VectorHit>> {
    if let Some(entry) = named_entry(db, name, selection, id_column)? {
        let requested_metric = match metric {
            crate::vector::Metric::Cosine => crate::persistent::RetrievalMetric::Cosine,
            crate::vector::Metric::SquaredEuclidean => crate::persistent::RetrievalMetric::SquaredEuclidean,
        };
        if !matches!(&entry.definition.index, RetrievalIndexKind::Vector { vector_column, dimensions, metric }
            if vector_column == column && *dimensions == query.len() && *metric == requested_metric) {
            return Err(PvError::Query("Named vector index column/metric/dimensions definition mismatch".into()));
        }
        if selection.accelerate {
            let ids = selected_ids(selection, entry, &[column])?;
            let index = entry.data.vector.as_ref().ok_or_else(|| crate::persistent::corrupt("missing admitted vector data"))?;
            return index.search_filtered(query, limit, &ids).map_err(|error| PvError::Query(error.to_string()));
        }
    }
    vector_index(&selection.rows, id_column, column, query, metric)?.search(query, limit).map_err(|error| PvError::Query(error.to_string()))
}

impl Database {
    /// Run full-text, exact vector or hybrid retrieval on one bounded SELECT.
    /// Without named indexes, the 2.2 bounded per-call rebuild remains unchanged.
    /// Explicit names accelerate eligible current projections using the exact
    /// filtered corpus. Historical/transformed projections safely rebuild.
    /// IDs in JSON output are decimal strings, preserving all 64 bits in browsers.
    pub fn retrieve_json(&mut self, request: &str) -> crate::Result<String> {
        if request.len() > 131072 { return Err(PvError::Query("Retrieval request exceeds 128 KiB".into())); }
        let request: RetrievalRequest = serde_json::from_str(request).map_err(|error| PvError::Query(error.to_string()))?;
        let hits = match request {
            #[cfg(all(feature = "full-text", feature = "vector-search"))]
            RetrievalRequest::Hybrid { sql, params, id_column, text_columns, vector_column, query, vector_query, metric,
                text_weight, candidate_limit, limit, text_index, vector_index } => {
                if limit > 100 || candidate_limit == 0 || candidate_limit > 100 || limit > candidate_limit
                    || !text_weight.is_finite() || !(0.0..=1.0).contains(&text_weight) || query.len() > 1024
                    || text_columns.is_empty() || text_columns.len() > 16 {
                    return Err(PvError::Query("Invalid hybrid search bounds or weight".into()));
                }
                let selection = snapshot(self, &sql, &params)?;
                let text_hits = text_hits(self, &selection, &id_column, &text_columns, &query, candidate_limit, text_index.as_deref())?;
                let vector_hits = vector_hits(self, &selection, &id_column, &vector_column, &vector_query, metric, candidate_limit, vector_index.as_deref())?;
                let mut combined = std::collections::BTreeMap::<i64, (f64, Option<usize>, Option<usize>)>::new();
                if text_weight > 0.0 {
                    for (rank, hit) in text_hits.iter().enumerate() {
                        let entry = combined.entry(hit.id).or_default();
                        entry.0 += text_weight / (60.0 + (rank + 1) as f64);
                        entry.1 = Some(rank + 1);
                    }
                }
                if text_weight < 1.0 {
                    for (rank, hit) in vector_hits.iter().enumerate() {
                        let entry = combined.entry(hit.id).or_default();
                        entry.0 += (1.0 - text_weight) / (60.0 + (rank + 1) as f64);
                        entry.2 = Some(rank + 1);
                    }
                }
                let mut ranked: Vec<_> = combined.into_iter().collect();
                ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
                ranked.truncate(limit);
                ranked.into_iter().map(|(id, (score, text_rank, vector_rank))|
                    serde_json::json!({"id": id.to_string(), "score": score, "text_rank": text_rank, "vector_rank": vector_rank})).collect::<Vec<_>>()
            }
            #[cfg(feature = "full-text")]
            RetrievalRequest::FullText { sql, params, id_column, text_columns, query, limit, index } => {
                if limit > 100 || query.len() > 1024 || text_columns.is_empty() || text_columns.len() > 16 {
                    return Err(PvError::Query("Invalid full-text bounds".into()));
                }
                let selection = snapshot(self, &sql, &params)?;
                text_hits(self, &selection, &id_column, &text_columns, &query, limit, index.as_deref())?
                    .into_iter().map(|hit| serde_json::json!({"id": hit.id.to_string(), "score": hit.score})).collect::<Vec<_>>()
            }
            #[cfg(feature = "vector-search")]
            RetrievalRequest::Vector { sql, params, id_column, vector_column, query, metric, limit, index } => {
                if limit > 100 { return Err(PvError::Query("At most 100 vector results".into())); }
                let selection = snapshot(self, &sql, &params)?;
                vector_hits(self, &selection, &id_column, &vector_column, &query, metric, limit, index.as_deref())?
                    .into_iter().map(|hit| serde_json::json!({"id": hit.id.to_string(), "distance": hit.distance})).collect::<Vec<_>>()
            }
        };
        serde_json::to_string(&hits).map_err(|error| PvError::Query(error.to_string()))
    }
}

#[cfg(feature = "vector-search")]
fn vector_index(source: &QueryResult, id_column: &str, vector_column: &str, query: &[f32], metric: crate::vector::Metric) -> crate::Result<crate::vector::VectorIndex> {
    let mut index = crate::vector::VectorIndex::new(query.len(), metric).map_err(|error| PvError::Query(error.to_string()))?;
    index.search(query, 0).map_err(|error| PvError::Query(error.to_string()))?;
    let QueryResult::Rows { columns, rows } = source else { unreachable!() };
    let id = columns.iter().position(|column| column == id_column).ok_or_else(|| PvError::Query("Missing ID column".into()))?;
    let vector = columns.iter().position(|column| column == vector_column).ok_or_else(|| PvError::Query("Missing vector column".into()))?;
    let mut seen = BTreeSet::new();
    for row in rows {
        let Value::Int(identity) = row[id] else { return Err(PvError::Query("Integer IDs required".into())); };
        if !seen.insert(identity) { return Err(PvError::Query("Duplicate retrieval ID".into())); }
        let Value::Text(encoded) = &row[vector] else { return Err(PvError::Query("Vectors must be JSON arrays stored in TEXT".into())); };
        if encoded.len() > 65536 { return Err(PvError::Query("Encoded vector exceeds 64 KiB".into())); }
        let values: Vec<f32> = serde_json::from_str(encoded).map_err(|error| PvError::Query(error.to_string()))?;
        index.upsert(identity, &values).map_err(|error| PvError::Query(error.to_string()))?;
    }
    Ok(index)
}
