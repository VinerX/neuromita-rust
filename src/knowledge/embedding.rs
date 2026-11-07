use std::{future::Future, pin::Pin, sync::Arc};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingSpace {
    pub backend: String,
    pub model: String,
    pub protocol: String,
    pub dimensions: usize,
}

impl EmbeddingSpace {
    pub fn new(
        backend: &str,
        model: &str,
        protocol: &str,
        dimensions: usize,
    ) -> Result<Self, EmbeddingError> {
        if [backend, model, protocol]
            .iter()
            .any(|s| s.trim().is_empty())
            || !(1..=3072).contains(&dimensions)
        {
            return Err(EmbeddingError::Configuration(
                "invalid embedding space".into(),
            ));
        }
        Ok(Self {
            backend: backend.into(),
            model: model.into(),
            protocol: protocol.into(),
            dimensions,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingPurpose {
    Query,
    Document,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum EmbeddingError {
    #[error("invalid embedding configuration: {0}")]
    Configuration(String),
    #[error("embedding transport failure: {0}")]
    Transport(String),
    #[error("embedding provider HTTP {0}")]
    Http(u16),
    #[error("invalid embedding result: {0}")]
    Response(String),
}

#[derive(Debug, Clone)]
pub struct Embedding {
    pub(crate) values: Arc<[f32]>,
}

impl Embedding {
    pub fn new(values: Vec<f32>, space: &EmbeddingSpace) -> Result<Self, EmbeddingError> {
        if values.len() != space.dimensions || !values.iter().all(|value| value.is_finite()) {
            return Err(EmbeddingError::Response(
                "nonfinite or dimension-mismatched vector".into(),
            ));
        }
        let norm = values
            .iter()
            .map(|v| f64::from(*v) * f64::from(*v))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return Err(EmbeddingError::Response(
                "zero or invalid vector norm".into(),
            ));
        }
        Ok(Self {
            values: values
                .into_iter()
                .map(|value| (f64::from(value) / norm) as f32)
                .collect(),
        })
    }

    pub(crate) fn similarity(&self, other: &Self) -> f64 {
        self.values
            .iter()
            .zip(other.values.iter())
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum()
    }
}

pub type EmbeddingFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, EmbeddingError>> + Send + 'a>>;

pub trait EmbeddingProvider: Send + Sync {
    fn space(&self) -> &EmbeddingSpace;
    fn embed(&self, purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding>;
    /// Number of actual HTTP or in-process calls used for a successful document batch.
    fn document_request_count(&self, document_count: usize) -> usize {
        document_count
    }
    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(async move {
            let mut embeddings = Vec::with_capacity(texts.len());
            for text in texts {
                embeddings.push(self.embed(EmbeddingPurpose::Document, text).await?);
            }
            Ok(embeddings)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_nonfinite_zero_and_dimension_mismatch() {
        let space = EmbeddingSpace::new("backend", "model", "protocol", 2).unwrap();
        assert!(Embedding::new(vec![0.0, 0.0], &space).is_err());
        assert!(Embedding::new(vec![1.0], &space).is_err());
        assert!(Embedding::new(vec![1.0, f32::NAN], &space).is_err());
        let x = Embedding::new(vec![3.0, 4.0], &space).unwrap();
        assert!((x.similarity(&x) - 1.0).abs() < 1e-6);
    }
}
