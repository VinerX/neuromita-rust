use std::{collections::BTreeMap, sync::Arc};

use neuromita::{
    character::CharacterId,
    knowledge::{
        embedding::EmbeddingError,
        reranking::{RerankFuture, RerankPolicy, RerankProvider, rerank_prefix},
        retrieval_fusion::{FusedHit, RetrievalKey, RetrievalSource},
    },
};

struct FixedScores(Result<Vec<f64>, EmbeddingError>);

impl RerankProvider for FixedScores {
    fn score<'a>(&'a self, _query: &'a str, _candidates: &'a [String]) -> RerankFuture<'a> {
        Box::pin(async { self.0.clone() })
    }
}

fn hit(id: &str, score: f64) -> FusedHit {
    FusedHit {
        key: RetrievalKey {
            character: CharacterId::Crazy,
            source: RetrievalSource::Memory,
            id: id.to_string(),
        },
        content: format!("Memory {id}"),
        score,
        evidence: vec![],
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn reranker_reorders_only_the_selected_prefix_without_losing_evidence() {
    let provider = Arc::new(FixedScores(Ok(vec![0.1, 0.9])));
    let mut hits = vec![hit("first", 0.9), hit("second", 0.8), hit("tail", 0.7)];
    rerank_prefix(
        provider.as_ref(),
        "query",
        &mut hits,
        RerankPolicy {
            alpha: 1.0,
            limit: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.key.id.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "first", "tail"]
    );
    assert_eq!(hits[0].key.character, CharacterId::Crazy);
    assert_eq!(hits[2].score, 0.7);
}

#[tokio::test]
async fn reranking_is_fail_closed_for_provider_and_score_errors() {
    let hits = vec![hit("first", 0.9), hit("second", 0.8)];
    for response in [
        Err(EmbeddingError::Transport("unavailable".into())),
        Ok(vec![0.9]),
        Ok(vec![0.9, f64::NAN]),
        Ok(vec![0.9, 1.1]),
    ] {
        let mut attempt = hits.clone();
        assert!(
            rerank_prefix(
                &FixedScores(response),
                "query",
                &mut attempt,
                RerankPolicy {
                    alpha: 1.0,
                    limit: 2
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            attempt
                .iter()
                .map(|hit| hit.key.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }
    for policy in [
        RerankPolicy {
            alpha: -0.1,
            limit: 1,
        },
        RerankPolicy {
            alpha: f64::NAN,
            limit: 1,
        },
        RerankPolicy {
            alpha: 0.5,
            limit: 0,
        },
        RerankPolicy {
            alpha: 0.5,
            limit: 33,
        },
    ] {
        assert!(policy.validate().is_err());
    }
}

struct EmbeddingFixture {
    space: neuromita::knowledge::embedding::EmbeddingSpace,
}

impl neuromita::knowledge::embedding::EmbeddingProvider for EmbeddingFixture {
    fn space(&self) -> &neuromita::knowledge::embedding::EmbeddingSpace {
        &self.space
    }

    fn embed(
        &self,
        purpose: neuromita::knowledge::embedding::EmbeddingPurpose,
        text: String,
    ) -> neuromita::knowledge::embedding::EmbeddingFuture<
        '_,
        neuromita::knowledge::embedding::Embedding,
    > {
        Box::pin(async move {
            let values = if purpose == neuromita::knowledge::embedding::EmbeddingPurpose::Query
                || text.contains("tea")
            {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            };
            neuromita::knowledge::embedding::Embedding::new(values, &self.space)
        })
    }
}

#[tokio::test]
async fn reranker_reorders_authoritative_memory_selection_after_vector_fusion() {
    use neuromita::{
        knowledge::{embedding::EmbeddingSpace, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryKind, MemoryState},
        timeline::domain::{MemoryId, MemoryPriority, MemoryRecord},
    };
    use uuid::Uuid;

    let embedding = Arc::new(EmbeddingFixture {
        space: EmbeddingSpace::new("fixture", "embedding", "v1", 2).unwrap(),
    });
    let retriever = SemanticMemoryRetriever::new(embedding, 8)
        .unwrap()
        .with_reranker(
            Arc::new(FixedScores(Ok(vec![0.1, 0.9]))),
            RerankPolicy {
                alpha: 1.0,
                limit: 2,
            },
        )
        .unwrap();
    let records = [(1, "tea"), (2, "art")]
        .into_iter()
        .map(|(id, content)| MemoryRecord {
            id: MemoryId(Uuid::from_u128(id)),
            priority: MemoryPriority::Normal,
            content: content.into(),
            kind: MemoryKind::Fact,
            state: MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        })
        .collect::<Vec<_>>();
    let selected = retriever
        .retrieve(CharacterId::Crazy, &records, "query")
        .await
        .unwrap();
    assert_eq!(selected.selected_ids, vec![records[1].id, records[0].id]);
    assert_eq!(selected.matches[0].cosine_similarity, 0.0);
    assert_eq!(selected.matches[0].fusion_score, 0.9);
    assert_eq!(selected.matches[1].fusion_score, 0.1);
    let failed = SemanticMemoryRetriever::new(
        Arc::new(EmbeddingFixture {
            space: EmbeddingSpace::new("fixture", "embedding", "v1", 2).unwrap(),
        }),
        8,
    )
    .unwrap()
    .with_reranker(
        Arc::new(FixedScores(Err(EmbeddingError::Transport(
            "offline".into(),
        )))),
        RerankPolicy {
            alpha: 0.8,
            limit: 2,
        },
    )
    .unwrap();
    assert!(matches!(
        failed.retrieve(CharacterId::Crazy, &records, "query").await,
        Err(EmbeddingError::Transport(_))
    ));
}
