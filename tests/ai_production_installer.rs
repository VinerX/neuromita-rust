#![cfg(unix)]

use std::{collections::HashSet, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use neuromita::ai_runtime::{
    AiService, ProcessTopology,
    backend::{BackendKind, LockedBackend},
    managed::{ManagedRuntimeBootstrap, ManagedRuntimeError},
    production::{AiProductionError, AiProductionInstaller, ModelAssets, SelectedModel},
    uv::{UvArchiveKind, UvDistribution, UvInstaller},
};
use serde_json::json;
use tokio::process::Command;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    runtime: AiProductionInstaller,
}

fn model(service: AiService, plugin_id: &str) -> SelectedModel {
    SelectedModel {
        service,
        plugin_id: plugin_id.into(),
        package_revision: "rev1".into(),
        backend: LockedBackend {
            kind: BackendKind::RemoteApi,
            revision: "api-v1".into(),
            python_version: "3.12.10".into(),
            packages: Vec::new(),
        },
        settings: json!({}),
        extra_backends: Vec::new(),
        assets: ModelAssets::None,
        approved_actions: HashSet::new(),
    }
}

impl Fixture {
    async fn new(topology: ProcessTopology) -> Self {
        let root = std::env::temp_dir().join(format!("nm-production-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let python_command =
            std::env::var("NEUROMITA_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        let actual_python = String::from_utf8(
            Command::new(python_command)
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
                .await
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let uv = root.join("uv");
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 'uv 0.13.4'; exit 0; fi
if [ "$1" = "python" ] && [ "$2" = "install" ]; then
    mkdir -p "$UV_PYTHON_INSTALL_DIR/cpython-test/bin"
    printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo Python 3.12.10; exit 0; fi\nexec {actual_python} "$@"\n' > "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    chmod +x "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
if [ "$1" = "python" ] && [ "$2" = "find" ]; then
    [ -f "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3" ] || exit 2
    printf '%s\n' "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
exit 5
"#
        );
        tokio::fs::write(&uv, script).await.unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let archive = root.join("uv.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&root)
                .arg("uv")
                .output()
                .await
                .unwrap()
                .status
                .success()
        );
        let bytes = tokio::fs::read(&archive).await.unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let digest: String = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let python_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let runtime = AiProductionInstaller {
            bootstrap: ManagedRuntimeBootstrap {
                uv: UvInstaller {
                    data_directory: root.join("AIData"),
                    distribution: UvDistribution {
                        version: "0.13.4".into(),
                        archive_url: format!("file://{}", archive.display()),
                        archive_sha256: digest,
                        archive_kind: UvArchiveKind::TarGz,
                    },
                    timeout: Duration::from_secs(8),
                },
                python_version: "3.12.10".into(),
                python_timeout: Duration::from_secs(8),
            },
            sdk_root: python_root.clone(),
            plugins_directory: python_root.join("plugins/builtin"),
            topology,
            timeout: Duration::from_secs(12),
            max_asset_bytes: 1024 * 1024,
            voice_profiles: None,
        };
        Self { root, runtime }
    }

    async fn close(self) {
        tokio::fs::remove_dir_all(self.root).await.unwrap();
    }

    async fn add_plugin(&self, name: &str, plan: &str) {
        let dir = self.root.join("plugins").join(name);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let manifest =
            json!({"id":name,"name":name,"kind":"tts","entrypoint":"plugin.py","api_version":1});
        tokio::fs::write(dir.join("plugin.json"), manifest.to_string())
            .await
            .unwrap();
        let content = format!(
            r#"from neuromita_ai_sdk import InstallPlan, AssetRequirement
from dataclasses import replace
class Plugin:
    def describe(self): return {{"kind": "tts"}}
    def install_plan(self): return replace({plan}, supported_backends=("remote-api",))
    def probe_install(self): return {{"installed": True}}
    def settings_schema(self): return {{}}
    def initialize(self, settings): return {{"model_ready": True}}
    def infer(self, input_data): return {{"text": "ready"}}
    def unload(self): pass
"#
        );
        tokio::fs::write(dir.join("plugin.py"), content)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn production_install_prepares_and_activates_shared_selected_models() {
    let fixture = Fixture::new(ProcessTopology::Shared).await;
    let choices = [
        model(AiService::Tts, "mock_tts"),
        model(AiService::Asr, "mock_asr"),
    ];
    let prepared = fixture.runtime.prepare(&choices).await.unwrap();
    assert_eq!(prepared.workers.len(), 1);
    assert_eq!(prepared.workers[0].models.len(), 2);
    assert_eq!(prepared.workers[0].composition.python_version(), "3.12.10");
    assert_eq!(prepared.workers[0].composition.paths().len(), 3);
    let running = prepared.activate().await.unwrap();
    assert!(running.has_service(AiService::Tts));
    assert!(running.has_service(AiService::Asr));
    running.shutdown().await.unwrap();
    let again = fixture.runtime.prepare(&choices).await.unwrap();
    assert_eq!(again.workers.len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn production_split_activates_independent_workers() {
    let fixture = Fixture::new(ProcessTopology::Split).await;
    let choices = [
        model(AiService::Tts, "mock_tts"),
        model(AiService::Asr, "mock_asr"),
    ];
    let running = fixture
        .runtime
        .prepare_and_activate(&choices)
        .await
        .unwrap();
    assert!(running.has_service(AiService::Tts));
    assert!(running.has_service(AiService::Asr));
    running.shutdown().await.unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn incompatible_backend_is_rejected_before_backend_layer_installation() {
    use neuromita::ai_runtime::overlay::LockedPackage;
    let fixture = Fixture::new(ProcessTopology::Split).await;
    let mut selected = model(AiService::Asr, "mock_asr");
    selected.backend.kind = BackendKind::TorchCpu;
    selected.backend.packages = vec![LockedPackage {
        name: "torch".into(),
        version: "2.8.0".into(),
    }];
    let error = fixture.runtime.prepare(&[selected]).await.err().unwrap();
    assert!(matches!(error, AiProductionError::Invalid(_)), "{error}");
    assert!(!fixture.root.join("AIData/backends/torch-cpu").exists());
    fixture.close().await;
}

#[tokio::test]
async fn missing_assets_fail_closed_without_model_activation() {
    let mut fixture = Fixture::new(ProcessTopology::Split).await;
    fixture
        .add_plugin(
            "weight_required",
            "InstallPlan(assets=(AssetRequirement('weight', 'weights.bin'),))",
        )
        .await;
    fixture.runtime.plugins_directory = fixture.root.join("plugins");
    let error = fixture
        .runtime
        .prepare(&[model(AiService::Tts, "weight_required")])
        .await
        .err()
        .unwrap();
    assert!(matches!(error, AiProductionError::Unready(_)), "{error}");
    fixture.close().await;
}

#[tokio::test]
async fn unsupported_python_and_duplicate_services_fail_before_bootstrap() {
    let mut fixture = Fixture::new(ProcessTopology::Split).await;
    fixture.runtime.bootstrap.python_version = "3.13.5".into();
    let mut wrong = model(AiService::Tts, "mock_tts");
    wrong.backend.python_version = "3.13.5".into();
    let choices = [wrong];
    let error = fixture.runtime.prepare(&choices).await.err().unwrap();
    assert!(matches!(
        error,
        AiProductionError::Bootstrap(ManagedRuntimeError::UnsupportedPython(_))
    ));
    assert!(!fixture.root.join("AIData").exists());
    fixture.runtime.bootstrap.python_version = "3.12.10".into();
    let duplicate = [
        model(AiService::Tts, "mock_tts"),
        model(AiService::Tts, "mock_asr"),
    ];
    assert!(matches!(
        fixture.runtime.prepare(&duplicate).await.err(),
        Some(AiProductionError::Invalid(_))
    ));
    assert!(!fixture.root.join("AIData").exists());
    fixture.close().await;
}

#[tokio::test]
async fn asset_receipt_requires_explicit_revision_and_checksum() {
    use neuromita::ai_runtime::assets::{AssetInput, AssetSource};
    let mut fixture = Fixture::new(ProcessTopology::Split).await;
    fixture
        .add_plugin(
            "verified_weight",
            "InstallPlan(assets=(AssetRequirement('weight', 'weights.bin'),))",
        )
        .await;
    fixture.runtime.plugins_directory = fixture.root.join("plugins");
    let source = fixture.root.join("weight.bin");
    tokio::fs::write(&source, b"weight-bytes").await.unwrap();
    let hash = ring::digest::digest(&ring::digest::SHA256, b"weight-bytes");
    let hash: String = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut selection = model(AiService::Tts, "verified_weight");
    selection.assets = ModelAssets::Sources {
        revision: "v1".into(),
        inputs: vec![AssetInput {
            role: "weight".into(),
            relative_path: "weights.bin".into(),
            sha256: hash,
            source: AssetSource::LocalFile(source),
        }],
    };
    let prepared = fixture.runtime.prepare(&[selection]).await.unwrap();
    assert_eq!(
        prepared.workers[0].models[0].asset_revision.as_deref(),
        Some("v1")
    );
    let model_dir = fixture.root.join("AIData/models/verified_weight/v1");
    assert!(model_dir.join(".neuromita-assets.json").is_file());
    tokio::fs::write(model_dir.join("weights.bin"), b"tampered")
        .await
        .unwrap();
    let mut existing = model(AiService::Tts, "verified_weight");
    existing.assets = ModelAssets::Installed {
        revision: "v1".into(),
    };
    assert!(matches!(
        fixture.runtime.prepare(&[existing]).await.err(),
        Some(AiProductionError::Plugin(_) | AiProductionError::Assets(_))
    ));
    fixture.close().await;
}

#[tokio::test]
async fn incompatible_interpreter_version_rejected_before_plugin_discovery() {
    let fixture = Fixture::new(ProcessTopology::Split).await;
    let first = fixture.runtime.bootstrap.prepare().await.unwrap();
    tokio::fs::write(
        &first.python_executable,
        "#!/bin/sh\necho 'Python 3.13.5'\n",
    )
    .await
    .unwrap();
    let result = fixture
        .runtime
        .prepare(&[model(AiService::Tts, "mock_tts")])
        .await;
    assert!(matches!(
        result.err(),
        Some(AiProductionError::Bootstrap(
            ManagedRuntimeError::Interpreter(_)
        ))
    ));
    assert!(!fixture.root.join("AIData/overlays").exists());
    fixture.close().await;
}

#[tokio::test]
async fn generated_assets_require_explicit_hook_approval() {
    let mut fixture = Fixture::new(ProcessTopology::Split).await;
    fixture
        .add_plugin(
            "hook_model",
            "InstallPlan(assets=(AssetRequirement('weight', 'weights.bin'),), asset_hooks=(__import__('neuromita_ai_sdk').AssetHook('export', 'source_model'),))",
        )
        .await;
    fixture.runtime.plugins_directory = fixture.root.join("plugins");
    let mut selected = model(AiService::Tts, "hook_model");
    selected.assets = ModelAssets::Generated {
        revision: "v1".into(),
        hook_id: "export".into(),
        source_plugin_id: "source_model".into(),
        source_revision: "v1".into(),
        settings: json!({}),
        approved_hook_id: None,
    };
    let result = fixture.runtime.prepare(&[selected]).await;
    assert!(matches!(
        result.err(),
        Some(AiProductionError::Generated(
            neuromita::ai_runtime::assets::ModelGenerationError::NotApproved
        ))
    ));
    assert!(!fixture.root.join("AIData/models/hook_model/v1").exists());
    fixture.close().await;
}

#[tokio::test]
async fn pinned_catalog_is_resolved_by_production_installer_and_fails_on_hash_change() {
    let mut fixture = Fixture::new(ProcessTopology::Split).await;
    fixture
        .add_plugin(
            "catalog_weight",
            "InstallPlan(assets=(AssetRequirement('weight', 'weights.bin'),))",
        )
        .await;
    fixture.runtime.plugins_directory = fixture.root.join("plugins");
    let source = fixture.root.join("catalog-source.bin");
    tokio::fs::write(&source, b"approved-catalog-weight")
        .await
        .unwrap();
    let sha256 = ring::digest::digest(&ring::digest::SHA256, b"approved-catalog-weight")
        .as_ref()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    let manifest = fixture.root.join("approved-sources.json");
    tokio::fs::write(
        &manifest,
        json!({
            "schema_version": 1,
            "entries": [{
                "plugin_id": "catalog_weight",
                "revision": "v1",
                "assets": [{
                    "role": "weight",
                    "relative_path": "weights.bin",
                    "sha256": sha256,
                    "source": {"kind": "local_file", "path": source.to_string_lossy()}
                }]
            }]
        })
        .to_string(),
    )
    .await
    .unwrap();
    let mut selected = model(AiService::Tts, "catalog_weight");
    selected.assets = ModelAssets::Catalog {
        revision: "v1".into(),
        manifest: manifest.clone(),
    };
    let prepared = fixture.runtime.prepare(&[selected]).await.unwrap();
    assert_eq!(
        prepared.workers[0].models[0].asset_revision.as_deref(),
        Some("v1")
    );
    let installed = fixture
        .root
        .join("AIData/models/catalog_weight/v1/weights.bin");
    assert_eq!(
        tokio::fs::read(&installed).await.unwrap(),
        b"approved-catalog-weight"
    );
    tokio::fs::write(&installed, b"corrupted").await.unwrap();
    let mut selected = model(AiService::Tts, "catalog_weight");
    selected.assets = ModelAssets::Catalog {
        revision: "v1".into(),
        manifest,
    };
    assert!(fixture.runtime.prepare(&[selected]).await.is_err());
    fixture.close().await;
}
