// Modified for PicoVolt 2.2.0 encryption/hybrid retrieval; see legal/COMPONENT-SCOPE-2.2.md.
// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Retrieval over one bounded SELECT snapshot. Mutations are rejected before execution.
use crate::engine::query::{bind_params, parse, Statement};
use crate::{Database, PvError, QueryLimits, QueryResult, Value};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetrievalRequest {
    #[cfg(all(feature = "full-text", feature = "vector-search"))]
    Hybrid {
        sql: String,
        #[serde(default)]
        params: Vec<serde_json::Value>,
        id_column: String,
        text_columns: Vec<String>,
        vector_column: String,
        query: String,
        vector_query: Vec<f32>,
        metric: crate::vector::Metric,
        text_weight: f64,
        candidate_limit: usize,
        limit: usize,
    },
    #[cfg(feature = "full-text")]
    FullText {
        sql: String,
        #[serde(default)]
        params: Vec<serde_json::Value>,
        id_column: String,
        text_columns: Vec<String>,
        query: String,
        limit: usize,
    },
    #[cfg(feature = "vector-search")]
    Vector {
        sql: String,
        #[serde(default)]
        params: Vec<serde_json::Value>,
        id_column: String,
        vector_column: String,
        query: Vec<f32>,
        metric: crate::vector::Metric,
        limit: usize,
    },
}

fn parameters(values: &[serde_json::Value]) -> crate::Result<Vec<Value>> {
    if values.len() > 256 {
        return Err(PvError::Query("At most 256 retrieval parameters".into()));
    }
    values
        .iter()
        .map(|v| match v {
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::String(s) => Ok(Value::Text(s.clone())),
            serde_json::Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
            serde_json::Value::Number(n) if n.as_i64().is_some() => {
                Ok(Value::Int(n.as_i64().unwrap()))
            }
            _ => Err(PvError::Query(
                "Retrieval parameters support null, text, bool and signed integers".into(),
            )),
        })
        .collect()
}

fn snapshot(
    db: &mut Database,
    sql: &str,
    params: &[serde_json::Value],
) -> crate::Result<QueryResult> {
    if sql.len() > 65536 {
        return Err(PvError::Query("Retrieval SQL exceeds 64 KiB".into()));
    }
    let values = parameters(params)?;
    if !matches!(
        parse(&bind_params(sql, &values)?)?,
        Statement::Select { .. }
    ) {
        return Err(PvError::Query(
            "Retrieval requires a single SELECT statement".into(),
        ));
    }
    db.query_with_limits(
        sql,
        &values,
        QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None),
    )
}

