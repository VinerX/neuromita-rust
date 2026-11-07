use std::{path::PathBuf, sync::Arc, time::Duration};

use neuromita::{
    ai_runtime::{AiService, AiSupervisor, ProcessTopology, PythonHostConfig, PythonVoiceProvider},
    settings::Volume,
    voice::{VoiceOptions, VoiceProvider},
};
use serde_json::json;
use uuid::Uuid;

fn python() -> PathBuf {
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

#[tokio::test]
async fn large_tts_wav_crosses_multiple_correlated_ipc_chunks() {
    let root = std::env::temp_dir().join(format!("neuromita-stream-{}", Uuid::new_v4()));
    let plugin = root.join("plugins/large_audio");
    tokio::fs::create_dir_all(&plugin).await.unwrap();
    tokio::fs::write(plugin.join("plugin.json"), r#"{"id":"large_audio","name":"Large Audio","kind":"tts","entrypoint":"plugin.py","api_version":1}"#).await.unwrap();
    tokio::fs::write(plugin.join("plugin.py"), r#"
from neuromita_ai_sdk import InstallPlan
import base64, io, wave
class Plugin:
    def describe(self): return {'kind': 'tts'}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {'installed': True}
    def settings_schema(self): return {}
    def initialize(self, settings): return {'model_ready': True}
    def infer(self, data):
        output = io.BytesIO()
        with wave.open(output, 'wb') as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes(bytes(120_000))
        return {'format': 'wav', 'audio_base64': base64.b64encode(output.getvalue()).decode('ascii')}
    def unload(self): pass
"#).await.unwrap();
    let supervisor = Arc::new(AiSupervisor::new(
        ProcessTopology::Split,
        PythonHostConfig {
            python_executable: python(),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(10),
        },
    ));
    supervisor.start(AiService::Tts).await.unwrap();
    supervisor
        .initialize(AiService::Tts, "large_audio", json!({}))
        .await
        .unwrap();
    let result = PythonVoiceProvider::new(Arc::clone(&supervisor))
        .synthesize(
            Arc::from("stream test"),
            VoiceOptions {
                voice: "default".into(),
                volume: Volume::new(80).unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.bytes.len(), 120_044);
    assert_eq!(&result.bytes[..4], b"RIFF");
    supervisor.stop(AiService::Tts).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
