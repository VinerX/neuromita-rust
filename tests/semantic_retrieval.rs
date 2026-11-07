use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use neuromita::{
    adapters::gemini_embedding::{GeminiEmbeddingProtocol, GeminiEmbeddingProvider},
    character::CharacterId,
    knowledge::{
        embedding::{
            Embedding, EmbeddingError, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose,
            EmbeddingSpace,
        },
        semantic_memory::SemanticMemoryRetriever,
    },
    timeline::domain::{MemoryId, MemoryPriority, MemoryRecord},
};
use uuid::Uuid;

fn memory(id: u128, priority: MemoryPriority, content: &str) -> MemoryRecord {
    MemoryRecord {
        id: MemoryId(Uuid::from_u128(id)),
        priority,
        content: content.into(),
        kind: neuromita::memory::MemoryKind::Fact,
        state: neuromita::memory::MemoryState::Active,
        protected: false,
        expires_at_unix_ms: None,
    }
}

struct SemanticFixture {
    space: EmbeddingSpace,
    document_batches: AtomicUsize,
    embedded_documents: AtomicUsize,
    query_count: AtomicUsize,
    fail: bool,
}

impl SemanticFixture {
    fn new(model: &str) -> Self {
        Self {
            space: EmbeddingSpace::new("fixture", model, "semantic-fixture-v1", 3).unwrap(),
            document_batches: AtomicUsize::new(0),
            embedded_documents: AtomicUsize::new(0),
            query_count: AtomicUsize::new(0),
            fail: false,
        }
    }
    fn vector(&self, text: &str) -> Embedding {
        let data =
            if text.contains("ромашковый") || text.contains("согреться") || text.contains("чай")
            {
                vec![1.0, 0.0, 0.0]
            } else if text.contains("картина") || text.contains("искусство") {
                vec![0.0, 1.0, 0.0]
            } else {
                vec![0.0, 0.0, 1.0]
            };
        Embedding::new(data, &self.space).unwrap()
    }
}

impl EmbeddingProvider for SemanticFixture {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }
    fn embed(&self, purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            if self.fail {
                return Err(EmbeddingError::Transport("fixture down".into()));
            }
            if purpose == EmbeddingPurpose::Query {
                self.query_count.fetch_add(1, Ordering::Relaxed);
            }
            Ok(self.vector(&text))
        })
    }
    fn document_request_count(&self, count: usize) -> usize {
        usize::from(count > 0)
    }
    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(async move {
            if self.fail {
                return Err(EmbeddingError::Transport("fixture down".into()));
            }
            self.document_batches.fetch_add(1, Ordering::Relaxed);
            self.embedded_documents
                .fetch_add(texts.len(), Ordering::Relaxed);
            Ok(texts.iter().map(|text| self.vector(text)).collect())
        })
    }
}

