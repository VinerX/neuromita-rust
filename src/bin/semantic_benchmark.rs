use std::{collections::HashSet, env, fs, sync::Arc, time::Instant};

use neuromita::{
    adapters::gemini_embedding::{GeminiEmbeddingProtocol, GeminiEmbeddingProvider},
    character::CharacterId,
    knowledge::{
        embedding::EmbeddingProvider,
        memory_context::lexical_selected_ids,
        semantic_memory::{RetrievalCost, SemanticMemoryRetriever},
    },
    timeline::domain::{MemoryId, MemoryPriority, MemoryRecord},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkDataset {
    cases: Vec<BenchmarkCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkCase {
    query: String,
    character: CharacterId,
    memories: Vec<BenchmarkMemory>,
    relevant_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkMemory {
    id: Uuid,
    content: String,
    priority: FixturePriority,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FixturePriority {
    Low,
    Normal,
    High,
    Critical,
}

impl From<FixturePriority> for MemoryPriority {
    fn from(priority: FixturePriority) -> Self {
        match priority {
            FixturePriority::Low => Self::Low,
            FixturePriority::Normal => Self::Normal,
            FixturePriority::High => Self::High,
            FixturePriority::Critical => Self::Critical,
        }
    }
}

#[derive(Default, Serialize)]
struct BenchmarkCounts {
    queries: usize,
    lexical_hit_at_1: usize,
    lexical_hit_at_3: usize,
    semantic_hit_at_1: usize,
    semantic_hit_at_3: usize,
    semantic_duration_ms: u128,
    embedding_requests: usize,
    embedded_texts: usize,
    embedded_characters: usize,
    cache_hits: usize,
}

#[derive(Serialize)]
struct BenchmarkReport {
    backend: String,
    model: String,
    protocol: String,
    dimensions: usize,
    live: bool,
    corpus_cases: usize,
    counts: BenchmarkCounts,
    lexical_hit_rate_at_1: f64,
    lexical_hit_rate_at_3: f64,
    semantic_hit_rate_at_1: f64,
    semantic_hit_rate_at_3: f64,
    mean_semantic_ms: f64,
    billing_tokens: Option<usize>,
    cost_usd: Option<f64>,
}

fn hit_at(ids: &[MemoryId], relevant: &HashSet<MemoryId>, k: usize) -> bool {
    ids.iter().take(k).any(|id| relevant.contains(id))
}

fn hit_rate(hits: usize, queries: usize) -> f64 {
    hits as f64 / queries as f64
}

fn validate(case: &BenchmarkCase) -> Result<(), String> {
    if case.query.trim().is_empty() || case.query.chars().count() > 2048 {
        return Err("query must contain 1..=2048 characters".into());
    }
    if case.memories.is_empty() || case.memories.len() > 256 {
        return Err("memories must contain 1..=256 records".into());
    }
    let mut ids = HashSet::new();
    for memory in &case.memories {
        if memory.content.trim().is_empty() || !ids.insert(memory.id) {
            return Err("memory content must not be empty and IDs must be unique".into());
        }
    }
    if case.relevant_ids.is_empty() || case.relevant_ids.iter().any(|id| !ids.contains(id)) {
        return Err("relevant_ids must be nonempty and present in memories".into());
    }
    Ok(())
}

async fn benchmark(
    data: BenchmarkDataset,
    provider: Arc<dyn EmbeddingProvider>,
) -> Result<BenchmarkReport, String> {
    if data.cases.is_empty() {
        return Err("benchmark must contain at least one case".into());
    }
    for (i, case) in data.cases.iter().enumerate() {
        validate(case).map_err(|err| format!("case {i}: {err}"))?;
    }
    let space = provider.space().clone();
    let retriever = SemanticMemoryRetriever::new(provider, 512).map_err(|e| e.to_string())?;
    let mut counts = BenchmarkCounts::default();
    for (i, case) in data.cases.into_iter().enumerate() {
        let records: Vec<_> = case
            .memories
            .into_iter()
            .map(|memory| MemoryRecord {
                id: MemoryId(memory.id),
                content: memory.content,
                priority: memory.priority.into(),
                kind: neuromita::memory::MemoryKind::Fact,
                state: neuromita::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            })
            .collect();
        let relevant: HashSet<_> = case.relevant_ids.into_iter().map(MemoryId).collect();
        let lexical = lexical_selected_ids(&records, &case.query);
        let started = Instant::now();
        let semantic = retriever
            .retrieve(case.character, &records, &case.query)
            .await
            .map_err(|error| format!("case {i}: {error}"))?;
        counts.semantic_duration_ms += started.elapsed().as_millis();
        counts.queries += 1;
        counts.lexical_hit_at_1 += usize::from(hit_at(&lexical, &relevant, 1));
        counts.lexical_hit_at_3 += usize::from(hit_at(&lexical, &relevant, 3));
        counts.semantic_hit_at_1 += usize::from(hit_at(&semantic.selected_ids, &relevant, 1));
        counts.semantic_hit_at_3 += usize::from(hit_at(&semantic.selected_ids, &relevant, 3));
        let RetrievalCost {
            embedding_requests,
            embedded_texts,
            embedded_characters,
            cache_hits,
        } = semantic.cost;
        counts.embedding_requests += embedding_requests;
        counts.embedded_texts += embedded_texts;
        counts.embedded_characters += embedded_characters;
        counts.cache_hits += cache_hits;
    }
    Ok(BenchmarkReport {
        backend: space.backend,
        model: space.model,
        protocol: space.protocol,
        dimensions: space.dimensions,
        live: true,
        corpus_cases: counts.queries,
        lexical_hit_rate_at_1: hit_rate(counts.lexical_hit_at_1, counts.queries),
        lexical_hit_rate_at_3: hit_rate(counts.lexical_hit_at_3, counts.queries),
        semantic_hit_rate_at_1: hit_rate(counts.semantic_hit_at_1, counts.queries),
        semantic_hit_rate_at_3: hit_rate(counts.semantic_hit_at_3, counts.queries),
        mean_semantic_ms: counts.semantic_duration_ms as f64 / counts.queries as f64,
        counts,
        billing_tokens: None,
        cost_usd: None,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 || args[2] != "--execute-live" {
        return Err("usage: semantic_benchmark <corpus.json> <gemini-embedding-001|gemini-embedding-2> --execute-live".into());
    }
    let protocol = match args[1].as_str() {
        "gemini-embedding-001" => GeminiEmbeddingProtocol::Embedding001,
        "gemini-embedding-2" => GeminiEmbeddingProtocol::Embedding2,
        _ => return Err("unknown embedding model".into()),
    };
    let bytes = fs::read(&args[0])?;
    let dataset: BenchmarkDataset = serde_json::from_slice(&bytes)?;
    let key = env::var("GEMINI_API_KEY")
        .map_err(|_| "GEMINI_API_KEY is required for explicit live benchmark")?;
    let dimensions = env::var("NEUROMITA_MEMORY_EMBEDDING_DIMENSIONS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(768);
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::new(GeminiEmbeddingProvider::google(protocol, &key, dimensions)?);
    let report = benchmark(dataset, provider).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_is_bounded_to_k_and_not_a_top_12_alias() {
        let ids = (1..=4)
            .map(|n| MemoryId(Uuid::from_u128(n)))
            .collect::<Vec<_>>();
        let relevant = HashSet::from([ids[3]]);
        assert!(!hit_at(&ids, &relevant, 1));
        assert!(!hit_at(&ids, &relevant, 3));
        assert!(hit_at(&ids, &relevant, 4));
    }

    #[tokio::test]
    async fn synthetic_fixture_evaluates_the_same_production_rankers() {
        use neuromita::knowledge::embedding::{
            Embedding, EmbeddingFuture, EmbeddingPurpose, EmbeddingSpace,
        };
        struct Fixture {
            space: EmbeddingSpace,
        }
        impl EmbeddingProvider for Fixture {
            fn space(&self) -> &EmbeddingSpace {
                &self.space
            }
            fn embed(
                &self,
                _purpose: EmbeddingPurpose,
                text: String,
            ) -> EmbeddingFuture<'_, Embedding> {
                Box::pin(async move {
                    let values = if text.contains("согреться") || text.contains("чай") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    };
                    Embedding::new(values, &self.space)
                })
            }
            fn document_request_count(&self, documents: usize) -> usize {
                usize::from(documents > 0)
            }
        }
        let data = serde_json::from_slice::<BenchmarkDataset>(include_bytes!(
            "../../tests/fixtures/semantic_benchmark_sample.json"
        ))
        .unwrap();
        let fixture: Arc<dyn EmbeddingProvider> = Arc::new(Fixture {
            space: EmbeddingSpace::new("fixture", "mock", "test", 2).unwrap(),
        });
        let report = benchmark(data, fixture).await.unwrap();
        assert_eq!(report.counts.queries, 1);
        assert_eq!(report.counts.lexical_hit_at_1, 0);
        assert_eq!(report.counts.semantic_hit_at_1, 1);
        assert_eq!(report.counts.embedding_requests, 2);
        assert_eq!(report.counts.embedded_texts, 3);
        assert!(report.billing_tokens.is_none() && report.cost_usd.is_none());
    }

    #[test]
    fn fixture_rejects_unknown_ground_truth() {
        let case = BenchmarkCase {
            query: "tea".into(),
            character: CharacterId::Crazy,
            memories: vec![BenchmarkMemory {
                id: Uuid::from_u128(1),
                content: "tea".into(),
                priority: FixturePriority::Normal,
            }],
            relevant_ids: vec![Uuid::from_u128(2)],
        };
        assert!(validate(&case).is_err());
    }
}
