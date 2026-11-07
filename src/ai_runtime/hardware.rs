//! Explicit compute-device selection, persisted backend administration and
//! readiness reporting through a live Python worker.
//!
//! Rust owns the selection, the worker lifecycle and the readiness state.
//! Device inventory and the execution probe happen inside the worker; a probe
//! that only found installed files or a driver never counts as inference
//! readiness.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    AiRuntimeError, AiService, AiSupervisor, ModelActivation, ProcessTopology, PythonHostConfig,
    backend::{BackendKind, load_installed_backend},
    composition::RuntimeComposition,
    layer::{LayerError, valid_segment},
};

pub const HARDWARE_CONFIG_SCHEMA: u32 = 1;
pub const HARDWARE_PROBE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Fp32,
    Fp16,
}

impl Precision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fp32 => "fp32",
            Self::Fp16 => "fp16",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fp32" => Some(Self::Fp32),
            "fp16" => Some(Self::Fp16),
            _ => None,
        }
    }
}

impl std::fmt::Display for Precision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    Cuda,
    DirectMl,
    Remote,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::DirectMl => "dml",
            Self::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceId {
    Cpu,
    Cuda(u32),
    DirectMl(u32),
    Remote,
}

impl DeviceId {
    /// Parse the canonical identifiers this runtime accepts. Aliases such as a
    /// bare `cuda` or padded ordinals are rejected instead of guessed.
    pub fn parse(text: &str) -> Result<Self, HardwareError> {
        let invalid = |reason: &str| HardwareError::InvalidDevice {
            device: text.to_owned(),
            reason: reason.into(),
        };
        if text.trim() != text || text.is_empty() {
            return Err(invalid("whitespace and empty identifiers are not accepted"));
        }
        match text {
            "cpu" => Ok(Self::Cpu),
            "remote" => Ok(Self::Remote),
            _ => {
                if let Some(rest) = text.strip_prefix("cuda:") {
                    return parse_ordinal(rest)
                        .map(Self::Cuda)
                        .ok_or_else(|| invalid("expected cuda:<ordinal>"));
                }
                if let Some(rest) = text.strip_prefix("dml:") {
                    return parse_ordinal(rest)
                        .map(Self::DirectMl)
                        .ok_or_else(|| invalid("expected dml:<ordinal>"));
                }
                Err(invalid("expected cpu, cuda:<ordinal> or dml:<ordinal>"))
            }
        }
    }

    pub fn kind(&self) -> DeviceKind {
        match self {
            Self::Cpu => DeviceKind::Cpu,
            Self::Cuda(_) => DeviceKind::Cuda,
            Self::DirectMl(_) => DeviceKind::DirectMl,
            Self::Remote => DeviceKind::Remote,
        }
    }
}