#[tokio::test]
async fn semantic_recalls_paraphrase_and_counts_calls_against_lexical_baseline() {
    let fixture = Arc::new(SemanticFixture::new("model-a"));
    let retrieval = SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap();
    let records = vec![
        memory(
            1,
            MemoryPriority::Critical,
            "У нас есть картина в мастерской",
        ),
        memory(2, MemoryPriority::Low, "Ромашковый чай хранится в кухне"),
    ];
    let lexical = records
        .iter()
        .map(|r| {
            let query = "хочу согреться";
            (
                r,
                query
                    .split_whitespace()
                    .filter(|word| r.content.to_lowercase().contains(word))
                    .count(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(lexical[0].1, 0);
    assert_eq!(lexical[1].1, 0);
    let result = retrieval
        .retrieve(CharacterId::Crazy, &records, "хочу согреться")
        .await
        .unwrap();
    assert_eq!(result.selected_ids[0], MemoryId(Uuid::from_u128(2)));
    assert_eq!(result.cost.embedding_requests, 2);
    assert_eq!(result.cost.embedded_texts, 3);
    assert_eq!(result.cost.cache_hits, 0);
    let repeated = retrieval
        .retrieve(CharacterId::Crazy, &records, "хочу согреться")
        .await
        .unwrap();
    assert_eq!(repeated.cost.embedding_requests, 1);
    assert_eq!(repeated.cost.embedded_texts, 1);
    assert_eq!(repeated.cost.cache_hits, 2);
    assert_eq!(fixture.document_batches.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.embedded_documents.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn exact_snapshot_membership_and_content_versions_cover_update_merge_delete_fork_reopen() {
    let fixture = Arc::new(SemanticFixture::new("model-a"));
    let retrieval = SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap();
    let main = vec![
        memory(1, MemoryPriority::Normal, "картина"),
        memory(2, MemoryPriority::High, "чай"),
    ];
    let first = retrieval
        .retrieve(CharacterId::Crazy, &main, "согреться")
        .await
        .unwrap();
    assert!(first.selected_ids.contains(&main[0].id) && first.selected_ids.contains(&main[1].id));
    let historical = main.clone();
    let fork = vec![memory(1, MemoryPriority::Normal, "ромашковый чай")];
    let changed = retrieval
        .retrieve(CharacterId::Crazy, &fork, "согреться")
        .await
        .unwrap();
    assert_eq!(changed.selected_ids, vec![main[0].id]);
    assert_eq!(changed.cost.cache_hits, 0);
    assert_eq!(changed.cost.embedded_texts, 2);
    let old = retrieval
        .retrieve(CharacterId::Crazy, &historical, "согреться")
        .await
        .unwrap();
    assert_eq!(old.cost.cache_hits, 2);
    assert_eq!(old.selected_ids[0], main[1].id);
    let merged = vec![memory(
        1,
        MemoryPriority::Critical,
        "картина и ромашковый чай",
    )];
    let res = retrieval
        .retrieve(CharacterId::Crazy, &merged, "согреться")
        .await
        .unwrap();
    assert_eq!(res.selected_ids, vec![main[0].id]);
    assert_eq!(res.cost.cache_hits, 0);
    let other = retrieval
        .retrieve(CharacterId::Kind, &merged, "согреться")
        .await
        .unwrap();
    assert_eq!(other.cost.cache_hits, 0);
    retrieval.clear_cache();
    let after_rebuild = retrieval
        .retrieve(CharacterId::Crazy, &main, "согреться")
        .await
        .unwrap();
    assert_eq!(after_rebuild.cost.cache_hits, 0);
    assert_eq!(after_rebuild.selected_ids[0], main[1].id);
    let empty = retrieval
        .retrieve(CharacterId::Crazy, &[], "согреться")
        .await
        .unwrap();
    assert!(empty.rendered.is_none() && empty.selected_ids.is_empty());
    assert_eq!(empty.cost.embedding_requests, 0);
}

#[tokio::test]
async fn provider_failure_does_not_silently_fallback() {
    let fixture = Arc::new(SemanticFixture {
        fail: true,
        ..SemanticFixture::new("model-a")
    });
    let retrieval = SemanticMemoryRetriever::new(fixture, 4).unwrap();
    let err = retrieval
        .retrieve(
            CharacterId::Crazy,
            &[memory(1, MemoryPriority::High, "чай")],
            "согреться",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EmbeddingError::Transport(_)));
}

#[tokio::test]
async fn gemini_http_fixture_verifies_query_and_document_protocol() {
    verify_gemini_http(GeminiEmbeddingProtocol::Embedding001).await;
    verify_gemini_http(GeminiEmbeddingProtocol::Embedding2).await;
}

async fn verify_gemini_http(protocol: GeminiEmbeddingProtocol) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for i in 0..2 {
            let (mut stream, _) = socket.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buffer[..n]);
                if let Some(pos) = data.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..pos]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.trim().parse().ok())
                        })
                        .unwrap();
                    if data.len() >= pos + 4 + length {
                        break;
                    }
                }
            }
            let pos = data.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
            let request = String::from_utf8_lossy(&data[..pos]);
            let body: serde_json::Value = serde_json::from_slice(&data[pos + 4..]).unwrap();
            assert!(request.contains("key=unit-secret"));
            let values = vec![1.0f32; 128];
            let response = if i == 0 {
                assert!(request.contains(":embedContent"));
                if protocol == GeminiEmbeddingProtocol::Embedding001 {
                    assert_eq!(body["taskType"], "RETRIEVAL_QUERY");
                } else {
                    assert!(body.get("taskType").is_none());
                    assert!(body["content"].to_string().contains("task: search result"));
                }
                serde_json::json!({"embedding":{"values":values}})
            } else {
                assert!(request.contains(":batchEmbedContents"));
                if protocol == GeminiEmbeddingProtocol::Embedding001 {
                    assert_eq!(body["requests"][0]["taskType"], "RETRIEVAL_DOCUMENT");
                } else {
                    assert!(body["requests"][0].get("taskType").is_none());
                    assert!(
                        body["requests"][0]["content"]
                            .to_string()
                            .contains("title: none")
                    );
                }
                serde_json::json!({"embeddings":[{"values":values}]})
            };
            let bytes = serde_json::to_vec(&response).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
        }
    });
    let provider = Arc::new(
        GeminiEmbeddingProvider::new(
            &format!("http://{address}/v1beta/"),
            protocol,
            "unit-secret",
            128,
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    let retrieval = SemanticMemoryRetriever::new(provider, 8).unwrap();
    let res = retrieval
        .retrieve(
            CharacterId::Crazy,
            &[memory(3, MemoryPriority::Normal, "ромашковый чай")],
            "согреться",
        )
        .await
        .unwrap();
    assert_eq!(res.selected_ids, vec![MemoryId(Uuid::from_u128(3))]);
    assert_eq!(res.cost.embedding_requests, 2);
    server.await.unwrap();
}

#[tokio::test]
async fn cancelled_document_embedding_does_not_populate_a_stale_cache() {
    use tokio::sync::Notify;
    struct Deferred {
        space: EmbeddingSpace,
        started: Notify,
        release: Notify,
        calls: AtomicUsize,
    }
    impl EmbeddingProvider for Deferred {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }
        fn embed(&self, purpose: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async move {
                if purpose == EmbeddingPurpose::Document {
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.started.notify_one();
                    self.release.notified().await;
                }
                Embedding::new(vec![1.0, 0.0, 0.0], &self.space)
            })
        }
    }
    let provider = Arc::new(Deferred {
        space: EmbeddingSpace::new("fixture", "cancel", "cancel-v1", 3).unwrap(),
        started: Notify::new(),
        release: Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let retrieval = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 8).unwrap());
    let memo = vec![memory(1, MemoryPriority::High, "чай")];
    let previous = retrieval.clone();
    let task =
        tokio::spawn(async move { previous.retrieve(CharacterId::Crazy, &memo, "чай").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.started.notified(),
    )
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    provider.release.notify_one();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        retrieval.retrieve(
            CharacterId::Crazy,
            &[memory(1, MemoryPriority::High, "чай")],
            "чай",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next.cost.cache_hits, 0);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn semantic_ranking_preserves_prompt_budget_and_order_independence() {
    let fixture = Arc::new(SemanticFixture::new("model-a"));
    let retrieval = SemanticMemoryRetriever::new(fixture, 512).unwrap();
    let mut memories = (1..=256)
        .map(|n| {
            memory(
                n,
                MemoryPriority::High,
                &format!("ромашковый чай {}", "чай".repeat(100)),
            )
        })
        .collect::<Vec<_>>();
    let first = retrieval
        .retrieve(CharacterId::Crazy, &memories, "хочу согреться")
        .await
        .unwrap();
    assert!(first.selected_ids.len() <= 12);
    assert!(first.rendered.as_ref().unwrap().chars().count() <= 6000);
    memories.reverse();
    let repeated = retrieval
        .retrieve(CharacterId::Crazy, &memories, "хочу согреться")
        .await
        .unwrap();
    assert_eq!(first.selected_ids, repeated.selected_ids);
    assert_eq!(first.rendered, repeated.rendered);
    assert_eq!(repeated.cost.cache_hits, 256);
}

#[tokio::test]
async fn provenance_and_explicit_filter_preserve_snapshot_ids_and_embedding_space() {
    use neuromita::knowledge::semantic_memory::{RetrievalStatus, SemanticRetrievalFilter};

    let fixture = Arc::new(SemanticFixture::new("model-a"));
    let retrieval = SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap();
    let memories = vec![
        memory(3, MemoryPriority::Critical, "картина"),
        memory(2, MemoryPriority::Low, "ромашковый чай"),
    ];
    let result = retrieval
        .retrieve_filtered(
            CharacterId::Crazy,
            &memories,
            "согреться",
            SemanticRetrievalFilter {
                minimum_cosine: Some(0.5),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, RetrievalStatus::Selected);
    assert_eq!(result.space, fixture.space);
    assert_eq!(result.selected_ids, vec![memories[1].id]);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].id, memories[1].id);
    assert!((result.matches[0].cosine_similarity - 1.0).abs() < 1e-6);
    assert_eq!(result.cost.embedding_requests, 2);
    assert!(result.rendered.unwrap().contains("ромашковый чай"));

    let no_matches = retrieval
        .retrieve_filtered(
            CharacterId::Crazy,
            &memories,
            "другая тема",
            SemanticRetrievalFilter {
                minimum_cosine: Some(0.5),
            },
        )
        .await
        .unwrap();
    assert_eq!(no_matches.status, RetrievalStatus::NoMatches);
    assert!(no_matches.selected_ids.is_empty());
    assert!(no_matches.matches.is_empty());
    assert!(no_matches.rendered.is_none());
    assert_eq!(no_matches.cost.cache_hits, 2);
    assert_eq!(no_matches.cost.embedding_requests, 1);

    let no_memories = retrieval
        .retrieve(CharacterId::Crazy, &[], "другая тема")
        .await
        .unwrap();
    assert_eq!(no_memories.status, RetrievalStatus::EmptyCorpus);
    assert!(no_memories.matches.is_empty());
    assert_eq!(no_memories.cost.embedding_requests, 0);
}

