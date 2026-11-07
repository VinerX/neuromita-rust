//! Typed cross-encoder scores from a selected Python reranker, without fusion policy.

use std::sync::Arc;

use serde_json::json;
use thiserror::Error;

use crate::ai_runtime::{AiRuntimeError, AiService, AiSupervisor};

#[derive(Debug, Error)]
pub enum RerankError {
    #[error("invalid reranking request: {0}")]
    Invalid(String),
    #[error("invalid reranker response: {0}")]
    Response(String),
    #[error(transparent)]
    Runtime(#[from] AiRuntimeError),
}

pub struct PythonReranker {
    supervisor: Arc<AiSupervisor>,
    model_id: String,
    protocol: String,
}

impl PythonReranker {
    pub fn new(
        supervisor: Arc<AiSupervisor>,
        model_id: String,
        protocol: String,
    ) -> Result<Self, RerankError> {
        if model_id.trim().is_empty()
            || protocol.trim().is_empty()
            || model_id.len() > 256
            || protocol.len() > 256
        {
            return Err(RerankError::Invalid(
                "bounded model and protocol required".into(),
            ));
        }
        Ok(Self {
            supervisor,
            model_id,
            protocol,
        })
    }

    pub async fn score(&self, query: &str, candidates: &[String]) -> Result<Vec<f64>, RerankError> {
        if query.trim().is_empty()
            || query.len() > 8192
            || candidates.is_empty()
            || candidates.len() > 32
            || candidates
                .iter()
                .any(|item| item.trim().is_empty() || item.len() > 8192)
        {
            return Err(RerankError::Invalid(
                "one to 32 nonempty candidate texts and a bounded query required".into(),
            ));
        }
        let result = self
            .supervisor
            .call(
                AiService::Reranker,
                "infer",
                json!({
                    "kind": "reranker", "input": {"query": query, "candidates": candidates}
                }),
            )
            .await?;
        if result["model"] != self.model_id || result["protocol"] != self.protocol {
            return Err(RerankError::Response("reranker identity mismatch".into()));
        }
        let scores = result["scores"]
            .as_array()
            .ok_or_else(|| RerankError::Response("scores missing".into()))?;
        if scores.len() != candidates.len() {
            return Err(RerankError::Response("score count mismatch".into()));
        }
        scores
            .iter()
            .map(|value| {
                let score = value
                    .as_f64()
                    .ok_or_else(|| RerankError::Response("invalid score".into()))?;
                if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                    return Err(RerankError::Response("score outside [0,1]".into()));
                }
                Ok(score)
            })
            .collect()
    }
}

impl crate::knowledge::reranking::RerankProvider for PythonReranker {
    fn score<'a>(
        &'a self,
        query: &'a str,
        candidates: &'a [String],
    ) -> crate::knowledge::reranking::RerankFuture<'a> {
        Box::pin(async move {
            self.score(query, candidates)
                .await
                .map_err(|error| match error {
                    RerankError::Invalid(message) => {
                        crate::knowledge::embedding::EmbeddingError::Configuration(message)
                    }
                    RerankError::Response(message) => {
                        crate::knowledge::embedding::EmbeddingError::Response(message)
                    }
                    RerankError::Runtime(error) => {
                        crate::knowledge::embedding::EmbeddingError::Transport(error.to_string())
                    }
                })
        })
    }
}
