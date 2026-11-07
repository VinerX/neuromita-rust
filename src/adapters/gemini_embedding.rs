use std::{sync::Arc, time::Duration};

use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::knowledge::embedding::{
    Embedding, EmbeddingError, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiEmbeddingProtocol {
    Embedding001,
    Embedding2,
}

pub struct GeminiEmbeddingProvider {
    client: reqwest::Client,
    endpoint: Url,
    api_key: Arc<str>,
    protocol: GeminiEmbeddingProtocol,
    space: EmbeddingSpace,
    timeout: Duration,
}

impl GeminiEmbeddingProvider {
    pub fn google(
        protocol: GeminiEmbeddingProtocol,
        api_key: &str,
        dimensions: usize,
    ) -> Result<Self, EmbeddingError> {
        Self::new(
            "https://generativelanguage.googleapis.com/v1beta/",
            protocol,
            api_key,
            dimensions,
            Duration::from_secs(30),
        )
    }

    pub fn new(
        endpoint: &str,
        protocol: GeminiEmbeddingProtocol,
        api_key: &str,
        dimensions: usize,
        timeout: Duration,
    ) -> Result<Self, EmbeddingError> {
        let url = Url::parse(endpoint)
            .map_err(|_| EmbeddingError::Configuration("invalid Gemini endpoint".into()))?;
        let local = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if (url.scheme() != "https" && !(url.scheme() == "http" && local))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.path().ends_with('/')
            || timeout.is_zero()
            || api_key.is_empty()
            || api_key.chars().any(|c| c.is_control() || c.is_whitespace())
            || !(128..=3072).contains(&dimensions)
        {
            return Err(EmbeddingError::Configuration(
                "invalid endpoint, credentials, dimension or timeout".into(),
            ));
        }
        let (model, version) = match protocol {
            GeminiEmbeddingProtocol::Embedding001 => {
                ("gemini-embedding-001", "embedContent:taskType-v1")
            }
            GeminiEmbeddingProtocol::Embedding2 => {
                ("gemini-embedding-2", "embedContent:instruction-v2")
            }
        };
        let space = EmbeddingSpace::new("gemini", model, version, dimensions)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| EmbeddingError::Transport(e.without_url().to_string()))?;
        Ok(Self {
            client,
            endpoint: url,
            api_key: Arc::from(api_key),
            protocol,
            space,
            timeout,
        })
    }

    fn endpoint(&self, action: &str) -> Url {
        let mut url = self
            .endpoint
            .join(&format!("models/{}:{action}", self.space.model))
            .expect("validated endpoint");
        url.query_pairs_mut().append_pair("key", &self.api_key);
        url
    }

    fn content(&self, purpose: EmbeddingPurpose, text: &str) -> Value {
        let text = match (self.protocol, purpose) {
            (GeminiEmbeddingProtocol::Embedding2, EmbeddingPurpose::Query) => {
                format!("task: search result | query: {text}")
            }
            (GeminiEmbeddingProtocol::Embedding2, EmbeddingPurpose::Document) => {
                format!("title: none | text: {text}")
            }
            _ => text.to_owned(),
        };
        json!({"parts": [{"text": text}]})
    }

    fn request(&self, purpose: EmbeddingPurpose, text: &str) -> Value {
        let mut request = json!({
            "model": format!("models/{}", self.space.model),
            "content": self.content(purpose, text),
            "outputDimensionality": self.space.dimensions,
        });
        if self.protocol == GeminiEmbeddingProtocol::Embedding001 {
            request["taskType"] = json!(match purpose {
                EmbeddingPurpose::Query => "RETRIEVAL_QUERY",
                EmbeddingPurpose::Document => "RETRIEVAL_DOCUMENT",
            });
        }
        request
    }

    async fn post(&self, action: &str, body: Value) -> Result<Value, EmbeddingError> {
        let mut response = self
            .client
            .post(self.endpoint(action))
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingError::Transport(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(EmbeddingError::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| EmbeddingError::Transport(e.without_url().to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(EmbeddingError::Response(
                    "embedding response exceeds 8 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| EmbeddingError::Response("invalid embedding JSON".into()))
    }

    fn parse_vector(&self, result: &Value) -> Result<Embedding, EmbeddingError> {
        #[derive(Deserialize)]
        struct Vector {
            values: Vec<f32>,
        }
        let vector: Vector = serde_json::from_value(result.clone())
            .map_err(|_| EmbeddingError::Response("missing embedding values".into()))?;
        Embedding::new(vector.values, &self.space)
    }
}

impl EmbeddingProvider for GeminiEmbeddingProvider {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    fn embed(&self, purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            if text.trim().is_empty() {
                return Err(EmbeddingError::Configuration(
                    "embedding text must not be empty".into(),
                ));
            }
            let result = self
                .post("embedContent", self.request(purpose, &text))
                .await?;
            self.parse_vector(
                result
                    .get("embedding")
                    .ok_or_else(|| EmbeddingError::Response("embedding missing".into()))?,
            )
        })
    }

    fn document_request_count(&self, document_count: usize) -> usize {
        document_count.div_ceil(64)
    }

    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(async move {
            if texts.iter().any(|text| text.trim().is_empty()) {
                return Err(EmbeddingError::Configuration(
                    "document embedding text is empty".into(),
                ));
            }
            let mut results = Vec::with_capacity(texts.len());
            for batch in texts.chunks(64) {
                let requests = batch
                    .iter()
                    .map(|text| self.request(EmbeddingPurpose::Document, text))
                    .collect::<Vec<_>>();
                let response = self
                    .post("batchEmbedContents", json!({"requests":requests}))
                    .await?;
                let vectors = response
                    .get("embeddings")
                    .and_then(Value::as_array)
                    .ok_or_else(|| EmbeddingError::Response("batch embeddings missing".into()))?;
                if vectors.len() != batch.len() {
                    return Err(EmbeddingError::Response(
                        "batch embedding count mismatch".into(),
                    ));
                }
                for vector in vectors {
                    results.push(self.parse_vector(vector)?);
                }
            }
            Ok(results)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_protocol_payloads_and_spaces() {
        let a = GeminiEmbeddingProvider::google(
            GeminiEmbeddingProtocol::Embedding001,
            "secret-key",
            768,
        )
        .unwrap();
        let b =
            GeminiEmbeddingProvider::google(GeminiEmbeddingProtocol::Embedding2, "secret-key", 768)
                .unwrap();
        assert_ne!(a.space, b.space);
        assert_eq!(
            a.request(EmbeddingPurpose::Query, "tea")["taskType"],
            "RETRIEVAL_QUERY"
        );
        assert_eq!(
            a.request(EmbeddingPurpose::Document, "tea")["taskType"],
            "RETRIEVAL_DOCUMENT"
        );
        assert!(
            b.request(EmbeddingPurpose::Query, "tea")
                .get("taskType")
                .is_none()
        );
        assert!(
            b.request(EmbeddingPurpose::Query, "tea")["content"]
                .to_string()
                .contains("task: search result")
        );
        assert!(
            !a.endpoint("embedContent")
                .to_string()
                .contains("not-the-secret")
        );
        assert!(
            GeminiEmbeddingProvider::google(GeminiEmbeddingProtocol::Embedding001, "", 768)
                .is_err()
        );
        assert!(
            GeminiEmbeddingProvider::google(GeminiEmbeddingProtocol::Embedding001, "key", 4)
                .is_err()
        );
    }
}
