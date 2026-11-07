use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use neuromita::{
    ai_runtime::{
        AiService, ModelActivation, ProcessTopology, PythonHostConfig,
        application::{AiApplicationConfig, AiWorkerSelection},
        backend::BackendInstaller,
        composition::{RuntimeComposition, ServiceSelection},
        overlay::{LockedOverlay, OverlayInstaller},
    },
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    settings::Volume,
    voice::VoiceOptions,
};
use serde_json::json;
use uuid::Uuid;

static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn runtime(root: &Path, plugin_id: &str) -> AiApplicationConfig {
    runtime_with_python(root, plugin_id, &test_python_version().await).await
}

async fn test_python_version() -> String {
    let output = tokio::process::Command::new(test_python())
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let reported = String::from_utf8(output.stdout).unwrap();
    let version = reported
        .trim()
        .strip_prefix("Python ")
        .expect("Python version probe");
    assert!(
        version.starts_with("3.12."),
        "Application fixtures require Python 3.12; set NEUROMITA_TEST_PYTHON to that interpreter"
    );
    version.to_owned()
}

fn test_python() -> PathBuf {
    std::env::var_os("NEUROMITA_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        })
}

async fn runtime_with_python(
    root: &Path,
    plugin_id: &str,
    python_version: &str,
) -> AiApplicationConfig {
    let overlay = LockedOverlay {
        plugin_id: "mock_tts".into(),
        revision: "test".into(),
        python_version: python_version.into(),
        packages: vec![],
        actions: vec![],
    };
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let overlay_directory = overlays.installed_path(&overlay).unwrap();
    tokio::fs::create_dir_all(&overlay_directory).await.unwrap();
    tokio::fs::write(
        overlay_directory.join(".neuromita-overlay.json"),
        serde_json::to_vec(&overlay).unwrap(),
    )
    .await
    .unwrap();
    let composition = RuntimeComposition::resolve(
        ProcessTopology::Split,
        AiService::Tts,
        &[ServiceSelection {
            service: AiService::Tts,
            backends: vec![],
            overlays: vec![overlay],
        }],
        &backends,
        &overlays,
    )
    .await
    .unwrap();
    AiApplicationConfig {
        topology: ProcessTopology::Split,
        host: PythonHostConfig {
            python_executable: test_python(),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("python/plugins/builtin"),
            models_directory: None,
            request_timeout: Duration::from_secs(10),
        },
        workers: vec![AiWorkerSelection {
            service: AiService::Tts,
            composition,
            models: vec![ModelActivation {
                service: AiService::Tts,
                plugin_id: plugin_id.into(),
                settings: json!({"speaker":"bright"}),
                asset_revision: None,
            }],
        }],
        voice_profiles: None,
    }
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(2).unwrap(),
        discovery_buffer: NonZeroUsize::new(8).unwrap(),
        character_commands: NonZeroUsize::new(8).unwrap(),
        presenter_updates: NonZeroUsize::new(8).unwrap(),
        retained_finished: NonZeroUsize::new(8).unwrap(),
    }
}

