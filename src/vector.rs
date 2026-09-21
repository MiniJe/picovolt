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
        self.search_matching(query, limit, None)
    }
    fn search_matching(
        &self,
        query: &[f32],
        limit: usize,
        allowed: Option<&std::collections::BTreeSet<i64>>,
    ) -> Result<Vec<VectorHit>, VectorError> {
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
            .filter(|(id, _)| allowed.map_or(true, |ids| ids.contains(id)))
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

// PV23 binary codec and exact filtered candidate execution.
impl VectorIndex {
    pub(crate) fn search_filtered(
        &self,
        query: &[f32],
        limit: usize,
        ids: &std::collections::BTreeSet<i64>,
    ) -> Result<Vec<VectorHit>, VectorError> {
        if ids.len() > 10_000 || ids.iter().any(|id| !self.documents.contains_key(id)) {
            return Err(VectorError::InvalidVector);
        }
        self.search_matching(query, limit, Some(ids))
    }
    pub(crate) fn encode_persistent(&self) -> crate::Result<Vec<u8>> {
        let mut out = crate::persistent::Encoder::new();
        out.count(self.dimensions)?;
        out.count(self.len())?;
        let metric = match self.metric {
            Metric::Cosine => 1,
            Metric::SquaredEuclidean => 2,
        };
        out.put(&[metric, 0, 0, 0])?;
        for (id, vector) in &self.documents {
            out.put(&id.to_le_bytes())?;
            for value in vector {
                out.put(&value.to_le_bytes())?;
            }
        }
        Ok(out.finish())
    }
    pub(crate) fn decode_persistent(
        bytes: &[u8],
        dimensions: usize,
        metric: Metric,
    ) -> crate::Result<Self> {
        use crate::persistent::{corrupt, Decoder};
        let mut input = Decoder::new(bytes)?;
        let actual_dimensions = input.count(4096)?;
        let count = input.count(10_000)?;
        let actual_metric = input.u8()?;
        let expected_metric = match metric {
            Metric::Cosine => 1,
            Metric::SquaredEuclidean => 2,
        };
        if actual_dimensions == 0
            || actual_dimensions != dimensions
            || actual_metric != expected_metric
            || input.take(3)? != [0, 0, 0]
        {
            return Err(corrupt("vector header/definition mismatch"));
        }
        let scalars = count
            .checked_mul(dimensions)
            .filter(|count| *count <= 4_194_304)
            .ok_or_else(|| corrupt("vector scalar budget"))?;
        let expected_bytes = scalars
            .checked_mul(4)
            .and_then(|value| value.checked_add(count * 8))
            .and_then(|value| value.checked_add(12))
            .ok_or_else(|| corrupt("vector extent overflow"))?;
        if expected_bytes != bytes.len() {
            return Err(corrupt("vector extent/count mismatch"));
        }
        let mut index =
            Self::new(dimensions, metric).map_err(|error| corrupt(error.to_string()))?;
        let mut last_id = None;
        for _ in 0..count {
            let id = input.i64()?;
            if last_id.is_some_and(|previous| previous >= id) {
                return Err(corrupt("vector IDs are not strictly ordered"));
            }
            last_id = Some(id);
            let mut vector = Vec::with_capacity(dimensions);
            for _ in 0..dimensions {
                vector.push(f32::from_le_bytes(
                    input
                        .take(4)?
                        .try_into()
                        .map_err(|_| corrupt("vector scalar"))?,
                ));
            }
            index
                .upsert(id, &vector)
                .map_err(|error| corrupt(error.to_string()))?;
        }
        input.finish()?;
        Ok(index)
    }
}
