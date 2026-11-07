use std::{path::PathBuf, sync::Arc, time::Duration};

use neuromita::{
    adapters::python_embedding::PythonEmbeddingProvider,
    ai_runtime::{
        AiService, AiSupervisor, ProcessTopology, PythonHostConfig, speech::PythonSpeechRecognizer,
    },
    knowledge::embedding::{EmbeddingError, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace},
};
use serde_json::json;

fn supervisor() -> Arc<AiSupervisor> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Arc::new(AiSupervisor::new(
        ProcessTopology::Split,
        PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    if cfg!(windows) {
                        "python".into()
                    } else {
                        "python3".into()
                    }
                }),
            sdk_root: root.join("python"),
            plugins_directory: root.join("python/plugins/builtin"),
            models_directory: None,
            request_timeout: Duration::from_secs(8),
        },
    ))
}

fn wav() -> Vec<u8> {
    let data = vec![0u8; 320];
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&32000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

#[tokio::test]
async fn typed_asr_client_accepts_mock_segment_and_rejects_bad_audio() {
    let sup = supervisor();
    sup.start(AiService::Asr).await.unwrap();
    sup.initialize(AiService::Asr, "mock_asr", json!({"language":"ru"}))
        .await
        .unwrap();
    let recognizer = PythonSpeechRecognizer::new(Arc::clone(&sup));
    let segment = recognizer.transcribe_wav(&wav()).await.unwrap();
    assert_eq!(segment.text, "");
    assert!(recognizer.transcribe_wav(b"invalid").await.is_err());
    let longer_than_old_ipc_bound = neuromita::audio::segment::SpeechSegment {
        samples: vec![0; 60 * 16_000],
    }
    .as_wav()
    .unwrap();
    assert!(
        recognizer
            .transcribe_wav(&longer_than_old_ipc_bound)
            .await
            .is_ok()
    );
    sup.stop(AiService::Asr).await.unwrap();
}

#[tokio::test]
async fn python_embedding_provider_uses_selected_model_space_and_batch_protocol() {
    let sup = supervisor();
    sup.start(AiService::Embedding).await.unwrap();
    sup.initialize(AiService::Embedding, "mock_embedding", json!({}))
        .await
        .unwrap();
    let space = EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap();
    let provider = PythonEmbeddingProvider::new(Arc::clone(&sup), space.clone());
    let tea = provider
        .embed(EmbeddingPurpose::Query, "чай".into())
        .await
        .unwrap();
    let docs = provider
        .embed_documents(vec!["чай ромашковый".into(), "картина".into()])
        .await
        .unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(provider.document_request_count(65), 3);
    assert!(matches!(
        provider.embed(EmbeddingPurpose::Query, "".into()).await,
        Err(EmbeddingError::Configuration(_))
    ));
    let mismatched = PythonEmbeddingProvider::new(
        Arc::clone(&sup),
        EmbeddingSpace::new("python", "different", "fixture-v1", 3).unwrap(),
    );
    assert!(matches!(
        mismatched
            .embed(EmbeddingPurpose::Query, "чай".into())
            .await,
        Err(EmbeddingError::Response(_))
    ));
    let _ = tea;
    sup.stop(AiService::Embedding).await.unwrap();
}

#[tokio::test]
async fn typed_python_reranker_retains_source_order_and_checks_space() {
    use neuromita::adapters::python_reranker::{PythonReranker, RerankError};
    let sup = supervisor();
    sup.start(AiService::Reranker).await.unwrap();
    sup.initialize(AiService::Reranker, "mock_reranker", json!({}))
        .await
        .unwrap();
    let provider = PythonReranker::new(
        Arc::clone(&sup),
        "rerank-fixture".into(),
        "fixture-rerank-v1".into(),
    )
    .unwrap();
    let docs = vec!["картина".into(), "ромашковый чай".into()];
    assert_eq!(provider.score("чай", &docs).await.unwrap(), vec![0.1, 1.0]);
    assert!(provider.score("", &docs).await.is_err());
    let invalid = PythonReranker::new(
        Arc::clone(&sup),
        "incorrect".into(),
        "fixture-rerank-v1".into(),
    )
    .unwrap();
    assert!(matches!(
        invalid.score("чай", &docs).await,
        Err(RerankError::Response(_))
    ));
    sup.stop(AiService::Reranker).await.unwrap();
}
