use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::identity::CharacterId;

/// A source-owned identity; equal opaque IDs from different sources never alias.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetrievalKey {
    pub character: CharacterId,
    pub source: RetrievalSource,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetrievalSource {
    Memory,
    History,
    Document,
    Sentence,
    Graph,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetrievalMethod {
    Vector,
    Keyword,
    Fts,
    Graph,
}

/// Read-only search evidence from an already scoped source. Bucket order is rank order.
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalHit {
    pub key: RetrievalKey,
    pub content: String,
    pub raw_score: Option<f64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct RetrievalBucket {
    pub method: RetrievalMethod,
    pub hits: Vec<RetrievalHit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FusionMode {
    Union,
    VectorOnly,
    Intersect {
        min_methods: usize,
        require_vector: bool,
    },
    TwoStage,
    Rrf {
        k: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FusionConfig {
    pub mode: FusionMode,
    pub limit: usize,
    pub minimum_graph: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalEvidence {
    pub method: RetrievalMethod,
    pub rank: usize,
    pub raw_score: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FusedHit {
    pub key: RetrievalKey,
    pub content: String,
    pub score: f64,
    pub evidence: Vec<RetrievalEvidence>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FusionError {
    #[error("invalid retrieval fusion configuration: {0}")]
    Configuration(&'static str),
    #[error("invalid or conflicting retrieval evidence: {0}")]
    Evidence(&'static str),
}

const MAX_HITS: usize = 1024;

/// Pure rank composition: no index, storage, provider calls, model, or fallback.
/// The caller owns source membership and must supply independently scoped buckets.
pub fn fuse(
    character: CharacterId,
    buckets: &[RetrievalBucket],
    config: FusionConfig,
) -> Result<Vec<FusedHit>, FusionError> {
    if config.limit == 0 || config.limit > MAX_HITS {
        return Err(FusionError::Configuration("limit must be within 1..=1024"));
    }
    if config.minimum_graph > config.limit {
        return Err(FusionError::Configuration(
            "graph quota exceeds result limit",
        ));
    }
    match config.mode {
        FusionMode::Intersect { min_methods, .. } if min_methods == 0 || min_methods > 4 => {
            return Err(FusionError::Configuration("invalid minimum method count"));
        }
        FusionMode::Rrf { k: 0 } => {
            return Err(FusionError::Configuration("RRF k must be positive"));
        }
        _ => {}
    }

    let mut methods = BTreeSet::new();
    let mut candidates: BTreeMap<RetrievalKey, FusedHit> = BTreeMap::new();
    for bucket in buckets {
        if !methods.insert(bucket.method) {
            return Err(FusionError::Evidence("duplicate retrieval method bucket"));
        }
        if bucket.hits.len() > MAX_HITS {
            return Err(FusionError::Evidence(
                "retrieval bucket exceeds bounded capacity",
            ));
        }
        let mut seen = BTreeSet::new();
        for (rank, hit) in bucket.hits.iter().enumerate() {
            if hit.key.character != character {
                return Err(FusionError::Evidence("cross-character retrieval hit"));
            }
            if hit.key.id.trim().is_empty() || hit.content.trim().is_empty() {
                return Err(FusionError::Evidence("empty source identity or content"));
            }
            if hit.raw_score.is_some_and(|score| !score.is_finite()) {
                return Err(FusionError::Evidence("nonfinite source score"));
            }
            if !seen.insert(hit.key.clone()) {
                return Err(FusionError::Evidence("duplicate hit within method bucket"));
            }
            let candidate = candidates
                .entry(hit.key.clone())
                .or_insert_with(|| FusedHit {
                    key: hit.key.clone(),
                    content: hit.content.clone(),
                    score: 0.0,
                    evidence: Vec::new(),
                    metadata: BTreeMap::new(),
                });
            if candidate.content != hit.content {
                return Err(FusionError::Evidence("conflicting source content"));
            }
            for (key, value) in &hit.metadata {
                if candidate
                    .metadata
                    .insert(key.clone(), value.clone())
                    .is_some_and(|previous| previous != *value)
                {
                    return Err(FusionError::Evidence("conflicting source metadata"));
                }
            }
            candidate.evidence.push(RetrievalEvidence {
                method: bucket.method,
                rank,
                raw_score: hit.raw_score,
            });
        }
    }

    let mut ranked: Vec<_> = candidates
        .into_values()
        .filter_map(|mut candidate| {
            candidate.evidence.sort_by_key(|e| e.method);
            let vector_rank = candidate
                .evidence
                .iter()
                .find(|e| e.method == RetrievalMethod::Vector)
                .map(|e| e.rank);
            let admitted = match config.mode {
                FusionMode::Union | FusionMode::Rrf { .. } => true,
                FusionMode::VectorOnly | FusionMode::TwoStage => vector_rank.is_some(),
                FusionMode::Intersect {
                    min_methods,
                    require_vector,
                } => {
                    candidate.evidence.len() >= min_methods
                        && (!require_vector || vector_rank.is_some())
                }
            };
            if !admitted {
                return None;
            }
            candidate.score = match config.mode {
                FusionMode::VectorOnly | FusionMode::TwoStage => {
                    1.0 / (vector_rank.expect("admitted vector hit") + 1) as f64
                }
                FusionMode::Rrf { k } => candidate
                    .evidence
                    .iter()
                    .map(|e| 1.0 / (k as f64 + e.rank as f64 + 1.0))
                    .sum(),
                FusionMode::Union | FusionMode::Intersect { .. } => candidate
                    .evidence
                    .iter()
                    .map(|e| 1.0 / (e.rank + 1) as f64)
                    .sum(),
            };
            Some(candidate)
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.key.cmp(&right.key))
    });

    if config.minimum_graph > 0 {
        let graph_ids = ranked
            .iter()
            .filter(|hit| hit.key.source == RetrievalSource::Graph)
            .take(config.minimum_graph)
            .map(|hit| hit.key.clone())
            .collect::<BTreeSet<_>>();
        let graph_reserved = ranked
            .iter()
            .filter(|hit| graph_ids.contains(&hit.key))
            .cloned()
            .collect::<Vec<_>>();
        let remaining = ranked
            .into_iter()
            .filter(|hit| !graph_ids.contains(&hit.key))
            .take(config.limit - graph_reserved.len());
        ranked = graph_reserved.into_iter().chain(remaining).collect();
        ranked.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
    } else {
        ranked.truncate(config.limit);
    }
    Ok(ranked)
}