fn parse_ordinal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ordinal: u32 = text.parse().ok()?;
    // Reject leading zeros so persisted identifiers stay canonical.
    (ordinal.to_string() == text).then_some(ordinal)
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu => formatter.write_str("cpu"),
            Self::Cuda(ordinal) => write!(formatter, "cuda:{ordinal}"),
            Self::DirectMl(ordinal) => write!(formatter, "dml:{ordinal}"),
            Self::Remote => formatter.write_str("remote"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeProfile {
    pub backend: BackendKind,
    pub device: String,
    pub precision: Precision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareSelection {
    pub backend: BackendKind,
    pub backend_revision: String,
    pub device: String,
    pub precision: Precision,
    pub service: AiService,
    pub plugin_id: String,
    pub settings: Value,
}

impl HardwareSelection {
    pub fn validate(&self) -> Result<DeviceId, HardwareError> {
        if !valid_segment(&self.backend_revision) {
            return Err(HardwareError::InvalidSelection(
                "backend_revision must be a safe layer segment".into(),
            ));
        }
        if !valid_segment(&self.plugin_id) {
            return Err(HardwareError::InvalidSelection(
                "plugin_id must match a safe plugin directory name".into(),
            ));
        }
        if !self.settings.is_object() {
            return Err(HardwareError::InvalidSelection(
                "settings must be a JSON object".into(),
            ));
        }
        if self
            .settings
            .as_object()
            .is_some_and(|object| object.contains_key("runtime"))
        {
            return Err(HardwareError::InvalidSelection(
                "settings must not contain the reserved \"runtime\" key".into(),
            ));
        }
        let device = DeviceId::parse(&self.device)?;
        let required = backend_device_kind(self.backend);
        if device.kind() != required {
            return Err(HardwareError::InvalidDevice {
                device: self.device.clone(),
                reason: format!(
                    "backend {} requires a {} device",
                    self.backend.as_str(),
                    required.as_str()
                ),
            });
        }
        if self.precision == Precision::Fp16
            && !matches!(
                self.backend,
                BackendKind::TorchCuda | BackendKind::OnnxCuda | BackendKind::Ctranslate2Cuda
            )
        {
            return Err(HardwareError::UnsupportedPrecision {
                backend: self.backend.as_str(),
                precision: self.precision,
            });
        }
        Ok(device)
    }

    pub fn profile(&self) -> RuntimeProfile {
        RuntimeProfile {
            backend: self.backend,
            device: self.device.clone(),
            precision: self.precision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareConfig {
    pub schema_version: u32,
    pub revision: u64,
    pub selection: HardwareSelection,
}

impl HardwareConfig {
    pub fn validate(&self) -> Result<(), HardwareError> {
        if self.schema_version != HARDWARE_CONFIG_SCHEMA {
            return Err(HardwareError::InvalidSelection(format!(
                "unsupported hardware config schema {}",
                self.schema_version
            )));
        }
        if self.revision == 0 {
            return Err(HardwareError::InvalidSelection(
                "revision must be at least 1".into(),
            ));
        }
        self.selection.validate().map(|_| ())
    }
}

#[derive(Debug, Clone, Default)]
pub struct SelectionUpdate {
    pub backend: Option<BackendKind>,
    pub backend_revision: Option<String>,
    pub device: Option<String>,
    pub precision: Option<Precision>,
    pub service: Option<AiService>,
    pub plugin_id: Option<String>,
    pub settings: Option<Value>,
}

fn require_field<T>(value: Option<T>, name: &str) -> Result<T, HardwareError> {
    value.ok_or_else(|| {
        HardwareError::IncompleteSelection(format!(
            "{name} is required when no persisted selection exists"
        ))
    })
}

pub struct HardwareConfigStore {
    path: PathBuf,
}

impl HardwareConfigStore {
    fn lock(&self) -> Result<std::fs::File, HardwareError> {
        let path = self.path.with_extension("hardware.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| HardwareError::Config {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        file.try_lock().map_err(|error| HardwareError::Config {
            path: self.path.clone(),
            message: error.to_string(),
        })?;
        Ok(file)
    }

    pub(crate) fn verify_current(
        &self,
        expected: &HardwareConfig,
    ) -> Result<std::fs::File, AiRuntimeError> {
        let guard = self
            .lock()
            .map_err(|error| AiRuntimeError::ActivationGuard(error.to_string()))?;
        if self
            .load()
            .map_err(|error| AiRuntimeError::ActivationGuard(error.to_string()))?
            != *expected
        {
            return Err(AiRuntimeError::ActivationSuperseded);
        }
        Ok(guard)
    }
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<HardwareConfig, HardwareError> {
        if let Ok(metadata) = std::fs::metadata(&self.path)
            && metadata.len() > 1024 * 1024
        {
            return Err(HardwareError::Config {
                path: self.path.clone(),
                message: "hardware config exceeds 1 MiB".into(),
            });
        }
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(HardwareError::MissingConfig(self.path.clone()));
            }
            Err(error) => {
                return Err(HardwareError::Config {
                    path: self.path.clone(),
                    message: error.to_string(),
                });
            }
        };
        let config: HardwareConfig =
            serde_json::from_slice(&bytes).map_err(|error| HardwareError::Config {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        config.validate()?;
        Ok(config)
    }

    /// Persist an explicit selection. Fields absent from `update` are inherited
    /// from the existing document; a new document requires every field.
    pub fn select(&self, update: SelectionUpdate) -> Result<HardwareConfig, HardwareError> {
        let _guard = self.lock()?;
        let previous = match self.load() {
            Ok(config) => Some(config),
            Err(HardwareError::MissingConfig(_)) => None,
            Err(error) => return Err(error),
        };
        let selection = match previous.as_ref() {
            Some(config) => HardwareSelection {
                backend: update.backend.unwrap_or(config.selection.backend),
                backend_revision: update
                    .backend_revision
                    .unwrap_or_else(|| config.selection.backend_revision.clone()),
                device: update
                    .device
                    .unwrap_or_else(|| config.selection.device.clone()),
                precision: update.precision.unwrap_or(config.selection.precision),
                service: update.service.unwrap_or(config.selection.service),
                plugin_id: update
                    .plugin_id
                    .unwrap_or_else(|| config.selection.plugin_id.clone()),
                settings: update
                    .settings
                    .unwrap_or_else(|| config.selection.settings.clone()),
            },
            None => HardwareSelection {
                backend: require_field(update.backend, "--backend")?,
                backend_revision: require_field(update.backend_revision, "--revision")?,
                device: require_field(update.device, "--device")?,
                precision: require_field(update.precision, "--precision")?,
                service: require_field(update.service, "--service")?,
                plugin_id: require_field(update.plugin_id, "--plugin")?,
                settings: require_field(update.settings, "--settings")?,
            },
        };
        selection.validate()?;
        let revision = match previous.as_ref() {
            Some(config) => config
                .revision
                .checked_add(1)
                .ok_or_else(|| HardwareError::InvalidSelection("revision overflow".into()))?,
            None => 1,
        };
        let config = HardwareConfig {
            schema_version: HARDWARE_CONFIG_SCHEMA,
            revision,
            selection,
        };
        self.write(&config)?;
        Ok(config)
    }

    pub fn save(&self, config: &HardwareConfig) -> Result<(), HardwareError> {
        let _guard = self.lock()?;
        self.write(config)
    }

    fn write(&self, config: &HardwareConfig) -> Result<(), HardwareError> {
        config.validate()?;
        let bytes = serde_json::to_vec_pretty(config).map_err(|error| HardwareError::Config {
            path: self.path.clone(),
            message: error.to_string(),
        })?;
        if bytes.len() > 1024 * 1024 {
            return Err(HardwareError::InvalidSelection(
                "hardware config exceeds 1 MiB".into(),
            ));
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| HardwareError::Config {
            path: self.path.clone(),
            message: error.to_string(),
        })
    }
}

#[derive(Debug, Error)]
pub enum HardwareError {
    #[error("hardware selection is incomplete: {0}")]
    IncompleteSelection(String),
    #[error("invalid hardware selection: {0}")]
    InvalidSelection(String),
    #[error("invalid device {device}: {reason}")]
    InvalidDevice { device: String, reason: String },
    #[error("precision {precision} is not allowed for backend {backend}")]
    UnsupportedPrecision {
        backend: &'static str,
        precision: Precision,
    },
    #[error("hardware config is missing: {0}")]
    MissingConfig(PathBuf),
    #[error("hardware config {path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("backend {backend} revision {revision} is not installed ({path})")]
    BackendNotInstalled {
        backend: &'static str,
        revision: String,
        path: String,
    },
    #[error("backend layer is inconsistent: {0}")]
    BackendLayer(String),
    #[error("device {device} is absent in the worker: {reason}")]
    DeviceUnavailable { device: String, reason: String },
    #[error("device {device} is present but not executable: {reason}")]
    DeviceNotExecutable { device: String, reason: String },
    #[error("python worker failed: {0}")]
    Worker(String),
    #[error("model {plugin_id} failed to load: {reason}")]
    ModelLoadFailed { plugin_id: String, reason: String },
    #[error("worker did not confirm the selected runtime profile for {plugin_id}")]
    RuntimeProfileNotConfirmed { plugin_id: String },
    #[error("worker did not report {plugin_id} as loaded and ready")]
    ModelNotReady { plugin_id: String },
    #[error("selection changed while activation was in progress; the previous worker was kept")]
    StaleSwitch,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareProbe {
    pub schema_version: u32,
    pub platform: String,
    pub python: String,
    #[serde(default)]
    pub adapter_error: String,
    #[serde(default)]
    pub cuda_error: String,
    #[serde(default)]
    pub adapters: Vec<PhysicalAdapter>,
    pub backends: Vec<BackendAvailability>,
    pub devices: Vec<ProbeDevice>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalAdapter {
    #[serde(default)]
    pub index: u32,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub vendor_id: String,
    #[serde(default)]
    pub dedicated_vram_bytes: u64,
    #[serde(default)]
    pub luid: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendAvailability {
    pub kind: String,
    pub available: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeDevice {
    pub backend: String,
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub compute_capability: String,
    #[serde(default)]
    pub memory_bytes: u64,
    #[serde(default)]
    pub driver_version: String,
    pub fp16: bool,
    #[serde(default)]
    pub fp16_reason: String,
    #[serde(default)]
    pub compute_types: Vec<String>,
    #[serde(default)]
    pub luid: String,
}

impl HardwareProbe {
    pub fn backend(&self, family: &str) -> Option<&BackendAvailability> {
        self.backends.iter().find(|backend| backend.kind == family)
    }

    pub fn device(&self, backend: &str, device: &DeviceId) -> Option<&ProbeDevice> {
        let id = device.to_string();
        self.devices.iter().find(|item| {
            item.backend == backend && item.kind == device.kind().as_str() && item.id == id
        })
    }
}

/// Prove that the worker reports the selected backend and device. Presence of
/// files or a driver alone never satisfies this check.
pub fn verify_probe(profile: &RuntimeProfile, probe: &HardwareProbe) -> Result<(), String> {
    if probe.schema_version != HARDWARE_PROBE_SCHEMA {
        return Err(format!(
            "unsupported worker hardware schema {}",
            probe.schema_version
        ));
    }
    let family = backend_family(profile.backend);
    let backend = probe
        .backend(family)
        .ok_or_else(|| format!("worker did not report the {family} backend"))?;
    if !backend.available {
        return Err(format!(
            "{family} backend is unavailable{}",
            if backend.detail.is_empty() {
                String::new()
            } else {
                format!(": {}", backend.detail)
            }
        ));
    }
    let device = DeviceId::parse(&profile.device).map_err(|error| error.to_string())?;
    let record = probe
        .device(family, &device)
        .ok_or_else(|| format!("worker does not report device {device}"))?;
    if profile.precision == Precision::Fp16 && !record.fp16 {
        return Err(format!(
            "FP16 is not allowed on {device}{}",
            if record.fp16_reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", record.fp16_reason)
            }
        ));
    }
    Ok(())
}

/// Interpret the worker execution probe. `verified: false` is a failed
/// readiness check, not a warning.
pub fn verify_execution(result: &Value) -> Result<(), String> {
    if result.get("verified").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let reason = result
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| result.get("detail").and_then(Value::as_str))
        .unwrap_or("worker did not verify execution on the selected device");
    Err(reason.to_owned())
}

pub fn backend_family(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::TorchCpu | BackendKind::TorchCuda => "torch",
        BackendKind::Ctranslate2Cpu | BackendKind::Ctranslate2Cuda => "ctranslate2",
        BackendKind::RemoteApi => "remote_api",
        BackendKind::OnnxCpu | BackendKind::OnnxCuda | BackendKind::OnnxDirectml => "onnxruntime",
    }
}

pub fn backend_device_kind(kind: BackendKind) -> DeviceKind {
    match kind {
        BackendKind::TorchCpu | BackendKind::OnnxCpu => DeviceKind::Cpu,
        BackendKind::TorchCuda | BackendKind::OnnxCuda => DeviceKind::Cuda,
        BackendKind::OnnxDirectml => DeviceKind::DirectMl,
        BackendKind::Ctranslate2Cpu => DeviceKind::Cpu,
        BackendKind::Ctranslate2Cuda => DeviceKind::Cuda,
        BackendKind::RemoteApi => DeviceKind::Remote,
    }
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub backend: BackendKind,
    pub revision: String,
    pub probe: HardwareProbe,
}

#[derive(Debug, Clone)]
pub struct ReadyReport {
    pub generation: uuid::Uuid,
    pub backend: BackendKind,
    pub revision: String,
    pub device: String,
    pub device_name: Option<String>,
    pub precision: Precision,
    pub plugin_id: String,
}

pub struct HardwareRuntime<'a> {
    application: &'a super::application::RunningAiApplication,
    supervisor: &'a AiSupervisor,
    store: HardwareConfigStore,
    backends_directory: PathBuf,
}

pub(crate) fn verify_worker_plan(
    worker: &super::application::AiWorkerSelection,
    config: &HardwareConfig,
) -> Result<(), HardwareError> {
    let selected = &config.selection;
    if !worker.models.iter().any(|model| {
        model.service == selected.service
            && model.plugin_id == selected.plugin_id
            && model.settings == selected.settings
    }) {
        return Err(HardwareError::InvalidSelection(
            "selection differs from application worker plan".into(),
        ));
    }
    for layer in worker.composition.paths() {
        let marker = layer.join(".neuromita-backend.json");
        match std::fs::read(marker) {
            Ok(bytes) => {
                let backend: super::backend::LockedBackend = serde_json::from_slice(&bytes)
                    .map_err(|error| HardwareError::BackendLayer(error.to_string()))?;
                if backend.kind == selected.backend && backend.revision == selected.backend_revision
                {
                    backend
                        .validate()
                        .map_err(|error| HardwareError::BackendLayer(error.to_string()))?;
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(HardwareError::BackendLayer(error.to_string())),
        }
    }
    Err(HardwareError::BackendLayer(
        "selected backend revision is absent from the resolved production composition".into(),
    ))
}

impl<'a> HardwareRuntime<'a> {
    pub fn new(
        application: &'a super::application::RunningAiApplication,
        data_directory: PathBuf,
        config_path: PathBuf,
    ) -> Self {
        Self {
            application,
            supervisor: application.supervisor(),
            store: HardwareConfigStore::new(config_path),
            backends_directory: data_directory.join("backends"),
        }
    }

    pub fn supervisor(&self) -> &AiSupervisor {
        self.supervisor
    }

    pub fn store(&self) -> &HardwareConfigStore {
        &self.store
    }

    /// Inventory the installed backend layer. This starts a one-shot worker and
    /// stops it before returning; no model is loaded.
    pub async fn inspect(
        &self,
        backend: BackendKind,
        revision: &str,
    ) -> Result<Inspection, HardwareError> {
        let (_, path) = load_installed_backend(&self.backends_directory, backend, revision)
            .map_err(|error| self.layer_error(error, backend, revision))?;
        let value = self
            .supervisor
            .inspect_layers(std::slice::from_ref(&path))
            .await
            .map_err(|error| self.runtime_error(error))?;
        let probe: HardwareProbe = serde_json::from_value(value)
            .map_err(|error| HardwareError::Worker(format!("invalid hardware probe: {error}")))?;
        Ok(Inspection {
            backend,
            revision: revision.to_owned(),
            probe,
        })
    }

    /// Apply an explicit selection. The candidate worker must prove device
    /// execution, load the model and confirm the runtime profile before it is
    /// published. Publication happens only while the persisted revision still
    /// matches the applied snapshot.
    pub async fn apply(
        &self,
        config: &HardwareConfig,
        worker: &super::application::AiWorkerSelection,
    ) -> Result<ReadyReport, HardwareError> {
        config.validate()?;
        let selection = &config.selection;
        let device = selection.validate()?;
        if self.application.worker_services(selection.service)
            != Some(worker.composition.services())
        {
            return Err(HardwareError::InvalidSelection(
                "switch must preserve the application's complete worker service set".into(),
            ));
        }
        verify_worker_plan(worker, config)?;
        let composition = &worker.composition;
        let models = &worker.models;
        if !models.iter().any(|model| {
            model.service == selection.service
                && model.plugin_id == selection.plugin_id
                && model.settings == selection.settings
        }) {
            return Err(HardwareError::InvalidSelection(
                "selection differs from the application worker plan".into(),
            ));
        }
        let profile = selection.profile();
        let store = &self.store;
        let verify = || store.verify_current(config);
        let generation = self
            .supervisor
            .activate_hardware(selection.service, composition, models, &profile, &verify)
            .await
            .map_err(|error| self.runtime_error(error))?;
        Ok(ReadyReport {
            generation,
            backend: selection.backend,
            revision: selection.backend_revision.clone(),
            device: device.to_string(),
            device_name: None,
            precision: selection.precision,
            plugin_id: selection.plugin_id.clone(),
        })
    }

    pub async fn stop(&self, service: AiService) -> Result<(), AiRuntimeError> {
        self.supervisor.stop(service).await
    }

    fn runtime_error(&self, error: AiRuntimeError) -> HardwareError {
        match error {
            AiRuntimeError::DeviceUnavailable { device, reason } => {
                HardwareError::DeviceUnavailable { device, reason }
            }
            AiRuntimeError::DeviceNotExecutable { device, reason } => {
                HardwareError::DeviceNotExecutable { device, reason }
            }
            AiRuntimeError::ModelLoadFailed { plugin_id, reason } => {
                HardwareError::ModelLoadFailed { plugin_id, reason }
            }
            AiRuntimeError::RuntimeProfileNotConfirmed { plugin_id } => {
                HardwareError::RuntimeProfileNotConfirmed { plugin_id }
            }
            AiRuntimeError::ActivationSuperseded => HardwareError::StaleSwitch,
            AiRuntimeError::ActivationGuard(message) => HardwareError::Config {
                path: self.store.path().to_path_buf(),
                message,
            },
            other => HardwareError::Worker(other.to_string()),
        }
    }

    fn layer_error(
        &self,
        error: LayerError,
        backend: BackendKind,
        revision: &str,
    ) -> HardwareError {
        match error {
            LayerError::NotInstalled(path) => HardwareError::BackendNotInstalled {
                backend: backend.as_str(),
                revision: revision.to_owned(),
                path,
            },
            other => HardwareError::BackendLayer(other.to_string()),
        }
    }
}

pub async fn run_cli(arguments: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
    match Invocation::parse(arguments)? {
        Invocation::Help => {
            print!("{HELP}");
            Ok(())
        }
        Invocation::Select(options) => run_select(options),
        Invocation::Inspect(options) => run_inspect(options).await,
        Invocation::Apply(options) => run_apply(options).await,
    }
}

const HELP: &str = "neuromita hardware inspect --config FILE | --backend KIND --revision REV
    --python EXE --sdk DIR --plugins DIR [--data DIR] [--topology shared|split] [--timeout SECS]
  neuromita hardware select --config FILE [--backend KIND] [--revision REV] [--device DEVICE]
    [--precision fp32|fp16] [--service tts|asr|embedding|reranker|beats] [--plugin ID] [--settings FILE]
  neuromita hardware apply --config FILE --composition FILE --models FILE --python EXE --sdk DIR --plugins DIR --data DIR
    [--topology shared|split] [--timeout SECS]

KIND: torch_cpu, torch_cuda, onnx_cpu, onnx_cuda, onnx_directml,
  ctranslate2_cpu, ctranslate2_cuda, remote_api.
DEVICE: cpu, cuda:<ordinal>, dml:<ordinal> or remote. FP16 is accepted only on CUDA
backends and only when the worker reports the device as FP16 capable.
`inspect` is read-only inventory. `select` persists the explicit choice.
`apply` loads the model and reports readiness; source availability alone is
never reported as ready.
";

enum Invocation {
    Help,
    Inspect(Options),
    Select(Options),
    Apply(Options),
}

#[derive(Default)]
struct Options {
    config: Option<PathBuf>,
    composition: Option<PathBuf>,
    models: Option<PathBuf>,
    python: Option<PathBuf>,
    sdk: Option<PathBuf>,
    plugins: Option<PathBuf>,
    data: Option<PathBuf>,
    backend: Option<String>,
    revision: Option<String>,
    device: Option<String>,
    precision: Option<String>,
    service: Option<String>,
    plugin: Option<String>,
    settings: Option<PathBuf>,
    topology: ProcessTopology,
    timeout: Option<u64>,
}

impl Invocation {
    fn parse(arguments: Vec<OsString>) -> io::Result<Self> {
        let mut arguments = arguments.into_iter();
        let Some(command) = arguments.next() else {
            return Ok(Self::Help);
        };
        let command = command.to_string_lossy();
        if command == "--help" || command == "help" {
            return Ok(Self::Help);
        }
        let kind = match command.as_ref() {
            "inspect" => 0,
            "select" => 1,
            "apply" => 2,
            _ => {
                return Err(invalid(
                    "expected `hardware inspect`, `hardware select` or `hardware apply`; use --help",
                ));
            }
        };
        let mut options = Options::default();
        let mut topology: Option<ProcessTopology> = None;
        while let Some(argument) = arguments.next() {
            let argument = argument.to_string_lossy();
            if argument == "--help" {
                return Ok(Self::Help);
            }
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| invalid("option requires a value"))
            };
            match argument.as_ref() {
                "--config" => options.config = unique_path(options.config, value()?)?,
                "--composition" => {
                    options.composition = unique_path(options.composition, value()?)?
                }
                "--models" => options.models = unique_path(options.models, value()?)?,
                "--python" => options.python = unique_path(options.python, value()?)?,
                "--sdk" => options.sdk = unique_path(options.sdk, value()?)?,
                "--plugins" => options.plugins = unique_path(options.plugins, value()?)?,
                "--data" => options.data = unique_path(options.data, value()?)?,
                "--settings" => options.settings = unique_path(options.settings, value()?)?,
                "--backend" => options.backend = unique_text(options.backend, value()?)?,
                "--revision" => options.revision = unique_text(options.revision, value()?)?,
                "--device" => options.device = unique_text(options.device, value()?)?,
                "--precision" => options.precision = unique_text(options.precision, value()?)?,
                "--service" => options.service = unique_text(options.service, value()?)?,
                "--plugin" => options.plugin = unique_text(options.plugin, value()?)?,
                "--topology" => {
                    let next = match value()?.to_str() {
                        Some("shared") => ProcessTopology::Shared,
                        Some("split") => ProcessTopology::Split,
                        _ => return Err(invalid("--topology requires shared or split")),
                    };
                    if topology.replace(next).is_some() {
                        return Err(invalid("duplicate --topology"));
                    }
                }
                "--timeout" => {
                    let seconds: u64 = value()?
                        .to_str()
                        .and_then(|text| text.parse().ok())
                        .ok_or_else(|| invalid("--timeout requires whole seconds"))?;
                    if seconds == 0 {
                        return Err(invalid("--timeout must be positive"));
                    }
                    if options.timeout.replace(seconds).is_some() {
                        return Err(invalid("duplicate --timeout"));
                    }
                }
                _ => return Err(invalid("unknown hardware option; use --help")),
            }
        }
        options.topology = topology.unwrap_or_default();
        Ok(match kind {
            0 => Self::Inspect(options),
            1 => Self::Select(options),
            _ => Self::Apply(options),
        })
    }
}

fn unique_path(slot: Option<PathBuf>, value: OsString) -> io::Result<Option<PathBuf>> {
    if value.is_empty() {
        return Err(invalid("option requires a path"));
    }
    if slot.is_some() {
        return Err(invalid("duplicate path option"));
    }
    Ok(Some(PathBuf::from(value)))
}

fn unique_text(slot: Option<String>, value: OsString) -> io::Result<Option<String>> {
    let value = value
        .into_string()
        .map_err(|_| invalid("option value must be valid Unicode"))?;
    if slot.is_some() {
        return Err(invalid("duplicate option"));
    }
    Ok(Some(value))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn run_select(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let path = options
        .config
        .ok_or_else(|| invalid("--config is required for hardware select"))?;
    let backend = options
        .backend
        .map(|text| {
            BackendKind::parse(&text).ok_or_else(|| {
                invalid(&format!(
                    "unknown backend {text}; use --help for accepted names"
                ))
            })
        })
        .transpose()?;
    let service = options
        .service
        .map(|text| match text.as_str() {
            "tts" => Ok(AiService::Tts),
            "asr" => Ok(AiService::Asr),
            "embedding" => Ok(AiService::Embedding),
            "reranker" => Ok(AiService::Reranker),
            "beats" => Ok(AiService::Beats),
            _ => Err(invalid(
                "unknown service; expected tts, asr, embedding, reranker or beats",
            )),
        })
        .transpose()?;
    let precision = options
        .precision
        .map(|text| {
            Precision::parse(&text)
                .ok_or_else(|| invalid("unknown precision; expected fp32 or fp16"))
        })
        .transpose()?;
    let settings = options
        .settings
        .map(|path| {
            let bytes = std::fs::read(&path)
                .map_err(|error| invalid(&format!("cannot read {}: {error}", path.display())))?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|error| invalid(&format!("settings must be JSON: {error}")))?;
            if !value.is_object() {
                return Err(invalid("settings file must contain a JSON object"));
            }
            Ok(value)
        })
        .transpose()?;
    let store = HardwareConfigStore::new(&path);
    let config = store.select(SelectionUpdate {
        backend,
        backend_revision: options.revision,
        device: options.device,
        precision,
        service,
        plugin_id: options.plugin,
        settings,
    })?;
    println!(
        "Selection persisted: {} (revision {})",
        path.display(),
        config.revision
    );
    print_selection(&config.selection);
    println!("Next: neuromita hardware apply --config {}", path.display());
    Ok(())
}

async fn run_inspect(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = options.config.clone();
    let (backend, revision) = match (
        &config_path,
        options.backend.as_deref(),
        options.revision.as_deref(),
    ) {
        (Some(config), None, None) => {
            let config = HardwareConfigStore::new(config).load()?;
            (config.selection.backend, config.selection.backend_revision)
        }
        (_, Some(backend), Some(revision)) => {
            let backend = BackendKind::parse(backend)
                .ok_or_else(|| invalid("unknown backend; use --help for accepted names"))?;
            (backend, revision.to_owned())
        }
        _ => return Err(invalid("inspect requires --config or --backend with --revision").into()),
    };
    let data = options
        .data
        .as_ref()
        .ok_or_else(|| invalid("--data is required for hardware inspect"))?;
    let host = host_config(&options, duration(options.timeout))?;
    let (_, path) = load_installed_backend(&data.join("backends"), backend, &revision)?;
    let supervisor = AiSupervisor::new(options.topology, host);
    let probe = serde_json::from_value(supervisor.inspect_layers(&[path]).await?)?;
    let inspection = Inspection {
        backend,
        revision,
        probe,
    };
    println!(
        "Backend: {} revision {} (Python {})",
        backend.as_str(),
        inspection.revision,
        inspection.probe.python
    );
    if inspection.probe.adapters.is_empty() {
        println!("Physical adapters: none reported");
    } else {
        println!("Physical adapters: {}", inspection.probe.adapters.len());
        for (index, adapter) in inspection.probe.adapters.iter().enumerate() {
            println!(
                "  [{index}] {} ({}, {} bytes, luid {})",
                adapter.name,
                if adapter.vendor.is_empty() {
                    "unknown"
                } else {
                    &adapter.vendor
                },
                adapter.dedicated_vram_bytes,
                if adapter.luid.is_empty() {
                    "-"
                } else {
                    &adapter.luid
                }
            );
        }
    }
    println!("Backends:");
    for entry in &inspection.probe.backends {
        if entry.available {
            println!(
                "  {} {} {}",
                entry.kind,
                entry.version,
                if entry.providers.is_empty() {
                    String::new()
                } else {
                    format!("providers={}", entry.providers.join(","))
                }
            );
        } else {
            println!("  {} unavailable: {}", entry.kind, entry.detail);
        }
    }
    println!("Executable devices:");
    if inspection.probe.devices.is_empty() {
        println!("  none reported");
    }
    for device in &inspection.probe.devices {
        println!(
            "  {} backend={} name={:?} compute={} memory={} fp16={} ({})",
            device.id,
            device.backend,
            device.name,
            if device.compute_capability.is_empty() {
                "-"
            } else {
                &device.compute_capability
            },
            device.memory_bytes,
            if device.fp16 { "yes" } else { "no" },
            if device.fp16_reason.is_empty() {
                "-"
            } else {
                &device.fp16_reason
            }
        );
    }
    println!("Inventory only: no model is loaded and no inference readiness is claimed.");
    Ok(())
}

async fn run_apply(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = options
        .config
        .as_ref()
        .ok_or_else(|| invalid("--config is required for hardware apply"))?
        .clone();
    let host = host_config(&options, duration(options.timeout))?;
    let data = options
        .data
        .clone()
        .ok_or_else(|| invalid("--data is required for hardware apply"))?;
    let store = HardwareConfigStore::new(&config_path);
    let config = store.load()?;
    let composition_path = options.composition.as_ref().ok_or_else(|| {
        invalid("--composition FILE is required; use the installed production backend/overlay plan")
    })?;
    let selections: Vec<super::composition::ServiceSelection> =
        serde_json::from_slice(&std::fs::read(composition_path)?)?;
    let installer = super::backend::BackendInstaller {
        uv_executable: PathBuf::new(),
        python_executable: host.python_executable.clone(),
        backends_directory: data.join("backends"),
        cache_directory: data.join("cache"),
        timeout: host.request_timeout,
    };
    let overlays = super::overlay::OverlayInstaller {
        uv_executable: PathBuf::new(),
        python_executable: host.python_executable.clone(),
        overlays_directory: data.join("overlays"),
        cache_directory: data.join("cache"),
        timeout: host.request_timeout,
    };
    let composition = RuntimeComposition::resolve(
        options.topology,
        config.selection.service,
        &selections,
        &installer,
        &overlays,
    )
    .await?;
    let models_path = options.models.as_ref().ok_or_else(|| {
        invalid("--models FILE is required; include explicit plugin/settings and installed asset revisions")
    })?;
    let models: Vec<ModelActivation> = serde_json::from_slice(&std::fs::read(models_path)?)?;
    let worker = super::application::AiWorkerSelection {
        service: config.selection.service,
        composition,
        models,
    };
    let app = super::application::AiApplicationConfig {
        topology: options.topology,
        host,
        workers: vec![worker],
        voice_profiles: None,
    }
    .activate_with_hardware(&config, &store)
    .await?;
    println!(
        "Model loaded with verified backend={} device={} precision={} plugin={}; stopping verification session.",
        config.selection.backend.as_str(),
        config.selection.device,
        config.selection.precision,
        config.selection.plugin_id
    );
    app.shutdown().await?;
    Ok(())
}
fn host_config(options: &Options, timeout: Duration) -> io::Result<PythonHostConfig> {
    let python = options
        .python
        .clone()
        .ok_or_else(|| invalid("--python is required"))?;
    let sdk = options
        .sdk
        .clone()
        .ok_or_else(|| invalid("--sdk is required"))?;
    let plugins = options
        .plugins
        .clone()
        .ok_or_else(|| invalid("--plugins is required"))?;
    Ok(PythonHostConfig {
        python_executable: python,
        sdk_root: sdk,
        models_directory: options.data.as_ref().map(|data| data.join("models")),
        plugins_directory: plugins,
        request_timeout: timeout,
    })
}

fn duration(seconds: Option<u64>) -> Duration {
    Duration::from_secs(seconds.unwrap_or(120))
}

fn print_selection(selection: &HardwareSelection) {
    println!(
        "Backend: {} revision {}",
        selection.backend.as_str(),
        selection.backend_revision
    );
    println!(
        "Device: {} precision {}",
        selection.device, selection.precision
    );
    println!(
        "Service: {} plugin {}",
        selection.service.kind(),
        selection.plugin_id
    );
    println!("Plugin settings: configured (values omitted)");
}
