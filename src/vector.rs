// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Exact, bounded vector similarity indexes owned by the application.
//! No embedding model, approximate graph, persistence or automatic SQL indexing.
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Cosine,
    SquaredEuclidean,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VectorError {
    #[error("vector index exceeds its dimension, document or element budget")]
    Limit,
    #[error(
        "vectors must match dimensions and contain finite values; cosine vectors must be nonzero"
    )]
    InvalidVector,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: i64,
    pub distance: f64,
}

/// Exhaustive exact nearest-neighbor search, with stable ID tie-breaking.
/// At most 10,000 documents, 4,096 dimensions and 4,194,304 scalar elements.
pub struct VectorIndex {
    dimensions: usize,
    metric: Metric,
    documents: BTreeMap<i64, Vec<f32>>,
}

impl VectorIndex {
    pub fn new(dimensions: usize, metric: Metric) -> Result<Self, VectorError> {
        if dimensions == 0 || dimensions > 4096 {
            return Err(VectorError::Limit);
        }
        Ok(Self {
            dimensions,
            metric,
            documents: BTreeMap::new(),
        })
    }
    pub fn len(&self) -> usize {
        self.documents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
    fn validate(&self, vector: &[f32]) -> Result<(), VectorError> {
        if vector.len() != self.dimensions
            || vector.iter().any(|v| !v.is_finite())
            || (matches!(self.metric, Metric::Cosine) && vector.iter().all(|v| *v == 0.0))
        {
            return Err(VectorError::InvalidVector);
        }
        Ok(())
    }
    /// Invalid replacement leaves the old vector intact.
    pub fn upsert(&mut self, id: i64, vector: &[f32]) -> Result<(), VectorError> {
        self.validate(vector)?;
        let count = self.len() + usize::from(!self.documents.contains_key(&id));
        if count > 10_000 || count * self.dimensions > 4_194_304 {
            return Err(VectorError::Limit);
        }
        self.documents.insert(id, vector.to_vec());
        Ok(())
    }
    pub fn remove(&mut self, id: i64) -> bool {
        self.documents.remove(&id).is_some()
    }
    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<VectorHit>, VectorError> {
        self.validate(query)?;
        if limit > 100 {
            return Err(VectorError::Limit);
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut hits: Vec<_> = self
            .documents
            .iter()
            .map(|(id, vector)| {
                let distance = match self.metric {
                    Metric::SquaredEuclidean => vector
                        .iter()
                        .zip(query)
                        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
                        .sum(),
                    Metric::Cosine => {
                        // Accumulate f32 values in f64 to avoid overflow and subnormal norm loss.
                        let dot: f64 = vector
                            .iter()
                            .zip(query)
                            .map(|(a, b)| f64::from(*a) * f64::from(*b))
                            .sum();
                        let norm =
                            |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
                        (1.0 - dot / (norm(vector) * norm(query))).clamp(0.0, 2.0)
                    }
                };
                VectorHit { id: *id, distance }
            })
            .collect();
        hits.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}
