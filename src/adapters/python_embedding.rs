//! Python AI worker bridge for Rust's authoritative snapshot-aware semantic retrieval.

use std::sync::Arc;

use serde_json::json;

use crate::{
    ai_runtime::{AiService, AiSupervisor},
    knowledge::embedding::{
        Embedding, EmbeddingError, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose,
        EmbeddingSpace,
    },
};

pub struct PythonEmbeddingProvider {
    supervisor: Arc<AiSupervisor>,
    space: EmbeddingSpace,
}

impl PythonEmbeddingProvider {
    pub fn new(supervisor: Arc<AiSupervisor>, space: EmbeddingSpace) -> Self {
        Self { supervisor, space }
    }

    async fn request(
        &self,
        purpose: EmbeddingPurpose,
        texts: Vec<String>,
    ) -> Result<Vec<Embedding>, EmbeddingError> {
        if texts.is_empty()
            || texts.len() > 32
            || texts
                .iter()
                .any(|text| text.trim().is_empty() || text.len() > 8192 || text.contains('\0'))
        {
            return Err(EmbeddingError::Configuration(
                "one to 32 nonempty bounded embedding texts required".into(),
            ));
        }
        let result = self
            .supervisor
            .call(
                AiService::Embedding,
                "infer",
                json!({
                    "kind": "embedding",
                    "input": {
                        "purpose": match purpose { EmbeddingPurpose::Query => "query", EmbeddingPurpose::Document => "document" },
                        "texts": texts,
                    },
                }),
            )
            .await
            .map_err(|error| EmbeddingError::Transport(error.to_string()))?;
        if result["model"] != self.space.model
            || result["protocol"] != self.space.protocol
            || result["dimensions"].as_u64() != Some(self.space.dimensions as u64)
        {
            return Err(EmbeddingError::Response(
                "Python embedding space does not match the active selection".into(),
            ));
        }
        let vectors = result["vectors"]
            .as_array()
            .ok_or_else(|| EmbeddingError::Response("missing embedding vectors".into()))?;
        if vectors.len() != texts.len() {
            return Err(EmbeddingError::Response(
                "embedding response count mismatch".into(),
            ));
        }
        vectors
            .iter()
            .map(|vector| {
                let values = vector
                    .as_array()
                    .ok_or_else(|| EmbeddingError::Response("invalid vector".into()))?;
                if values.len() != self.space.dimensions {
                    return Err(EmbeddingError::Response("embedding width mismatch".into()));
                }
                let coordinates: Vec<f32> = values
                    .iter()
                    .map(|value| {
                        let number = value
                            .as_f64()
                            .ok_or_else(|| EmbeddingError::Response("invalid coordinate".into()))?;
                        if !number.is_finite() || number.abs() > f32::MAX as f64 {
                            return Err(EmbeddingError::Response(
                                "nonfinite embedding coordinate".into(),
                            ));
                        }
                        Ok(number as f32)
                    })
                    .collect::<Result<_, _>>()?;
                Embedding::new(coordinates, &self.space)
            })
            .collect()
    }
}

impl EmbeddingProvider for PythonEmbeddingProvider {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    fn embed(&self, purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            let mut values = self.request(purpose, vec![text]).await?;
            Ok(values.remove(0))
        })
    }

    fn document_request_count(&self, document_count: usize) -> usize {
        document_count.div_ceil(32)
    }

    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(async move {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let mut embeddings = Vec::with_capacity(texts.len());
            for chunk in texts.chunks(32) {
                embeddings.extend(
                    self.request(EmbeddingPurpose::Document, chunk.to_vec())
                        .await?,
                );
            }
            Ok(embeddings)
        })
    }
}