#[tokio::test]
async fn invalid_similarity_filter_is_rejected_before_embedding_requests() {
    use neuromita::knowledge::semantic_memory::SemanticRetrievalFilter;

    let fixture = Arc::new(SemanticFixture::new("model-a"));
    let retrieval = SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap();
    for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.01, 1.01] {
        let error = retrieval
            .retrieve_filtered(
                CharacterId::Crazy,
                &[memory(1, MemoryPriority::Normal, "чай")],
                "согреться",
                SemanticRetrievalFilter {
                    minimum_cosine: Some(score),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EmbeddingError::Configuration(_)));
    }
    assert_eq!(fixture.query_count.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.document_batches.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn semantic_results_remain_distinguishable_from_provider_failure() {
    use neuromita::knowledge::semantic_memory::{RetrievalStatus, SemanticRetrievalFilter};

    let fixture = Arc::new(SemanticFixture {
        fail: true,
        ..SemanticFixture::new("model-a")
    });
    let retrieval = SemanticMemoryRetriever::new(fixture, 16).unwrap();
    let memories = [memory(1, MemoryPriority::Normal, "чай")];
    let failure = retrieval
        .retrieve_filtered(
            CharacterId::Crazy,
            &memories,
            "согреться",
            SemanticRetrievalFilter {
                minimum_cosine: Some(0.5),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(failure, EmbeddingError::Transport(_)));

    let empty = retrieval
        .retrieve(CharacterId::Crazy, &[], "согреться")
        .await
        .unwrap();
    assert_eq!(empty.status, RetrievalStatus::EmptyCorpus);
}

#[tokio::test]
async fn explicit_fusion_reranks_synthetic_memory_without_changing_vector_default() {
    use neuromita::knowledge::{
        retrieval_fusion::{FusionMode, RetrievalMethod},
        semantic_memory::SemanticRetrievalFilter,
    };
    let fixture = Arc::new(SemanticFixture::new("fixture-fusion"));
    let retrieval = SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap();
    let records = vec![
        memory(1, MemoryPriority::Critical, "картина в мастерской"),
        memory(2, MemoryPriority::Low, "ромашковый чай на кухне"),
    ];
    let query = "картина согреться";
    let vector = retrieval
        .retrieve(CharacterId::Crazy, &records, query)
        .await
        .unwrap();
    assert_eq!(vector.selected_ids[0], records[1].id);
    let two_stage = retrieval
        .retrieve_with_fusion(
            CharacterId::Crazy,
            &records,
            query,
            SemanticRetrievalFilter::all(),
            FusionMode::TwoStage,
        )
        .await
        .unwrap();
    assert_eq!(two_stage.selected_ids[0], records[1].id);
    let rrf = retrieval
        .retrieve_with_fusion(
            CharacterId::Crazy,
            &records,
            query,
            SemanticRetrievalFilter::all(),
            FusionMode::Rrf { k: 60 },
        )
        .await
        .unwrap();
    assert_eq!(rrf.selected_ids[0], records[0].id);
    assert_eq!(
        rrf.matches[0].methods,
        vec![RetrievalMethod::Vector, RetrievalMethod::Keyword]
    );
    assert_eq!(rrf.matches[1].methods, vec![RetrievalMethod::Vector]);
    assert_eq!(rrf.cost.cache_hits, 2);
    assert_eq!(rrf.cost.embedded_texts, 1);
}

#[test]
fn generation_fusion_opt_in_rejects_invalid_mode_without_provider_calls() {
    use neuromita::knowledge::{KnowledgeRetrieval, retrieval_fusion::FusionMode};
    let fixture = Arc::new(SemanticFixture::new("fixture-fusion-validation"));
    let retrieval = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap());
    assert!(KnowledgeRetrieval::semantic_fusion(retrieval, FusionMode::Rrf { k: 0 }).is_err());
    assert_eq!(fixture.query_count.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.document_batches.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn forgotten_and_deleted_records_never_reach_embeddings_even_via_direct_retriever() {
    use neuromita::memory::MemoryState;
    let fixture = Arc::new(SemanticFixture::new("isolation-model"));
    let retriever = SemanticMemoryRetriever::new(fixture.clone(), 12).unwrap();
    let mut forgotten = memory(1901, MemoryPriority::High, "ромашковый чай");
    forgotten.state = MemoryState::Forgotten;
    let mut deleted = memory(1902, MemoryPriority::Normal, "картина в мастерской");
    deleted.state = MemoryState::Deleted;
    let result = retriever
        .retrieve(CharacterId::Crazy, &[forgotten.clone(), deleted], "чай")
        .await
        .unwrap();
    assert!(result.selected_ids.is_empty());
    assert_eq!(
        result.status,
        neuromita::knowledge::semantic_memory::RetrievalStatus::EmptyCorpus
    );
    assert_eq!(fixture.query_count.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.embedded_documents.load(Ordering::Relaxed), 0);
    let mut restored = forgotten;
    restored.state = MemoryState::Active;
    let result = retriever
        .retrieve(CharacterId::Crazy, &[restored], "чай")
        .await
        .unwrap();
    assert_eq!(result.selected_ids.len(), 1);
    assert_eq!(fixture.embedded_documents.load(Ordering::Relaxed), 1);
}
