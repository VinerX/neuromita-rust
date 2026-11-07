use std::{path::PathBuf, sync::Arc, time::Duration};

use neuromita::ai_runtime::{
    AiService, ModelActivation, ProcessTopology, PythonHostConfig,
    application::{AiApplicationConfig, AiWorkerSelection},
    backend::{BackendInstaller, BackendKind, LockedBackend},
    composition::{RuntimeComposition, ServiceSelection},
    hardware::{
        DeviceId, HardwareConfig, HardwareConfigStore, HardwareProbe, HardwareRuntime,
        HardwareSelection, Precision, SelectionUpdate,
    },
    overlay::OverlayInstaller,
};
use serde_json::{Value, json};

struct Fixture {
    root: PathBuf,
    worker: AiWorkerSelection,
    host: PythonHostConfig,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("neuromita-hardware-{}", uuid::Uuid::new_v4()));
        let package = root.join("sdk/neuromita_ai_sdk");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("__init__.py"), "").unwrap();
        std::fs::write(
            package.join("host.py"),
            include_str!("fixtures/ai_hardware_fixture_host.py"),
        )
        .unwrap();
        let python = std::env::var("NEUROMITA_TEST_PYTHON")
            .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into());
        let version = std::process::Command::new(&python)
            .arg("--version")
            .output()
            .unwrap();
        let version = String::from_utf8(version.stdout)
            .unwrap()
            .trim()
            .strip_prefix("Python ")
            .unwrap()
            .to_owned();
        let backend = LockedBackend {
            kind: BackendKind::TorchCpu,
            revision: "fixture".into(),
            python_version: version,
            packages: vec![neuromita::ai_runtime::overlay::LockedPackage {
                name: "torch".into(),
                version: "2.7.0".into(),
            }],
        };
        let layer = root.join("backends/torch-cpu/fixture");
        std::fs::create_dir_all(&layer).unwrap();
        std::fs::write(
            layer.join(".neuromita-backend.json"),
            serde_json::to_vec(&backend).unwrap(),
        )
        .unwrap();
        let metadata = layer.join("torch-2.7.0.dist-info");
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::write(
            metadata.join("METADATA"),
            "Metadata-Version: 2.1\nName: torch\nVersion: 2.7.0\n",
        )
        .unwrap();
        let backend_installer = BackendInstaller {
            uv_executable: root.join("unused-uv"),
            python_executable: PathBuf::from(&python),
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let overlay_installer = OverlayInstaller {
            uv_executable: root.join("unused-uv"),
            python_executable: PathBuf::from(&python),
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let composition = RuntimeComposition::resolve(
            ProcessTopology::Split,
            AiService::Tts,
            &[ServiceSelection {
                service: AiService::Tts,
                backends: vec![backend],
                overlays: vec![],
            }],
            &backend_installer,
            &overlay_installer,
        )
        .await
        .unwrap();
        let worker = AiWorkerSelection {
            service: AiService::Tts,
            composition,
            models: vec![ModelActivation {
                service: AiService::Tts,
                plugin_id: "fixture_tts".into(),
                settings: json!({}),
                asset_revision: None,
            }],
        };
        let host = PythonHostConfig {
            python_executable: python.into(),
            sdk_root: root.join("sdk"),
            plugins_directory: root.clone(),
            models_directory: None,
            request_timeout: Duration::from_secs(5),
        };
        let fixture = Self { root, worker, host };
        fixture.write(json!({}));
        fixture
    }
    async fn worker_for(
        &self,
        topology: ProcessTopology,
        services: &[AiService],
        backend_kind: BackendKind,
    ) -> AiWorkerSelection {
        let backend = LockedBackend {
            kind: backend_kind,
            revision: "fixture".into(),
            python_version: self.worker.composition.python_version().into(),
            packages: vec![neuromita::ai_runtime::overlay::LockedPackage {
                name: "torch".into(),
                version: "2.7.0".into(),
            }],
        };
        let backend_installer = BackendInstaller {
            uv_executable: self.root.join("unused-uv"),
            python_executable: self.host.python_executable.clone(),
            backends_directory: self.root.join("backends"),
            cache_directory: self.root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let layer = backend_installer.installed_path(&backend).unwrap();
        std::fs::create_dir_all(&layer).unwrap();
        std::fs::write(
            layer.join(".neuromita-backend.json"),
            serde_json::to_vec(&backend).unwrap(),
        )
        .unwrap();
        let metadata = layer.join("torch-2.7.0.dist-info");
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::write(
            metadata.join("METADATA"),
            "Metadata-Version: 2.1\nName: torch\nVersion: 2.7.0\n",
        )
        .unwrap();
        let overlay_installer = OverlayInstaller {
            uv_executable: self.root.join("unused-uv"),
            python_executable: self.host.python_executable.clone(),
            overlays_directory: self.root.join("overlays"),
            cache_directory: self.root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let selections: Vec<_> = services
            .iter()
            .map(|service| ServiceSelection {
                service: *service,
                backends: vec![backend.clone()],
                overlays: vec![],
            })
            .collect();
        let composition = RuntimeComposition::resolve(
            topology,
            services[0],
            &selections,
            &backend_installer,
            &overlay_installer,
        )
        .await
        .unwrap();
        AiWorkerSelection {
            service: services[0],
            composition,
            models: services
                .iter()
                .map(|service| ModelActivation {
                    service: *service,
                    plugin_id: match service {
                        AiService::Tts => "fixture_tts",
                        AiService::Asr => "fixture_asr",
                        _ => panic!("unsupported fixture service"),
                    }
                    .into(),
                    settings: json!({}),
                    asset_revision: None,
                })
                .collect(),
        }
    }

    fn config(&self) -> HardwareConfig {
        HardwareConfig {
            schema_version: 1,
            revision: 1,
            selection: HardwareSelection {
                backend: BackendKind::TorchCpu,
                backend_revision: "fixture".into(),
                device: "cpu".into(),
                precision: Precision::Fp32,
                service: AiService::Tts,
                plugin_id: "fixture_tts".into(),
                settings: json!({}),
            },
        }
    }
    fn write(&self, overrides: Value) {
        let mut value = json!({
            "plugins": [{"id":"fixture_tts", "kind":"tts", "discovery_status":"available"}],
            "hardware": {"schema_version":1,"platform":"fixture","python":"3.12","backends":[{"kind":"torch","available":true}],"devices":[{"backend":"torch","kind":"cpu","id":"cpu","name":"fixture remote","fp16":false}]}
        });
        for (key, item) in overrides.as_object().unwrap() {
            value[key] = item.clone();
        }
        std::fs::write(
            self.root.join("fixture.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    fn app_config(&self) -> AiApplicationConfig {
        AiApplicationConfig {
            topology: ProcessTopology::Split,
            host: self.host.clone(),
            workers: vec![self.worker.clone()],
            voice_profiles: None,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn invalid_device_precision_and_persistence_have_explicit_outcomes() {
    let fixture = Fixture::new().await;
    for invalid in ["", "cuda", "cuda:01", "cuda:-1", "cpu ", "dml:4294967296"] {
        assert!(DeviceId::parse(invalid).is_err(), "{invalid}");
    }
    let store = HardwareConfigStore::new(fixture.root.join("selection.json"));
    let mut config = fixture.config();
    store.save(&config).unwrap();
    assert_eq!(store.load().unwrap(), config);
    config.selection.precision = Precision::Fp16;
    assert!(store.save(&config).is_err());
    assert_eq!(store.load().unwrap(), fixture.config());
    let changed = store
        .select(SelectionUpdate {
            settings: Some(json!({"changed": true})),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(changed.revision, 2);
    assert_eq!(store.load().unwrap(), changed);
}

#[tokio::test]
async fn candidate_failures_preserve_the_running_application_worker() {
    let fixture = Fixture::new().await;
    let store_path = fixture.root.join("selection.json");
    HardwareConfigStore::new(&store_path)
        .save(&fixture.config())
        .unwrap();
    let app = fixture
        .app_config()
        .activate_with_hardware(&fixture.config(), &HardwareConfigStore::new(&store_path))
        .await
        .unwrap();
    let runtime = HardwareRuntime::new(&app, fixture.root.clone(), store_path);
    let before = runtime
        .supervisor()
        .readiness(AiService::Tts)
        .await
        .unwrap();
    for overrides in [
        json!({"hardware":{"schema_version":1,"platform":"fixture","python":"3.12","backends":[],"devices":[]}}),
        json!({"execute_error":"device execution failed"}),
        json!({"initialize":{"fixture_tts":{"error":"failed model load"}}}),
        json!({"initialize":{"fixture_tts":{"ready":false}}}),
        json!({"initialize":{"fixture_tts":{"runtime":null}}}),
    ] {
        fixture.write(overrides);
        assert!(
            runtime
                .apply(&fixture.config(), &fixture.worker)
                .await
                .is_err()
        );
        assert_eq!(
            runtime
                .supervisor()
                .readiness(AiService::Tts)
                .await
                .unwrap(),
            before
        );
    }
    fixture.write(json!({}));
    let accepted = runtime
        .apply(&fixture.config(), &fixture.worker)
        .await
        .unwrap();
    assert_eq!(accepted.device, "cpu");
    drop(runtime);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_persisted_selection_rejects_a_late_candidate() {
    let fixture = Arc::new(Fixture::new().await);
    let path = fixture.root.join("selection.json");
    let store = HardwareConfigStore::new(&path);
    store.save(&fixture.config()).unwrap();
    let app = fixture.app_config().activate().await.unwrap();
    let runtime = HardwareRuntime::new(&app, fixture.root.clone(), path.clone());
    let marker = fixture.root.join("initializing");
    fixture.write(json!({"initialize":{"fixture_tts":{"marker":marker,"delay_ms":300}}}));
    let change = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        store
            .select(SelectionUpdate {
                settings: Some(json!({"changed":true})),
                ..Default::default()
            })
            .unwrap();
    };
    let config = fixture.config();
    let (result, ()) = tokio::join!(runtime.apply(&config, &fixture.worker), change);
    assert!(matches!(
        result,
        Err(neuromita::ai_runtime::hardware::HardwareError::StaleSwitch)
    ));
    let result = runtime
        .supervisor()
        .call(AiService::Tts, "infer", json!({}))
        .await
        .unwrap();
    assert_eq!(result["text"], "fixture");
    drop(runtime);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn core_startup_applies_the_explicit_selection_and_can_recover_from_load_failure() {
    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        generation::{
            GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
        },
    };
    let fixture = Fixture::new().await;
    let path = fixture.root.join("selection.json");
    HardwareConfigStore::new(&path)
        .save(&fixture.config())
        .unwrap();
    let configure = || {
        let template = GeminiTemplate::google(
            "fixture",
            "dummy-key",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
        let limits = RuntimeLimits {
            request_queue: 2.try_into().unwrap(),
            discovery_buffer: 8.try_into().unwrap(),
            character_commands: 8.try_into().unwrap(),
            presenter_updates: 8.try_into().unwrap(),
            retained_finished: 8.try_into().unwrap(),
        };
        let mut configuration = ApplicationConfig::minimal(
            GenerationPreset::new(ProviderTemplate::Gemini(template)),
            ResponseGenerator::new([provider]).unwrap(),
            limits,
            "127.0.0.1:0".parse().unwrap(),
        );
        configuration.ai_runtime = Some(fixture.app_config());
        configuration.ai_hardware = Some(path.clone());
        configuration
    };
    fixture.write(json!({"initialize":{"fixture_tts":{"ready":false}}}));
    assert!(ApplicationLifecycle::start(configure()).await.is_err());
    fixture.write(json!({}));
    ApplicationLifecycle::start(configure())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let mut missing_owner = configure();
    missing_owner.ai_runtime = None;
    assert!(ApplicationLifecycle::start(missing_owner).await.is_err());
}

#[test]
fn real_python_probe_json_crosses_strict_rust_dtos() {
    let python = std::env::var("NEUROMITA_TEST_PYTHON")
        .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into());
    let script = r#"
import json
from types import SimpleNamespace
from unittest.mock import patch
from neuromita_ai_sdk import hardware

torch = SimpleNamespace(tensor=lambda *a, **k: None, float32='float32',
    __version__='fixture', cuda=SimpleNamespace(is_available=lambda: False))
ort = SimpleNamespace(InferenceSession=lambda *a, **k: None,
    SessionOptions=lambda: None, __version__='fixture',
    get_available_providers=lambda: ['CPUExecutionProvider', 'DmlExecutionProvider'])
ct2 = SimpleNamespace(Translator=lambda *a, **k: None, __version__='fixture',
    get_supported_compute_types=lambda device, ordinal=0: {'float32'},
    get_cuda_device_count=lambda: 1)
adapter = dict(index=0, name='fixture DML', vendor='AMD', vendor_id='1002',
    dedicated_vram_bytes=1024, luid='0102030405060708')

def capture(modules, adapters, cuda):
    def load(name):
        if name not in modules:
            raise ModuleNotFoundError(name)
        return modules[name]
    with patch.object(hardware, '_import', side_effect=load), \
         patch.object(hardware, '_dxgi_adapters', return_value=adapters), \
         patch.object(hardware, '_cuda_driver', return_value=cuda):
        return hardware.probe()

print(json.dumps([
    capture({'torch': torch}, [], {'devices': [], 'detail': 'CUDA driver absent'}),
    capture({'torch': torch, 'onnxruntime': ort, 'ctranslate2': ct2}, [adapter], {'devices': []}),
]))
"#;
    let output = std::process::Command::new(python)
        .args(["-B", "-c", script])
        .env(
            "PYTHONPATH",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
        )
        .env("PYTHONNOUSERSITE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let documents: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(documents.len(), 2);
    let cpu: HardwareProbe = serde_json::from_value(documents[0].clone()).unwrap();
    assert_eq!(cpu.cuda_error, "CUDA driver absent");
    assert!(cpu.device("torch", &DeviceId::Cpu).is_some());
    let mixed: HardwareProbe = serde_json::from_value(documents[1].clone()).unwrap();
    assert!(mixed.cuda_error.is_empty());
    assert_eq!(
        mixed
            .device("onnxruntime", &DeviceId::DirectMl(0))
            .unwrap()
            .luid,
        "0102030405060708"
    );
    for device in [DeviceId::Cpu, DeviceId::Cuda(0)] {
        assert_eq!(
            mixed.device("ctranslate2", &device).unwrap().compute_types,
            vec!["float32"]
        );
    }
    assert!(
        mixed
            .device("torch", &DeviceId::Cpu)
            .unwrap()
            .compute_types
            .is_empty()
    );
    let mut unknown = documents[0].clone();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<HardwareProbe>(unknown).is_err());
    let mut unknown = documents[1].clone();
    unknown["devices"][0]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<HardwareProbe>(unknown).is_err());
}

#[tokio::test]
async fn shared_switch_preserves_and_revalidates_neighbor_runtime_profile() {
    let fixture = Fixture::new().await;
    let worker = fixture
        .worker_for(
            ProcessTopology::Shared,
            &[AiService::Tts, AiService::Asr],
            BackendKind::TorchCuda,
        )
        .await;
    let inventory = json!({
        "schema_version":1,"platform":"fixture","python":"3.12",
        "backends":[{"kind":"torch","available":true}],
        "devices":[
            {"backend":"torch","kind":"cuda","id":"cuda:0","fp16":true},
            {"backend":"torch","kind":"cuda","id":"cuda:1","fp16":true}
        ]
    });
    let plugins = json!([
        {"id":"fixture_tts","kind":"tts","discovery_status":"available"},
        {"id":"fixture_asr","kind":"asr","discovery_status":"available"}
    ]);
    fixture.write(json!({"plugins":plugins,"hardware":inventory}));
    let mut config = fixture.config();
    config.selection.backend = BackendKind::TorchCuda;
    config.selection.device = "cuda:1".into();
    config.selection.precision = Precision::Fp16;
    let path = fixture.root.join("selection.json");
    let store = HardwareConfigStore::new(&path);
    store.save(&config).unwrap();
    let application = AiApplicationConfig {
        topology: ProcessTopology::Shared,
        host: fixture.host.clone(),
        workers: vec![worker.clone()],
        voice_profiles: None,
    }
    .activate_with_hardware(&config, &store)
    .await
    .unwrap();
    let runtime = HardwareRuntime::new(&application, fixture.root.clone(), path);
    let expected_neighbor = json!({"backend":"torch_cuda","device":"cuda:1","precision":"fp16"});
    let before = runtime
        .supervisor()
        .readiness(AiService::Tts)
        .await
        .unwrap();
    assert_eq!(before["services"]["tts"]["runtime"], expected_neighbor);
    let selected = store
        .select(SelectionUpdate {
            service: Some(AiService::Asr),
            plugin_id: Some("fixture_asr".into()),
            device: Some("cuda:0".into()),
            precision: Some(Precision::Fp32),
            ..Default::default()
        })
        .unwrap();
    let report = runtime.apply(&selected, &worker).await.unwrap();
    assert_eq!(report.device, "cuda:0");
    let preserved = runtime
        .supervisor()
        .readiness(AiService::Asr)
        .await
        .unwrap();
    assert_eq!(preserved["services"]["tts"]["runtime"], expected_neighbor);
    assert_eq!(
        preserved["services"]["asr"]["runtime"],
        json!({"backend":"torch_cuda","device":"cuda:0","precision":"fp32"})
    );
    // A later switch must also retain profiles published by the previous switch.
    runtime.apply(&selected, &worker).await.unwrap();
    assert_eq!(
        runtime
            .supervisor()
            .readiness(AiService::Tts)
            .await
            .unwrap(),
        preserved
    );
    // The target remains executable but losing the neighbor's device rejects replacement.
    let mut missing_neighbor = inventory.clone();
    missing_neighbor["devices"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    fixture.write(json!({"plugins":plugins,"hardware":missing_neighbor}));
    assert!(runtime.apply(&selected, &worker).await.is_err());
    assert_eq!(
        runtime
            .supervisor()
            .readiness(AiService::Tts)
            .await
            .unwrap(),
        preserved
    );
    // A neighbor that no longer confirms its profile must also reject the whole candidate.
    fixture.write(json!({"plugins":plugins,"hardware":inventory,
        "initialize":{"fixture_tts":{"runtime":null}}}));
    assert!(runtime.apply(&selected, &worker).await.is_err());
    assert_eq!(
        runtime
            .supervisor()
            .readiness(AiService::Asr)
            .await
            .unwrap(),
        preserved
    );
    drop(runtime);
    application.shutdown().await.unwrap();
}

#[tokio::test]
async fn split_startup_rejects_selection_changed_after_selected_worker_before_final_publication() {
    use neuromita::ai_runtime::{AiRuntimeError, application::AiApplicationError};
    let fixture = Fixture::new().await;
    let asr = fixture
        .worker_for(
            ProcessTopology::Split,
            &[AiService::Asr],
            BackendKind::TorchCpu,
        )
        .await;
    let marker = fixture.root.join("second-worker-initializing");
    let first_marker = fixture.root.join("selected-worker-initializing");
    let plugins = json!([
        {"id":"fixture_tts","kind":"tts","discovery_status":"available"},
        {"id":"fixture_asr","kind":"asr","discovery_status":"available"}
    ]);
    fixture.write(json!({"plugins":plugins,"initialize":{
        "fixture_tts":{"marker":first_marker},
        "fixture_asr":{"marker":marker,"delay_ms":300}
    }}));
    let path = fixture.root.join("selection.json");
    let store = HardwareConfigStore::new(&path);
    let selected = fixture.config();
    store.save(&selected).unwrap();
    let configure = || AiApplicationConfig {
        topology: ProcessTopology::Split,
        host: fixture.host.clone(),
        workers: vec![fixture.worker.clone(), asr.clone()],
        voice_profiles: None,
    };
    let change = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(first_marker.exists());
        store
            .select(SelectionUpdate {
                settings: Some(json!({"changed":true})),
                ..Default::default()
            })
            .unwrap()
    };
    let (result, latest) = tokio::join!(
        configure().activate_with_hardware(&selected, &store),
        change
    );
    match result {
        Err(AiApplicationError::Runtime(AiRuntimeError::ActivationSuperseded)) => {}
        Ok(application) => {
            application.shutdown().await.unwrap();
            panic!("split startup published a superseded hardware selection");
        }
        Err(error) => panic!("unexpected startup error: {error}"),
    }
    assert_eq!(store.load().unwrap(), latest);
    assert_eq!(latest.revision, selected.revision + 1);
    // A fresh startup with the new selection remains possible after rollback.
    fixture.write(json!({"plugins":plugins}));
    let mut recovered = configure();
    recovered.workers[0].models[0].settings = latest.selection.settings.clone();
    recovered
        .activate_with_hardware(&latest, &store)
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}
