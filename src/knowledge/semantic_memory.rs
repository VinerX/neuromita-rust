use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use super::{
    embedding::{Embedding, EmbeddingError, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace},
    memory_context::{lexical_ranked, priority_rank, render_ranked_memory_context, tokens},
    reranking::{RerankPolicy, RerankProvider, rerank_prefix},
    retrieval_fusion::{
        FusionConfig, FusionMode, RetrievalBucket, RetrievalHit, RetrievalKey, RetrievalMethod,
        RetrievalSource, fuse,
    },
};
use crate::{
    identity::CharacterId,
    memory::{MemoryKind, MemoryState},
    timeline::domain::{MemoryId, MemoryRecord},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    character: CharacterId,
    space: EmbeddingSpace,
    id: MemoryId,
    content: String,
}

struct EmbeddingCache {
    entries: HashMap<CacheKey, Embedding>,
    order: VecDeque<CacheKey>,
}

impl EmbeddingCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    fn insert(&mut self, key: CacheKey, embedding: Embedding, max: usize) {
        if self.entries.contains_key(&key) {
            return;
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, embedding);
        while self.order.len() > max {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalCost {
    pub embedding_requests: usize,
    pub embedded_texts: usize,
    pub embedded_characters: usize,
    pub cache_hits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticRetrievalFilter {
    pub minimum_cosine: Option<f64>,
}

impl SemanticRetrievalFilter {
    pub const fn all() -> Self {
        Self {
            minimum_cosine: None,
        }
    }

    fn validate(self) -> Result<Self, EmbeddingError> {
        if self
            .minimum_cosine
            .is_some_and(|score| !score.is_finite() || !(-1.0..=1.0).contains(&score))
        {
            return Err(EmbeddingError::Configuration(
                "minimum cosine must be finite and within [-1, 1]".into(),
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalStatus {
    EmptyCorpus,
    NoMatches,
    Selected,
}

/// One admitted Timeline memory. The ID identifies the source; the embedding
/// space in RetrievedMemory identifies how its similarity was evaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedMemoryMatch {
    pub id: MemoryId,
    pub cosine_similarity: f64,
    pub fusion_score: f64,
    pub methods: Vec<RetrievalMethod>,
}

#[derive(Debug)]
pub struct RetrievedMemory {
    pub rendered: Option<String>,
    pub selected_ids: Vec<MemoryId>,
    pub matches: Vec<RetrievedMemoryMatch>,
    pub space: EmbeddingSpace,
    pub status: RetrievalStatus,
    pub cost: RetrievalCost,
}

/// A bounded, derived embedding cache. Membership always comes from the captured Timeline projection.
/// No network await is performed while holding the cache lock.
pub struct SemanticMemoryRetriever {
    provider: Arc<dyn EmbeddingProvider>,
    cache: Mutex<EmbeddingCache>,
    max_entries: usize,
    reranker: Option<(Arc<dyn RerankProvider>, RerankPolicy)>,
}

impl SemanticMemoryRetriever {
    pub fn new(
        provider: Arc<dyn EmbeddingProvider>,
        max_entries: usize,
    ) -> Result<Self, EmbeddingError> {
        if max_entries == 0 {
            return Err(EmbeddingError::Configuration(
                "cache capacity must be positive".into(),
            ));
        }
        Ok(Self {
            provider,
            cache: Mutex::new(EmbeddingCache::new()),
            max_entries,
            reranker: None,
        })
    }

    pub fn with_reranker(
        mut self,
        provider: Arc<dyn RerankProvider>,
        policy: RerankPolicy,
    ) -> Result<Self, EmbeddingError> {
        self.reranker = Some((provider, policy.validate()?));
        Ok(self)
    }

    pub fn clear_cache(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.entries.clear();
        cache.order.clear();
    }

    pub async fn retrieve(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_filtered(character, memories, query, SemanticRetrievalFilter::all())
            .await
    }

    /// Explicitly filter cosine similarity without changing the captured source
    /// membership, the embedding cache or the provider error policy.
    pub async fn retrieve_filtered(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
        filter: SemanticRetrievalFilter,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_with_fusion(character, memories, query, filter, FusionMode::VectorOnly)
            .await
    }

    /// Explicit source fusion over the same captured memory projection. There is
    /// no second index or provider; the lexical bucket is computed locally.
    pub async fn retrieve_with_fusion(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
        filter: SemanticRetrievalFilter,
        mode: FusionMode,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        let filter = filter.validate()?;
        let config = FusionConfig {
            mode,
            limit: 1024,
            minimum_graph: 0,
        };
        fuse(character, &[], config).map_err(|e| EmbeddingError::Configuration(e.to_string()))?;
        let memories = memories
            .iter()
            .filter(|record| record.kind == MemoryKind::Fact && record.state == MemoryState::Active)
            .cloned()
            .collect::<Vec<_>>();
        let space = self.provider.space().clone();
        let mut cost = RetrievalCost {
            embedding_requests: 0,
            embedded_texts: 0,
            embedded_characters: 0,
            cache_hits: 0,
        };
        if memories.is_empty() {
            return Ok(RetrievedMemory {
                rendered: None,
                selected_ids: Vec::new(),
                matches: Vec::new(),
                space,
                status: RetrievalStatus::EmptyCorpus,
                cost,
            });
        }
        let keys: Vec<_> = memories
            .iter()
            .map(|memory| CacheKey {
                character,
                space: space.clone(),
                id: memory.id,
                content: memory.content.clone(),
            })
            .collect();
        let mut vectors: Vec<Option<Embedding>> = vec![None; memories.len()];
        let mut missing = Vec::new();
        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for (index, key) in keys.iter().enumerate() {
                if let Some(vector) = cache.entries.get(key) {
                    cost.cache_hits += 1;
                    vectors[index] = Some(vector.clone());
                } else {
                    missing.push(index);
                }
            }
        }
        let query = query.chars().take(2048).collect::<String>();
        if query.trim().is_empty() {
            return Err(EmbeddingError::Configuration(
                "semantic query is empty".into(),
            ));
        }
        let query_vector = self
            .provider
            .embed(EmbeddingPurpose::Query, query.clone())
            .await?;
        cost.embedded_characters += query.chars().count();
        cost.embedding_requests += 1;
        cost.embedded_texts += 1;
        if !missing.is_empty() {
            let texts = missing
                .iter()
                .map(|&i| memories[i].content.clone())
                .collect::<Vec<_>>();
            cost.embedded_characters +=
                texts.iter().map(|text| text.chars().count()).sum::<usize>();
            let fresh = self.provider.embed_documents(texts).await?;
            cost.embedding_requests += self.provider.document_request_count(missing.len());
            cost.embedded_texts += missing.len();
            if fresh.len() != missing.len() {
                return Err(EmbeddingError::Response(
                    "document embedding count mismatch".into(),
                ));
            }
            for (&index, vector) in missing.iter().zip(fresh) {
                if vector.values.len() != space.dimensions
                    || vector.values.iter().any(|v| !v.is_finite())
                {
                    return Err(EmbeddingError::Response(
                        "invalid document embedding".into(),
                    ));
                }
                vectors[index] = Some(vector.clone());
                let mut cache = self
                    .cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                cache.insert(keys[index].clone(), vector, self.max_entries);
            }
        }
        if query_vector.values.len() != space.dimensions
            || query_vector.values.iter().any(|v| !v.is_finite())
        {
            return Err(EmbeddingError::Response("invalid query embedding".into()));
        }
        let mut ranked = memories
            .iter()
            .zip(vectors.iter())
            .map(|(m, v)| {
                (
                    m,
                    v.as_ref()
                        .expect("filled embedding")
                        .similarity(&query_vector),
                )
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|(a, ascore), (b, bscore)| {
            bscore
                .total_cmp(ascore)
                .then_with(|| priority_rank(b.priority).cmp(&priority_rank(a.priority)))
                .then_with(|| a.id.cmp(&b.id))
        });
        let scores = ranked
            .iter()
            .map(|(memory, score)| (memory.id, *score))
            .collect::<HashMap<_, _>>();
        let vector_hits = ranked
            .iter()
            .filter(|(_, score)| filter.minimum_cosine.is_none_or(|min| *score >= min))
            .map(|(memory, score)| RetrievalHit {
                key: RetrievalKey {
                    character,
                    source: RetrievalSource::Memory,
                    id: memory.id.0.to_string(),
                },
                content: memory.content.clone(),
                raw_score: Some(*score),
                metadata: BTreeMap::new(),
            })
            .collect::<Vec<_>>();
        let mut buckets = vec![RetrievalBucket {
            method: RetrievalMethod::Vector,
            hits: vector_hits,
        }];
        if !matches!(mode, FusionMode::VectorOnly) {
            let query_tokens = tokens(&query);
            let lexical_hits = lexical_ranked(&memories, &query)
                .into_iter()
                .filter(|memory| {
                    tokens(&memory.content)
                        .intersection(&query_tokens)
                        .next()
                        .is_some()
                })
                .map(|memory| RetrievalHit {
                    key: RetrievalKey {
                        character,
                        source: RetrievalSource::Memory,
                        id: memory.id.0.to_string(),
                    },
                    content: memory.content.clone(),
                    raw_score: None,
                    metadata: BTreeMap::new(),
                })
                .collect();
            buckets.push(RetrievalBucket {
                method: RetrievalMethod::Keyword,
                hits: lexical_hits,
            });
        }
        let mut fused = fuse(character, &buckets, config)
            .map_err(|error| EmbeddingError::Response(error.to_string()))?;
        if let Some((provider, policy)) = &self.reranker {
            rerank_prefix(provider.as_ref(), &query, &mut fused, *policy).await?;
        }
        let by_id = memories
            .iter()
            .map(|memory| (memory.id.0.to_string(), memory))
            .collect::<HashMap<_, _>>();
        let (rendered, selected_ids) = render_ranked_memory_context(
            fused
                .iter()
                .filter_map(|hit| by_id.get(&hit.key.id).copied()),
        );
        let selected_set = selected_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let matches = fused
            .into_iter()
            .filter_map(|hit| {
                let memory = by_id.get(&hit.key.id)?;
                selected_set
                    .contains(&memory.id)
                    .then(|| RetrievedMemoryMatch {
                        id: memory.id,
                        cosine_similarity: scores[&memory.id],
                        fusion_score: hit.score,
                        methods: hit.evidence.into_iter().map(|e| e.method).collect(),
                    })
            })
            .collect();
        let status = if selected_ids.is_empty() {
            RetrievalStatus::NoMatches
        } else {
            RetrievalStatus::Selected
        };
        Ok(RetrievedMemory {
            rendered,
            selected_ids,
            matches,
            space,
            status,
            cost,
        })
    }
}
