use std::{path::PathBuf, time::Duration};

use neuromita::ai_runtime::{
    AiService, AiSupervisor, ProcessTopology, PythonHostConfig,
    backend::BackendInstaller,
    composition::{RuntimeComposition, ServiceSelection},
    overlay::{LockedOverlay, OverlayInstaller},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn python_host_imports_active_model_overlay() {
    let root = std::env::temp_dir().join(format!("nm-worker-overlay-{}", Uuid::new_v4()));
    let plugins = root.join("plugins").join("fixture");
    tokio::fs::create_dir_all(&plugins).await.unwrap();
    tokio::fs::write(
        plugins.join("plugin.json"),
        r#"{"id":"fixture","name":"Fixture","kind":"tts","entrypoint":"plugin.py","api_version":1}"#,
    ).await.unwrap();
    tokio::fs::write(
        plugins.join("plugin.py"),
        r#"from neuromita_ai_sdk import InstallPlan
class Plugin:
    def describe(self): return {"kind": "tts"}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {"installed": True}
    def settings_schema(self): return {}
    def initialize(self, settings):
        import fixture_overlay_dependency
        return {"model_ready": True, "value": fixture_overlay_dependency.VALUE}
    def infer(self, input_data): return {"text": "ready"}
    def unload(self): pass
"#,
    )
    .await
    .unwrap();
    let overlay = LockedOverlay {
        plugin_id: "fixture".into(),
        revision: "rev1".into(),
        python_version: "3.13.5".into(),
        packages: vec![],
        actions: vec![],
    };
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(2),
    };
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(2),
    };
    let overlay_dir = overlays.installed_path(&overlay).unwrap();
    tokio::fs::create_dir_all(&overlay_dir).await.unwrap();
    tokio::fs::write(
        overlay_dir.join("fixture_overlay_dependency.py"),
        "VALUE = 'overlay-loaded'\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        overlay_dir.join(".neuromita-overlay.json"),
        serde_json::to_vec(&overlay).unwrap(),
    )
    .await
    .unwrap();
    let selected = ServiceSelection {
        service: AiService::Tts,
        backends: vec![],
        overlays: vec![overlay],
    };
    let composition = RuntimeComposition::resolve(
        ProcessTopology::Split,
        AiService::Tts,
        &[selected],
        &backends,
        &overlays,
    )
    .await
    .unwrap();
    let python = std::env::var("NEUROMITA_TEST_PYTHON")
        .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into());
    let supervisor = AiSupervisor::new(
        ProcessTopology::Split,
        PythonHostConfig {
            python_executable: PathBuf::from(python),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(10),
        },
    );
    supervisor
        .start_composed(AiService::Tts, &composition)
        .await
        .unwrap();
    let result = supervisor
        .initialize(AiService::Tts, "fixture", json!({}))
        .await
        .unwrap();
    assert_eq!(result["value"], "overlay-loaded");
    supervisor.stop(AiService::Tts).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