impl Database {
    /// Run full-text or vector retrieval on a bounded, filtered SELECT snapshot.
    /// IDs in JSON output are decimal strings, preserving all 64 bits in browsers.
    /// Indexes are rebuilt per call; use SearchIndex/VectorIndex directly to reuse one.
    pub fn retrieve_json(&mut self, request: &str) -> crate::Result<String> {
        if request.len() > 131072 {
            return Err(PvError::Query("Retrieval request exceeds 128 KiB".into()));
        }
        let request: RetrievalRequest =
            serde_json::from_str(request).map_err(|e| PvError::Query(e.to_string()))?;
        let hits = match request {
            #[cfg(all(feature = "full-text", feature = "vector-search"))]
            RetrievalRequest::Hybrid {
                sql,
                params,
                id_column,
                text_columns,
                vector_column,
                query,
                vector_query,
                metric,
                text_weight,
                candidate_limit,
                limit,
            } => {
                if limit > 100
                    || candidate_limit == 0
                    || candidate_limit > 100
                    || limit > candidate_limit
                    || !text_weight.is_finite()
                    || !(0.0..=1.0).contains(&text_weight)
                    || query.len() > 1024
                    || text_columns.is_empty()
                    || text_columns.len() > 16
                {
                    return Err(PvError::Query(
                        "Invalid hybrid search bounds or weight".into(),
                    ));
                }
                // Both rankings see exactly one SQL snapshot, including authorization filters.
                let rows = snapshot(self, &sql, &params)?;
                let columns: Vec<_> = text_columns.iter().map(String::as_str).collect();
                let text = crate::search::SearchIndex::from_rows(&rows, &id_column, &columns)
                    .map_err(|e| PvError::Query(e.to_string()))?;
                let vectors =
                    vector_index(&rows, &id_column, &vector_column, &vector_query, metric)?;
                let text_hits = text
                    .search(&query, candidate_limit)
                    .map_err(|e| PvError::Query(e.to_string()))?;
                let vector_hits = vectors
                    .search(&vector_query, candidate_limit)
                    .map_err(|e| PvError::Query(e.to_string()))?;
                // Weighted reciprocal-rank fusion; scores need no incompatible-scale normalization.
                let mut combined =
                    std::collections::BTreeMap::<i64, (f64, Option<usize>, Option<usize>)>::new();
                if text_weight > 0.0 {
                    for (i, hit) in text_hits.iter().enumerate() {
                        let entry = combined.entry(hit.id).or_default();
                        entry.0 += text_weight / (60.0 + (i + 1) as f64);
                        entry.1 = Some(i + 1);
                    }
                }
                if text_weight < 1.0 {
                    for (i, hit) in vector_hits.iter().enumerate() {
                        let entry = combined.entry(hit.id).or_default();
                        entry.0 += (1.0 - text_weight) / (60.0 + (i + 1) as f64);
                        entry.2 = Some(i + 1);
                    }
                }
                let mut ranked: Vec<_> = combined.into_iter().collect();
                ranked.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0).then(a.0.cmp(&b.0)));
                ranked.truncate(limit);
                ranked.into_iter().map(|(id,(score,text_rank,vector_rank))|serde_json::json!({"id":id.to_string(),"score":score,"text_rank":text_rank,"vector_rank":vector_rank})).collect::<Vec<_>>()
            }
            #[cfg(feature = "full-text")]
            RetrievalRequest::FullText {
                sql,
                params,
                id_column,
                text_columns,
                query,
                limit,
            } => {
                if limit > 100
                    || query.len() > 1024
                    || text_columns.is_empty()
                    || text_columns.len() > 16
                {
                    return Err(PvError::Query("Invalid full-text bounds".into()));
                }
                let rows = snapshot(self, &sql, &params)?;
                let columns: Vec<_> = text_columns.iter().map(String::as_str).collect();
                let index = crate::search::SearchIndex::from_rows(&rows, &id_column, &columns)
                    .map_err(|e| PvError::Query(e.to_string()))?;
                index
                    .search(&query, limit)
                    .map_err(|e| PvError::Query(e.to_string()))?
                    .into_iter()
                    .map(|h| serde_json::json!({"id":h.id.to_string(),"score":h.score}))
                    .collect::<Vec<_>>()
            }
            #[cfg(feature = "vector-search")]
            RetrievalRequest::Vector {
                sql,
                params,
                id_column,
                vector_column,
                query,
                metric,
                limit,
            } => {
                if limit > 100 {
                    return Err(PvError::Query("At most 100 vector results".into()));
                }
                let source = snapshot(self, &sql, &params)?;
                let index = vector_index(&source, &id_column, &vector_column, &query, metric)?;
                index
                    .search(&query, limit)
                    .map_err(|e| PvError::Query(e.to_string()))?
                    .into_iter()
                    .map(|h| serde_json::json!({"id":h.id.to_string(),"distance":h.distance}))
                    .collect::<Vec<_>>()
            }
        };
        serde_json::to_string(&hits).map_err(|e| PvError::Query(e.to_string()))
    }
}

#[cfg(feature = "vector-search")]
fn vector_index(
    source: &QueryResult,
    id_column: &str,
    vector_column: &str,
    query: &[f32],
    metric: crate::vector::Metric,
) -> crate::Result<crate::vector::VectorIndex> {
    let mut index = crate::vector::VectorIndex::new(query.len(), metric)
        .map_err(|e| PvError::Query(e.to_string()))?;
    // Validate the query even for an empty result set.
    index
        .search(query, 0)
        .map_err(|e| PvError::Query(e.to_string()))?;
    let QueryResult::Rows { columns, rows } = source else {
        unreachable!()
    };
    let id = columns
        .iter()
        .position(|c| c == id_column)
        .ok_or_else(|| PvError::Query("Missing ID column".into()))?;
    let vector = columns
        .iter()
        .position(|c| c == vector_column)
        .ok_or_else(|| PvError::Query("Missing vector column".into()))?;
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        let Value::Int(identity) = row[id] else {
            return Err(PvError::Query("Integer IDs required".into()));
        };
        if !seen.insert(identity) {
            return Err(PvError::Query("Duplicate retrieval ID".into()));
        }
        let Value::Text(encoded) = &row[vector] else {
            return Err(PvError::Query(
                "Vectors must be JSON arrays stored in TEXT".into(),
            ));
        };
        if encoded.len() > 65536 {
            return Err(PvError::Query("Encoded vector exceeds 64 KiB".into()));
        }
        let values: Vec<f32> =
            serde_json::from_str(encoded).map_err(|e| PvError::Query(e.to_string()))?;
        index
            .upsert(identity, &values)
            .map_err(|e| PvError::Query(e.to_string()))?;
    }
    Ok(index)
}
