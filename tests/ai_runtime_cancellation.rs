use std::{path::PathBuf, sync::Arc, time::Duration};

use neuromita::ai_runtime::{
    AiRuntimeError, AiService, AiSupervisor, ProcessTopology, PythonHostConfig,
};
use serde_json::json;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[tokio::test]
async fn canceling_inference_poisoned_worker_requires_explicit_restart() {
    let root = std::env::temp_dir().join(format!("nm-ai-cancel-{}", Uuid::new_v4()));
    let plugin = root.join("plugins/slow_tts");
    tokio::fs::create_dir_all(&plugin).await.unwrap();
    tokio::fs::write(
        plugin.join("plugin.json"),
        r#"{"id":"slow_tts","name":"Slow TTS","kind":"tts","entrypoint":"plugin.py","api_version":1}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        plugin.join("plugin.py"),
        r#"import time
from pathlib import Path
from neuromita_ai_sdk import InstallPlan
class Plugin:
    def describe(self): return {'kind': 'tts'}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {'installed': True}
    def settings_schema(self): return {}
    def initialize(self, settings): return {'model_ready': True}
    def infer(self, data):
        Path(data['marker']).write_text('running')
        time.sleep(30)
        return {'text': 'late'}
    def unload(self): pass
"#,
    )
    .await
    .unwrap();
    let supervisor = Arc::new(AiSupervisor::new(
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
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(35),
        },
    ));
    supervisor.start(AiService::Tts).await.unwrap();
    supervisor
        .initialize(AiService::Tts, "slow_tts", json!({}))
        .await
        .unwrap();
    let marker = root.join("request-started");
    let request = tokio::spawn({
        let supervisor = Arc::clone(&supervisor);
        let marker = marker.clone();
        async move {
            supervisor
                .call(
                    AiService::Tts,
                    "infer",
                    json!({"kind": "tts", "input": {"marker": marker}}),
                )
                .await
        }
    });
    timeout(Duration::from_secs(6), async {
        while !marker.is_file() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let error = timeout(
        Duration::from_secs(2),
        supervisor.call(AiService::Tts, "discover", json!({})),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        matches!(error, AiRuntimeError::Protocol(message) if message.contains("requires restart"))
    );
    supervisor.stop(AiService::Tts).await.unwrap();
    supervisor.start(AiService::Tts).await.unwrap();
    supervisor
        .initialize(AiService::Tts, "slow_tts", json!({}))
        .await
        .unwrap();
    assert!(supervisor.discover(AiService::Tts).await.is_ok());
    supervisor.stop(AiService::Tts).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
