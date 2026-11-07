//! A bounded, optional scoring port. The authoritative source, admission and order
//! of retrieval hits remain in Rust; external models only return aligned scores.

use std::{future::Future, pin::Pin};

use super::{embedding::EmbeddingError, retrieval_fusion::FusedHit};

pub type RerankFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<f64>, EmbeddingError>> + Send + 'a>>;

pub trait RerankProvider: Send + Sync {
    fn score<'a>(&'a self, query: &'a str, candidates: &'a [String]) -> RerankFuture<'a>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RerankPolicy {
    pub alpha: f64,
    pub limit: usize,
}

impl RerankPolicy {
    pub fn validate(self) -> Result<Self, EmbeddingError> {
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            return Err(EmbeddingError::Configuration(
                "reranking alpha must be in [0,1]".into(),
            ));
        }
        if !(1..=32).contains(&self.limit) {
            return Err(EmbeddingError::Configuration(
                "reranking limit must be within 1..=32".into(),
            ));
        }
        Ok(self)
    }
}

/// Rerank a candidate prefix without introducing or deleting any source identities.
/// An unavailable or malformed model fails the retrieval explicitly.
pub async fn rerank_prefix(
    provider: &dyn RerankProvider,
    query: &str,
    hits: &mut [FusedHit],
    policy: RerankPolicy,
) -> Result<(), EmbeddingError> {
    let policy = policy.validate()?;
    if hits.is_empty() {
        return Ok(());
    }
    let count = policy.limit.min(hits.len());
    let (ranked, _) = hits.split_at_mut(count);
    let texts = ranked
        .iter()
        .map(|hit| hit.content.clone())
        .collect::<Vec<_>>();
    let scores = provider.score(query, &texts).await?;
    if scores.len() != count
        || scores
            .iter()
            .any(|score| !score.is_finite() || !(0.0..=1.0).contains(score))
    {
        return Err(EmbeddingError::Response(
            "invalid reranker score count or range".into(),
        ));
    }
    let minimum = ranked
        .iter()
        .map(|hit| hit.score)
        .fold(f64::INFINITY, f64::min);
    let maximum = ranked
        .iter()
        .map(|hit| hit.score)
        .fold(f64::NEG_INFINITY, f64::max);
    let span = maximum - minimum;
    for (hit, neural) in ranked.iter_mut().zip(scores) {
        let linear = if span > 1e-9 {
            (hit.score - minimum) / span
        } else {
            0.5
        };
        hit.score = policy.alpha * neural + (1.0 - policy.alpha) * linear;
    }
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(())
}