fn application(ai_runtime: AiApplicationConfig) -> ApplicationConfig {
    let generator: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let template = GeminiTemplate::google(
        "fixture",
        "dummy-key",
        GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let mut config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([generator]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.ai_runtime = Some(ai_runtime);
    config
}

#[tokio::test]
async fn application_owns_initialized_tts_and_releases_it_on_shutdown() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-application-ai-{}", Uuid::new_v4()));
    let running = runtime(&root, "mock_tts").await.activate().await.unwrap();
    let audio = running
        .voice_provider()
        .synthesize(
            Arc::from("Test"),
            VoiceOptions {
                voice: "default".into(),
                volume: Volume::new(50).unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(audio.bytes.starts_with(b"RIFF"));
    running.shutdown().await.unwrap();

    let app = ApplicationLifecycle::start(application(runtime(&root, "mock_tts").await))
        .await
        .unwrap();
    app.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn invalid_ai_selection_aborts_application_without_voice_fallback() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-application-ai-fail-{}", Uuid::new_v4()));
    let failure =
        ApplicationLifecycle::start(application(runtime(&root, "missing_tts").await)).await;
    assert!(failure.is_err());
    let valid = ApplicationLifecycle::start(application(runtime(&root, "mock_tts").await))
        .await
        .unwrap();
    valid.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn mismatched_locked_python_blocks_application_startup() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-application-ai-abi-{}", Uuid::new_v4()));
    let incompatible = runtime_with_python(&root, "mock_tts", "99.0.0").await;
    let outcome = ApplicationLifecycle::start(application(incompatible)).await;
    assert!(matches!(
        outcome,
        Err(neuromita::app::ApplicationError::Ai(
            neuromita::ai_runtime::application::AiApplicationError::Interpreter(_)
        ))
    ));
    let valid = ApplicationLifecycle::start(application(runtime(&root, "mock_tts").await))
        .await
        .unwrap();
    valid.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn invalid_timeline_stops_startup_before_ai_activation() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-application-ai-timeline-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&root).await.unwrap();
    let mut config = application(runtime_with_python(&root, "mock_tts", "3.12.10").await);
    config.timeline = neuromita::timeline::TimelineConfig::Persistent {
        database: root.clone(),
    };
    let outcome = ApplicationLifecycle::start(config).await;
    assert!(matches!(
        outcome,
        Err(neuromita::app::ApplicationError::Timeline(_))
    ));
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn occupied_port_stops_startup_before_ai_activation() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-application-ai-server-{}", Uuid::new_v4()));
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = application(runtime_with_python(&root, "mock_tts", "3.12.10").await);
    config.server = neuromita::server::ServerConfig::local_control(socket.local_addr().unwrap());
    let outcome = ApplicationLifecycle::start(config).await;
    assert!(matches!(
        outcome,
        Err(neuromita::app::ApplicationError::Server(_))
    ));
    drop(socket);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

async fn fixture_worker(root: &Path, service: AiService, plugin_id: &str) -> AiWorkerSelection {
    let overlay = LockedOverlay {
        plugin_id: plugin_id.into(),
        revision: "test".into(),
        python_version: test_python_version().await,
        packages: vec![],
        actions: vec![],
    };
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let destination = overlays.installed_path(&overlay).unwrap();
    tokio::fs::create_dir_all(&destination).await.unwrap();
    tokio::fs::write(
        destination.join(".neuromita-overlay.json"),
        serde_json::to_vec(&overlay).unwrap(),
    )
    .await
    .unwrap();
    let composition = RuntimeComposition::resolve(
        ProcessTopology::Split,
        service,
        &[ServiceSelection {
            service,
            backends: vec![],
            overlays: vec![overlay],
        }],
        &backends,
        &overlays,
    )
    .await
    .unwrap();
    AiWorkerSelection {
        service,
        composition,
        models: vec![ModelActivation {
            service,
            plugin_id: plugin_id.into(),
            settings: json!({}),
            asset_revision: None,
        }],
    }
}

#[tokio::test]
async fn asr_only_application_does_not_require_a_tts_worker() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-app-asr-only-{}", Uuid::new_v4()));
    let mut ai = runtime(&root, "mock_tts").await;
    ai.workers = vec![fixture_worker(&root, AiService::Asr, "mock_asr").await];
    let running = ai.activate().await.unwrap();
    assert!(running.speech_recognizer().is_some());
    assert!(!running.has_service(AiService::Tts));
    running.shutdown().await.unwrap();
    let mut ai = runtime(&root, "mock_tts").await;
    ai.workers = vec![fixture_worker(&root, AiService::Asr, "mock_asr").await];
    let app = ApplicationLifecycle::start(application(ai)).await.unwrap();
    app.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn application_binds_explicit_local_embedding_retriever_without_fallback() {
    use neuromita::{
        app::{AiKnowledgeSelection, ApplicationError},
        knowledge::{embedding::EmbeddingSpace, retrieval_fusion::FusionMode},
    };
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-app-rag-{}", Uuid::new_v4()));
    let mut ai = runtime(&root, "mock_tts").await;
    let embedding = fixture_worker(&root, AiService::Embedding, "mock_embedding").await;
    ai.workers = vec![embedding];
    let mut config = application(ai);
    config.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 32,
        fusion: FusionMode::VectorOnly,
        reranker: None,
    });
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.shutdown().await.unwrap();
    let mut invalid = application(runtime(&root, "mock_tts").await);
    invalid.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 32,
        fusion: FusionMode::VectorOnly,
        reranker: None,
    });
    let result = ApplicationLifecycle::start(invalid).await;
    assert!(matches!(
        result,
        Err(ApplicationError::MissingEmbeddingWorker)
    ));
    let next = ApplicationLifecycle::start(application(runtime(&root, "mock_tts").await))
        .await
        .unwrap();
    next.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn application_requires_an_explicit_reranker_worker_for_reranked_knowledge() {
    use neuromita::{
        app::{AiKnowledgeSelection, AiRerankSelection, ApplicationError},
        knowledge::{
            embedding::EmbeddingSpace, reranking::RerankPolicy, retrieval_fusion::FusionMode,
        },
    };
    let _guard = CORE_TEST_MUTEX.lock().await;
    let root = std::env::temp_dir().join(format!("nm-app-reranker-{}", Uuid::new_v4()));
    let mut ai = runtime(&root, "mock_tts").await;
    ai.workers = vec![fixture_worker(&root, AiService::Embedding, "mock_embedding").await];
    let mut config = application(ai);
    config.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 32,
        fusion: FusionMode::VectorOnly,
        reranker: Some(AiRerankSelection {
            model_id: "rerank-fixture".into(),
            protocol: "fixture-rerank-v1".into(),
            policy: RerankPolicy {
                alpha: 0.75,
                limit: 8,
            },
        }),
    });
    let result = ApplicationLifecycle::start(config).await;
    assert!(matches!(
        result,
        Err(ApplicationError::MissingRerankerWorker)
    ));
    let mut ai = runtime(&root, "mock_tts").await;
    ai.workers = vec![
        fixture_worker(&root, AiService::Embedding, "mock_embedding").await,
        fixture_worker(&root, AiService::Reranker, "mock_reranker").await,
    ];
    let mut config = application(ai);
    config.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 32,
        fusion: FusionMode::VectorOnly,
        reranker: Some(AiRerankSelection {
            model_id: "rerank-fixture".into(),
            protocol: "fixture-rerank-v1".into(),
            policy: RerankPolicy {
                alpha: 0.75,
                limit: 8,
            },
        }),
    });
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
