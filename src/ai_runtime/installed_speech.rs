//! Read-only activation planning for previously installed speech environments.
//! No bootstrap, package installation, asset download or backend substitution occurs here.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;

use crate::settings::SpeechSettings;

use super::{
    AiService, ProcessTopology, PythonHostConfig,
    application::{AiApplicationConfig, AiWorkerSelection},
    backend::{BackendInstaller, BackendKind, LockedBackend, load_installed_backend},
    bootstrap::{PythonBootstrap, PythonBootstrapState},
    composition::{RuntimeComposition, ServiceSelection},
    fetch_install_plan_for,
    install::PluginInstallPlan,
    layer::valid_segment,
    overlay::{LockedOverlay, OverlayInstaller, load_installed_overlay},
    speech_catalog::{SpeechInventory, SpeechModelCatalog},
    speech_selection::{inspect_speech_selection, select_speech_model},
};

#[derive(Debug, Error)]
pub enum InstalledSpeechError {
    #[error("installed speech environment: {0}")]
    Environment(String),
    #[error("speech model selection: {0}")]
    Selection(#[from] super::speech_selection::SpeechSelectionError),
    #[error("installed speech layer: {0}")]
    Layer(String),
    #[error("installed composition: {0}")]
    Composition(#[from] super::composition::CompositionError),
    #[error("Python plugin metadata: {0}")]
    Inspection(#[from] super::AiRuntimeError),
}

#[derive(Clone)]
pub struct InstalledSpeechEnvironment {
    pub data_directory: PathBuf,
    pub sdk_root: PathBuf,
    pub plugins_directory: PathBuf,
    pub timeout: Duration,
}

impl InstalledSpeechEnvironment {
    pub fn for_desktop(data_directory: PathBuf) -> Self {
        let installed_sdk = data_directory.join("AIData/sdk");
        let source_sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let sdk_root = if installed_sdk.join("neuromita_ai_sdk/host.py").is_file() {
            installed_sdk
        } else {
            source_sdk.clone()
        };
        let installed_plugins = data_directory.join("AIData/Scripts");
        let plugins_directory = if installed_plugins.is_dir() {
            installed_plugins
        } else {
            source_sdk.join("plugins/builtin")
        };
        Self {
            data_directory,
            sdk_root,
            plugins_directory,
            timeout: Duration::from_secs(30),
        }
    }

    fn ai_data(&self) -> PathBuf {
        self.data_directory.join("AIData")
    }

    fn validate(&self) -> Result<(), InstalledSpeechError> {
        if !self.data_directory.is_absolute()
            || !self.sdk_root.is_absolute()
            || !self.sdk_root.join("neuromita_ai_sdk/host.py").is_file()
            || !self.plugins_directory.is_absolute()
            || !self.plugins_directory.is_dir()
            || self.timeout.is_zero()
        {
            return Err(InstalledSpeechError::Environment(
                "absolute data, SDK, installed plugin directory and timeout required".into(),
            ));
        }
        Ok(())
    }

    fn installed_uv(&self) -> Result<PathBuf, InstalledSpeechError> {
        let root = self.ai_data().join("bootstrap/uv");
        let mut matches = Vec::new();
        for entry in entries(&root)? {
            let path = entry.join(if cfg!(windows) { "uv.exe" } else { "uv" });
            if path.is_file() && !path.is_symlink() {
                matches.push(path);
            }
        }
        unique(matches, "installed uv binary")
    }

    async fn managed_python(&self, version: &str) -> Result<PathBuf, InstalledSpeechError> {
        let uv = self.installed_uv()?;
        let bootstrap = PythonBootstrap::new(uv, self.ai_data(), version.to_owned());
        match bootstrap
            .probe()
            .await
            .map_err(|e| InstalledSpeechError::Environment(e.to_string()))?
        {
            PythonBootstrapState::Ready(path) => Ok(path),
            _ => Err(InstalledSpeechError::Environment(format!(
                "managed CPython {version} is not installed"
            ))),
        }
    }

    pub async fn prepare_from_saved(
        &self,
        speech: &SpeechSettings,
    ) -> Result<Option<AiApplicationConfig>, InstalledSpeechError> {
        let inventory = SpeechModelCatalog::for_desktop(self.data_directory.clone())
            .snapshot()
            .await;
        self.prepare(speech, &inventory).await
    }

    pub async fn prepare(
        &self,
        speech: &SpeechSettings,
        inventory: &SpeechInventory,
    ) -> Result<Option<AiApplicationConfig>, InstalledSpeechError> {
        let mut selected = Vec::new();
        for service in [AiService::Tts, AiService::Asr] {
            if let Some(selection) = select_speech_model(speech, inventory, service)? {
                let overlay_directory = self.ai_data().join("overlays").join(&selection.plugin_id);
                if entries(&overlay_directory)?.is_empty() {
                    continue;
                }
                selected.push(selection);
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        self.validate()?;
        for selected in &selected {
            if !self
                .plugins_directory
                .join(&selected.plugin_id)
                .join("plugin.json")
                .is_file()
            {
                return Err(InstalledSpeechError::Environment(format!(
                    "plugin {} absent from configured plugin root",
                    selected.plugin_id
                )));
            }
        }
        let overlays = selected
            .iter()
            .map(|model| installed_overlay(&self.ai_data(), &model.plugin_id))
            .collect::<Result<Vec<_>, _>>()?;
        let version = overlays[0].python_version.clone();
        if overlays
            .iter()
            .any(|overlay| overlay.python_version != version)
        {
            return Err(InstalledSpeechError::Environment(
                "selected speech plugins require different managed Python versions".into(),
            ));
        }
        let python = self.managed_python(&version).await?;
        let host = PythonHostConfig {
            python_executable: python.clone(),
            sdk_root: self.sdk_root.clone(),
            plugins_directory: self.plugins_directory.clone(),
            models_directory: Some(self.ai_data().join("models")),
            request_timeout: self.timeout,
        };
        let backends = BackendInstaller {
            uv_executable: self.installed_uv()?,
            python_executable: python.clone(),
            backends_directory: self.ai_data().join("backends"),
            cache_directory: self.ai_data().join("cache/uv"),
            timeout: self.timeout,
        };
        let overlay_installer = OverlayInstaller {
            uv_executable: backends.uv_executable.clone(),
            python_executable: python,
            overlays_directory: self.ai_data().join("overlays"),
            cache_directory: backends.cache_directory.clone(),
            timeout: self.timeout,
        };
        let mut workers = Vec::new();
        for (selection, overlay) in selected.into_iter().zip(overlays) {
            let plan =
                fetch_install_plan_for(&host, &selection.plugin_id, selection.overrides.clone())
                    .await?;
            plan.validate(&version)
                .map_err(|e| InstalledSpeechError::Layer(e.to_string()))?;
            if !overlay_installer
                .installed(&overlay)
                .await
                .map_err(|e| InstalledSpeechError::Layer(e.to_string()))?
            {
                return Err(InstalledSpeechError::Layer(format!(
                    "overlay {} is missing",
                    selection.plugin_id
                )));
            }
            let locked_backends = select_installed_backends(&self.ai_data(), &plan, &version)?;
            let layer = ServiceSelection {
                service: selection.service,
                backends: locked_backends,
                overlays: vec![overlay],
            };
            let composition = RuntimeComposition::resolve(
                ProcessTopology::Split,
                selection.service,
                &[layer],
                &backends,
                &overlay_installer,
            )
            .await?;
            let inspected = inspect_speech_selection(selection, &host, composition.paths()).await?;
            if plan.backend != inspected.inspection.install_plan.backend
                || plan.supported_backends != inspected.inspection.install_plan.supported_backends
                || plan.required_backends != inspected.inspection.install_plan.required_backends
            {
                return Err(InstalledSpeechError::Layer(format!(
                    "effective settings for {} require a different prepared backend",
                    inspected.activation.plugin_id
                )));
            }
            workers.push(AiWorkerSelection {
                service: inspected.activation.service,
                composition,
                models: vec![inspected.activation],
            });
        }
        Ok(Some(AiApplicationConfig {
            topology: ProcessTopology::Split,
            host,
            workers,
            voice_profiles: None,
        }))
    }
}

fn entries(root: &Path) -> Result<Vec<PathBuf>, InstalledSpeechError> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|e| InstalledSpeechError::Environment(e.to_string()))?
        .take(128)
    {
        let entry = entry.map_err(|e| InstalledSpeechError::Environment(e.to_string()))?;
        if entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn unique<T>(mut values: Vec<T>, label: &str) -> Result<T, InstalledSpeechError> {
    if values.len() != 1 {
        return Err(InstalledSpeechError::Environment(format!(
            "expected exactly one {label}, found {}",
            values.len()
        )));
    }
    Ok(values.remove(0))
}

fn installed_overlay(data: &Path, plugin_id: &str) -> Result<LockedOverlay, InstalledSpeechError> {
    if !valid_segment(plugin_id) {
        return Err(InstalledSpeechError::Environment(
            "invalid plugin ID".into(),
        ));
    }
    let root = data.join("overlays").join(plugin_id);
    let revisions = entries(&root)?;
    let revision = unique(revisions, &format!("installed revision of {plugin_id}"))?;
    let name = revision
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| InstalledSpeechError::Environment("invalid overlay revision".into()))?;
    let (overlay, _) = load_installed_overlay(&data.join("overlays"), plugin_id, name)
        .map_err(|e| InstalledSpeechError::Layer(e.to_string()))?;
    Ok(overlay)
}

fn backend_kind(value: &str) -> Option<BackendKind> {
    match value {
        "torch-cpu" => Some(BackendKind::TorchCpu),
        "torch-cuda" => Some(BackendKind::TorchCuda),
        "onnx-cpu" => Some(BackendKind::OnnxCpu),
        "onnx-cuda" => Some(BackendKind::OnnxCuda),
        "onnx-directml" => Some(BackendKind::OnnxDirectml),
        "ctranslate2-cpu" => Some(BackendKind::Ctranslate2Cpu),
        "ctranslate2-cuda" => Some(BackendKind::Ctranslate2Cuda),
        "remote-api" => Some(BackendKind::RemoteApi),
        _ => None,
    }
}

fn installed_backend(
    data: &Path,
    kind: BackendKind,
    version: &str,
) -> Result<Option<LockedBackend>, InstalledSpeechError> {
    let root = data.join("backends").join(kind.directory());
    let mut found = Vec::new();
    for revision in entries(&root)? {
        let name = revision
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| InstalledSpeechError::Environment("invalid backend revision".into()))?;
        let (locked, _) = load_installed_backend(&data.join("backends"), kind, name)
            .map_err(|e| InstalledSpeechError::Layer(e.to_string()))?;
        if locked.python_version == version {
            found.push(locked);
        }
    }
    if found.len() > 1 {
        return Err(InstalledSpeechError::Environment(format!(
            "multiple installed revisions of {}",
            kind.as_str()
        )));
    }
    Ok(found.pop())
}

fn select_installed_backends(
    data: &Path,
    plan: &PluginInstallPlan,
    version: &str,
) -> Result<Vec<LockedBackend>, InstalledSpeechError> {
    let requested = if let Some(kind) = &plan.backend {
        vec![kind.clone()]
    } else {
        plan.supported_backends.clone()
    };
    if requested.is_empty() {
        return Err(InstalledSpeechError::Environment(
            "plugin did not declare an explicit backend".into(),
        ));
    }
    let mut candidates = Vec::new();
    for name in requested {
        let kind = backend_kind(&name).ok_or_else(|| {
            InstalledSpeechError::Environment(format!("unsupported backend {name}"))
        })?;
        if let Some(backend) = installed_backend(data, kind, version)?
            && !candidates
                .iter()
                .any(|item: &LockedBackend| item.kind == backend.kind)
        {
            candidates.push(backend);
        }
    }
    let chosen = unique(candidates, "compatible installed backend")?;
    let mut selected = vec![chosen];
    for name in &plan.required_backends {
        let kind = backend_kind(name).ok_or_else(|| {
            InstalledSpeechError::Environment(format!("unsupported supplemental backend {name}"))
        })?;
        if selected.iter().any(|backend| backend.kind == kind) {
            continue;
        }
        let backend = installed_backend(data, kind, version)?.ok_or_else(|| {
            InstalledSpeechError::Environment(format!("required backend {name} is not installed"))
        })?;
        selected.push(backend);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::super::overlay::LockedPackage;
    use super::*;

    fn plan(kinds: &[&str], extra: &[&str]) -> PluginInstallPlan {
        PluginInstallPlan {
            packages: vec![],
            requirements: vec![],
            assets: vec![],
            backend: None,
            supported_backends: kinds.iter().map(|s| (*s).into()).collect(),
            required_backends: extra.iter().map(|s| (*s).into()).collect(),
            supported_python_minors: vec![],
            actions: vec![],
            asset_hooks: vec![],
        }
    }

    fn write_backend(data: &Path, kind: BackendKind, revision: &str) {
        let receipt = LockedBackend {
            kind,
            revision: revision.into(),
            python_version: "3.12.10".into(),
            packages: if kind == BackendKind::RemoteApi {
                vec![]
            } else {
                vec![LockedPackage {
                    name: "torch".into(),
                    version: "2.5.1".into(),
                }]
            },
        };
        let folder = data.join("backends").join(kind.directory()).join(revision);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(".neuromita-backend.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn chooses_only_unambiguous_explicit_backend_without_installing() {
        let root = std::env::temp_dir().join(format!("speech-installed-{}", uuid::Uuid::new_v4()));
        write_backend(&root, BackendKind::TorchCpu, "a");
        write_backend(&root, BackendKind::TorchCuda, "b");
        assert!(
            select_installed_backends(&root, &plan(&["torch-cpu", "torch-cuda"], &[]), "3.12.10")
                .is_err()
        );
        let selected =
            select_installed_backends(&root, &plan(&["torch-cuda"], &["torch-cpu"]), "3.12.10")
                .unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].kind, BackendKind::TorchCuda);
        assert_eq!(selected[1].kind, BackendKind::TorchCpu);
        write_backend(&root, BackendKind::TorchCuda, "c");
        assert!(select_installed_backends(&root, &plan(&["torch-cuda"], &[]), "3.12.10").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_overlay_requires_unique_verifiable_receipt() {
        let root = std::env::temp_dir().join(format!("speech-overlay-{}", uuid::Uuid::new_v4()));
        let overlays = root.join("overlays/mock_asr/rev1");
        std::fs::create_dir_all(&overlays).unwrap();
        let receipt = LockedOverlay {
            plugin_id: "mock_asr".into(),
            revision: "rev1".into(),
            python_version: "3.12.10".into(),
            packages: vec![],
            actions: vec![],
        };
        std::fs::write(
            overlays.join(".neuromita-overlay.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(installed_overlay(&root, "mock_asr").unwrap(), receipt);
        std::fs::create_dir_all(root.join("overlays/mock_asr/rev2")).unwrap();
        assert!(installed_overlay(&root, "mock_asr").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn selected_but_uninstalled_asr_remains_inactive_without_failing_startup() {
        let data = std::env::temp_dir().join(format!("speech-missing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let cfg = InstalledSpeechEnvironment {
            data_directory: data.clone(),
            sdk_root: PathBuf::from("/nonexistent"),
            plugins_directory: PathBuf::from("/nonexistent"),
            timeout: Duration::from_secs(5),
        };
        let mut settings = SpeechSettings::default();
        settings.asr.recognizer_id = "gigaam".into();
        let inventory = SpeechInventory {
            asr: vec![super::super::speech_catalog::SpeechModelOption {
                id: "gigaam".into(),
                name: "GigaAM".into(),
                package_detected: false,
                verified_assets: false,
                installed_revisions: vec![],
            }],
            ..Default::default()
        };
        assert!(cfg.prepare(&settings, &inventory).await.unwrap().is_none());
        assert_eq!(settings.asr.recognizer_id, "gigaam");
        std::fs::remove_dir_all(data).unwrap();
    }

    #[tokio::test]
    async fn no_speech_models_selected_does_not_probe_or_start_python() {
        let cfg = InstalledSpeechEnvironment {
            data_directory: PathBuf::from("/nonexistent"),
            sdk_root: PathBuf::from("/nonexistent"),
            plugins_directory: PathBuf::from("/nonexistent"),
            timeout: Duration::from_secs(5),
        };
        assert!(
            cfg.prepare(&SpeechSettings::default(), &SpeechInventory::default())
                .await
                .unwrap()
                .is_none()
        );
    }
}
