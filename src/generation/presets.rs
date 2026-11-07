use std::{
    collections::BTreeMap,
    fmt,
    io::Write,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use super::{
    GenerationPreset, ProviderTemplate,
    gemini::{GeminiTemplate, GenerationSettings},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PresetId(pub u32);

#[derive(Debug, Error)]
pub enum PresetCatalogError {
    #[error("preset catalog is unavailable")]
    Unavailable,
    #[error("preset {0:?} does not exist")]
    NotFound(PresetId),
    #[error("preset {0:?} is a runtime-only preset and cannot be edited")]
    RuntimeOnly(PresetId),
    #[error("preset identifier space is exhausted")]
    IdExhausted,
    #[error("invalid preset definition: {0}")]
    Invalid(String),
    #[error("credential reference is unavailable: {0}")]
    MissingCredential(String),
    #[error("preset storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeminiPresetDefinition {
    pub options: super::gemini::GeminiRequestOptions,
    pub transforms: Vec<super::MessageTransform>,
    pub endpoint: String,
    pub model: String,
    /// Explicit credential source; never the secret value itself.
    pub credential: super::CredentialReference,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetProtocol {
    Gemini,
    OpenAi(super::openai::OpenAiProtocol),
}

pub struct PresetTemplate {
    pub protocol: PresetProtocol,
    pub send_temperature: bool,
    pub send_max_output_tokens: bool,
    pub endpoint: String,
    pub credential: super::CredentialReference,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

impl PresetTemplate {
    pub fn output_token_wire_key(&self) -> &'static str {
        match self.protocol {
            PresetProtocol::Gemini => "maxOutputTokens",
            PresetProtocol::OpenAi(super::openai::OpenAiProtocol::Responses) => "max_output_tokens",
            PresetProtocol::OpenAi(_) => "max_tokens",
        }
    }
    pub fn named(id: &str) -> Result<Self, PresetCatalogError> {
        use super::openai::OpenAiProtocol::*;
        if id == "gemini" {
            return Ok(Self::for_provider(super::ProviderKind::Gemini));
        }
        let (protocol, endpoint, credential) = match id {
            "compatible" => (
                Compatible,
                "https://api.openai.com/v1/chat/completions",
                "OPENAI_API_KEY",
            ),
            "mistral" => (
                Mistral,
                "https://api.mistral.ai/v1/chat/completions",
                "MISTRAL_API_KEY",
            ),
            "openrouter" => (
                OpenRouter,
                "https://openrouter.ai/api/v1/chat/completions",
                "OPENROUTER_API_KEY",
            ),
            "aiio" => (
                Aiio,
                "https://api.intelligence.io.solutions/api/v1/chat/completions",
                "AIIO_API_KEY",
            ),
            "proxyapi" => (
                Compatible,
                "https://openai.api.proxyapi.ru/v1/chat/completions",
                "PROXYAPI_KEY",
            ),
            "groq" => (
                Compatible,
                "https://api.groq.com/openai/v1/chat/completions",
                "GROQ_API_KEY",
            ),
            "together" => (
                Compatible,
                "https://api.together.ai/v1/chat/completions",
                "TOGETHER_API_KEY",
            ),
            "chutes" => (
                Compatible,
                "https://llm.chutes.ai/v1/chat/completions",
                "CHUTES_API_KEY",
            ),
            "kodikrouter" => (
                Compatible,
                "https://api.kodikrouter.ru/v1/chat/completions",
                "KODIKROUTER_API_KEY",
            ),
            "lmstudio" => (
                LmStudio,
                "http://127.0.0.1:1234/v1/chat/completions",
                "LMSTUDIO_API_KEY",
            ),
            "ollama" => (
                Compatible,
                "http://127.0.0.1:11434/v1/chat/completions",
                "OLLAMA_API_KEY",
            ),
            "openai" => (
                Responses,
                "https://api.openai.com/v1/responses",
                "OPENAI_API_KEY",
            ),
            _ => {
                return Err(PresetCatalogError::Invalid(format!(
                    "unsupported provider template: {id}"
                )));
            }
        };
        Ok(Self {
            protocol: PresetProtocol::OpenAi(protocol),
            endpoint: endpoint.into(),
            credential: credential.into(),
            temperature: 0.7,
            max_output_tokens: 3000,
            timeout_ms: 60_000,
            send_temperature: true,
            send_max_output_tokens: true,
        })
    }

    pub fn for_provider(provider: super::ProviderKind) -> Self {
        match provider {
            super::ProviderKind::OpenAi => {
                Self::named("compatible").expect("built-in compatible template")
            }
            super::ProviderKind::Gemini => Self {
                protocol: PresetProtocol::Gemini,
                send_temperature: true,
                send_max_output_tokens: true,
                endpoint: GeminiTemplate::GOOGLE_ENDPOINT.into(),
                credential: "GEMINI_API_KEY".into(),
                temperature: 0.7,
                max_output_tokens: 3000,
                timeout_ms: 60_000,
            },
        }
    }

    pub fn configure(
        &self,
        name: String,
        model: String,
        credential: super::CredentialReference,
    ) -> Result<PresetRecord, PresetCatalogError> {
        if let PresetProtocol::OpenAi(protocol) = self.protocol {
            let record = PresetRecord {
                name,
                definition: PresetDefinition::OpenAi(super::openai::OpenAiPresetDefinition {
                    protocol,
                    endpoint: self.endpoint.clone(),
                    model,
                    credential: Some(credential),
                    temperature: self.temperature,
                    max_output_tokens: self.max_output_tokens,
                    timeout_ms: self.timeout_ms,
                    options: {
                        let mut options = protocol.options();
                        options.send_temperature = self.send_temperature;
                        options.send_max_output_tokens = self.send_max_output_tokens;
                        options
                    },
                    transforms: protocol.transforms(),
                }),
            };
            record.validate()?;
            return Ok(record);
        }
        let record = PresetRecord {
            name,
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                options: super::gemini::GeminiRequestOptions {
                    send_temperature: self.send_temperature,
                    send_max_output_tokens: self.send_max_output_tokens,
                    ..Default::default()
                },
                transforms: Vec::new(),
                endpoint: self.endpoint.clone(),
                model,
                credential,
                temperature: self.temperature,
                max_output_tokens: self.max_output_tokens,
                timeout_ms: self.timeout_ms,
            }),
        };
        record.validate()?;
        Ok(record)
    }
}

impl GeminiPresetDefinition {
    fn validate(&self) -> Result<(), PresetCatalogError> {
        self.credential.validate()?;
        self.template("validation-key").map(|_| ())
    }

    pub(super) fn template(&self, credential: &str) -> Result<GeminiTemplate, PresetCatalogError> {
        let settings = GenerationSettings::new(
            self.temperature,
            self.max_output_tokens,
            Duration::from_millis(self.timeout_ms),
        )
        .map_err(|err| PresetCatalogError::Invalid(err.to_string()))?;
        GeminiTemplate::new(&self.endpoint, &self.model, credential, settings)
            .and_then(|template| {
                template
                    .with_transforms(self.transforms.clone())
                    .with_options(self.options.clone())
            })
            .map_err(|err| PresetCatalogError::Invalid(err.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", content = "configuration", rename_all = "snake_case")]
pub enum PresetDefinition {
    Unconfigured,
    Gemini(GeminiPresetDefinition),
    OpenAi(super::openai::OpenAiPresetDefinition),
}

impl PresetDefinition {
    fn validate(&self) -> Result<(), PresetCatalogError> {
        match self {
            Self::Unconfigured => Ok(()),
            Self::Gemini(config) => config.validate(),
            Self::OpenAi(config) => super::openai::OpenAiTemplate::new(
                config.clone(),
                config.credential.as_ref().map(|_| "validation-key"),
            )
            .map(|_| ()),
        }
    }

    fn materialize(
        &self,
        credentials: &dyn PresetCredentials,
    ) -> Result<GenerationPreset, PresetCatalogError> {
        let template = match self {
            Self::Unconfigured => {
                return Err(PresetCatalogError::Invalid(
                    "preset is not configured".into(),
                ));
            }
            Self::OpenAi(config) => {
                let credential = config
                    .credential
                    .as_ref()
                    .map(|reference| credentials.resolve(reference))
                    .transpose()?;
                ProviderTemplate::OpenAi(super::openai::OpenAiTemplate::new(
                    config.clone(),
                    credential.as_deref(),
                )?)
            }
            Self::Gemini(config) => {
                let credential = credentials.resolve(&config.credential)?;
                ProviderTemplate::Gemini(config.template(&credential)?)
            }
        };
        Ok(GenerationPreset::new(template))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRecord {
    pub name: String,
    pub definition: PresetDefinition,
}

/// Connection/settings fields editable independently of the provider pipeline.
pub struct PresetGenerationEdit {
    pub protocol: PresetProtocol,
    pub send_temperature: bool,
    pub send_max_output_tokens: bool,
    pub endpoint: String,
    pub model: String,
    pub credential: Option<super::CredentialReference>,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

pub struct PresetConnectionEdit {
    pub key: Option<super::SecretKey>,
    pub name: String,
    pub connection: Option<PresetGenerationEdit>,
}

impl PresetRecord {
    pub(super) fn validate(&self) -> Result<(), PresetCatalogError> {
        if self.name.trim().is_empty() {
            return Err(PresetCatalogError::Invalid(
                "preset name must not be empty".into(),
            ));
        }
        self.definition.validate()
    }
}

pub trait PresetCredentials: Send + Sync {
    fn store(
        &self,
        _key: &super::SecretKey,
    ) -> Result<super::CredentialReference, PresetCatalogError> {
        Err(PresetCatalogError::Storage(
            "credential store is read-only".into(),
        ))
    }
    fn get(&self, key: &str) -> Result<String, PresetCatalogError>;
    fn resolve(
        &self,
        reference: &super::CredentialReference,
    ) -> Result<String, PresetCatalogError> {
        reference.validate()?;
        match reference {
            super::CredentialReference::Environment(name) => self.get(name),
            super::CredentialReference::Local(_) => {
                Err(PresetCatalogError::MissingCredential(reference.to_string()))
            }
        }
    }
}

pub struct EnvironmentPresetCredentials;

impl PresetCredentials for EnvironmentPresetCredentials {
    fn get(&self, key: &str) -> Result<String, PresetCatalogError> {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PresetCatalogError::MissingCredential(key.into()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPresets {
    schema_version: u32,
    next_id: u32,
    entries: BTreeMap<PresetId, PresetRecord>,
}

impl StoredPresets {
    pub(super) fn from_records(
        entries: BTreeMap<PresetId, PresetRecord>,
    ) -> Result<Self, PresetCatalogError> {
        let next_id = entries.keys().next_back().map_or(Ok(0), |id| {
            id.0.checked_add(1).ok_or(PresetCatalogError::IdExhausted)
        })?;
        let stored = Self {
            schema_version: 1,
            next_id,
            entries,
        };
        stored.validate()?;
        Ok(stored)
    }

    fn empty(next_id: u32) -> Self {
        Self {
            schema_version: 1,
            next_id,
            entries: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<(), PresetCatalogError> {
        if self.schema_version != 1 {
            return Err(PresetCatalogError::Storage(
                "unsupported preset catalog schema".into(),
            ));
        }
        if self.entries.keys().any(|id| id.0 >= self.next_id) {
            return Err(PresetCatalogError::Storage(
                "invalid preset identifier sequence".into(),
            ));
        }
        for value in self.entries.values() {
            value.validate()?;
        }
        Ok(())
    }
}

pub trait PresetRepository: Send + Sync {
    fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError>;
    fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError>;
}

pub struct JsonPresetRepository {
    path: PathBuf,
}

impl JsonPresetRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl PresetRepository for JsonPresetRepository {
    fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PresetCatalogError::Storage(error.to_string())),
        };
        let stored: StoredPresets = serde_json::from_slice(&bytes)
            .map_err(|error| PresetCatalogError::Storage(error.to_string()))?;
        stored.validate()?;
        Ok(Some(stored))
    }

    fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| PresetCatalogError::Storage(error.to_string()))?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), std::io::Error> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| PresetCatalogError::Storage(error.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct PresetCatalogSnapshot {
    pub revision: u64,
    pub entries: BTreeMap<PresetId, PresetRecord>,
}

struct CatalogState {
    runtime: BTreeMap<PresetId, GenerationPreset>,
    stored: StoredPresets,
    revision: u64,
}

pub struct PresetCatalog {
    state: RwLock<CatalogState>,
    repository: Option<Arc<dyn PresetRepository>>,
    credentials: Arc<dyn PresetCredentials>,
    changes: watch::Sender<Arc<PresetCatalogSnapshot>>,
}

impl fmt::Debug for PresetCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresetCatalog").finish_non_exhaustive()
    }
}

impl PresetCatalog {
    pub fn new(entries: BTreeMap<PresetId, GenerationPreset>) -> Self {
        let next_id = entries
            .keys()
            .next_back()
            .and_then(|id| id.0.checked_add(1))
            .unwrap_or(1);
        let stored = StoredPresets::empty(next_id);
        let (changes, _) = watch::channel(Arc::new(PresetCatalogSnapshot {
            revision: 0,
            entries: stored.entries.clone(),
        }));
        Self {
            state: RwLock::new(CatalogState {
                runtime: entries,
                stored,
                revision: 0,
            }),
            repository: None,
            credentials: Arc::new(EnvironmentPresetCredentials),
            changes,
        }
    }

    pub fn single(preset: GenerationPreset) -> Self {
        Self::new(BTreeMap::from([(PresetId(0), preset)]))
    }

    pub fn open(
        runtime: BTreeMap<PresetId, GenerationPreset>,
        repository: Arc<dyn PresetRepository>,
        credentials: Arc<dyn PresetCredentials>,
    ) -> Result<Self, PresetCatalogError> {
        let min_next = runtime
            .keys()
            .next_back()
            .map_or(0, |id| id.0.saturating_add(1));
        let stored = repository
            .load()?
            .unwrap_or_else(|| StoredPresets::empty(min_next));
        stored.validate()?;
        if stored.entries.keys().any(|id| runtime.contains_key(id)) {
            return Err(PresetCatalogError::Storage(
                "persisted ID conflicts with runtime preset".into(),
            ));
        }
        if stored.next_id < min_next {
            return Err(PresetCatalogError::Storage(
                "persisted catalog overlaps reserved runtime IDs".into(),
            ));
        }
        let (changes, _) = watch::channel(Arc::new(PresetCatalogSnapshot {
            revision: 0,
            entries: stored.entries.clone(),
        }));
        Ok(Self {
            state: RwLock::new(CatalogState {
                runtime,
                stored,
                revision: 0,
            }),
            repository: Some(repository),
            credentials,
            changes,
        })
    }

    pub fn get(&self, id: PresetId) -> Result<Option<GenerationPreset>, PresetCatalogError> {
        let state = self.state.read().expect("preset catalog poisoned");
        if let Some(preset) = state.runtime.get(&id) {
            return Ok(Some(preset.clone()));
        }
        let record = state.stored.entries.get(&id).cloned();
        drop(state);
        record
            .map(|entry| entry.definition.materialize(self.credentials.as_ref()))
            .transpose()
    }

    pub fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.changes.subscribe()
    }

    pub fn list(&self) -> Vec<(PresetId, PresetRecord)> {
        self.state
            .read()
            .expect("preset catalog poisoned")
            .stored
            .entries
            .iter()
            .map(|(id, value)| (*id, value.clone()))
            .collect()
    }

    pub fn configured(&self, id: PresetId) -> Result<bool, PresetCatalogError> {
        let state = self.state.read().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Ok(true);
        }
        match state.stored.entries.get(&id) {
            Some(record) => Ok(!matches!(record.definition, PresetDefinition::Unconfigured)),
            None => Err(PresetCatalogError::NotFound(id)),
        }
    }

    pub fn editor_key(&self, id: PresetId) -> Result<Option<super::SecretKey>, PresetCatalogError> {
        let state = self.state.read().expect("preset catalog poisoned");
        let record = state
            .stored
            .entries
            .get(&id)
            .ok_or(PresetCatalogError::NotFound(id))?;
        let reference = match &record.definition {
            PresetDefinition::Gemini(config) => Some(&config.credential),
            PresetDefinition::OpenAi(config) => config.credential.as_ref(),
            PresetDefinition::Unconfigured => None,
        };
        reference
            .map(|reference| {
                self.credentials
                    .resolve(reference)
                    .map(super::SecretKey::new)
            })
            .transpose()
    }

    pub fn contains(&self, id: PresetId) -> bool {
        let state = self.state.read().expect("preset catalog poisoned");
        state.runtime.contains_key(&id) || state.stored.entries.contains_key(&id)
    }

    pub fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        record.validate()?;
        let mut state = self.state.write().expect("preset catalog poisoned");
        let id = PresetId(state.stored.next_id);
        let next_id = id.0.checked_add(1).ok_or(PresetCatalogError::IdExhausted)?;
        let mut next = state.stored.clone();
        next.next_id = next_id;
        next.entries.insert(id, record);
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(id)
    }

    pub fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        record.validate()?;
        let mut state = self.state.write().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Err(PresetCatalogError::RuntimeOnly(id));
        }
        if !state.stored.entries.contains_key(&id) {
            return Err(PresetCatalogError::NotFound(id));
        }
        let mut next = state.stored.clone();
        next.entries.insert(id, record);
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(())
    }

    pub fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        let mut state = self.state.write().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Err(PresetCatalogError::RuntimeOnly(id));
        }
        let mut next = state.stored.clone();
        if next.entries.remove(&id).is_none() {
            return Err(PresetCatalogError::NotFound(id));
        }
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(())
    }

    pub fn update_connection(
        &self,
        id: PresetId,
        edit: PresetConnectionEdit,
    ) -> Result<(), PresetCatalogError> {
        let mut state = self.state.write().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Err(PresetCatalogError::RuntimeOnly(id));
        }
        let current = state
            .stored
            .entries
            .get(&id)
            .ok_or(PresetCatalogError::NotFound(id))?;
        let edit_name = edit.name;
        let definition = match edit.connection {
            None => PresetDefinition::Unconfigured,
            Some(mut connection) => {
                if let Some(key) = edit.key {
                    connection.credential = if key.expose().is_empty() {
                        None
                    } else {
                        let previous = match &current.definition {
                            PresetDefinition::Gemini(config) => Some(&config.credential),
                            PresetDefinition::OpenAi(config) => config.credential.as_ref(),
                            PresetDefinition::Unconfigured => None,
                        };
                        if let Some(reference) = previous.filter(|r| {
                            self.credentials
                                .resolve(r)
                                .is_ok_and(|value| value == key.expose())
                        }) {
                            Some(reference.clone())
                        } else {
                            Some(self.credentials.store(&key)?)
                        }
                    };
                }
                let edit = connection;
                if let PresetProtocol::OpenAi(protocol) = edit.protocol {
                    let (transforms, mut options) = match &current.definition {
                        PresetDefinition::OpenAi(config) if config.protocol == protocol => {
                            (config.transforms.clone(), config.options.clone())
                        }
                        _ => (protocol.transforms(), protocol.options()),
                    };
                    options.send_temperature = edit.send_temperature;
                    options.send_max_output_tokens = edit.send_max_output_tokens;
                    let definition =
                        PresetDefinition::OpenAi(super::openai::OpenAiPresetDefinition {
                            protocol,
                            endpoint: edit.endpoint,
                            model: edit.model,
                            credential: edit.credential,
                            temperature: edit.temperature,
                            max_output_tokens: edit.max_output_tokens,
                            timeout_ms: edit.timeout_ms,
                            options,
                            transforms,
                        });
                    let record = PresetRecord {
                        name: edit_name,
                        definition,
                    };
                    record.validate()?;
                    let mut next = state.stored.clone();
                    next.entries.insert(id, record);
                    self.persist(&next)?;
                    self.publish(&mut state, next);
                    return Ok(());
                }

                let (transforms, mut options) = match &current.definition {
                    PresetDefinition::Gemini(config) => {
                        (config.transforms.clone(), config.options.clone())
                    }
                    PresetDefinition::OpenAi(_) | PresetDefinition::Unconfigured => {
                        (Vec::new(), super::gemini::GeminiRequestOptions::default())
                    }
                };
                options.send_temperature = edit.send_temperature;
                options.send_max_output_tokens = edit.send_max_output_tokens;
                PresetDefinition::Gemini(GeminiPresetDefinition {
                    endpoint: edit.endpoint,
                    model: edit.model,
                    credential: edit.credential.ok_or_else(|| {
                        PresetCatalogError::Invalid("Gemini requires a credential reference".into())
                    })?,
                    temperature: edit.temperature,
                    max_output_tokens: edit.max_output_tokens,
                    timeout_ms: edit.timeout_ms,
                    transforms,
                    options,
                })
            }
        };
        let record = PresetRecord {
            name: edit_name,
            definition,
        };
        record.validate()?;
        let mut next = state.stored.clone();
        next.entries.insert(id, record);
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(())
    }

    fn publish(&self, state: &mut CatalogState, stored: StoredPresets) {
        state.revision += 1;
        state.stored = stored;
        self.changes.send_replace(Arc::new(PresetCatalogSnapshot {
            revision: state.revision,
            entries: state.stored.entries.clone(),
        }));
    }

    fn persist(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
        if let Some(repo) = &self.repository {
            repo.save(value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestCredentials(Mutex<BTreeMap<String, String>>);
    impl PresetCredentials for TestCredentials {
        fn get(&self, key: &str) -> Result<String, PresetCatalogError> {
            self.0
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| PresetCatalogError::MissingCredential(key.into()))
        }
    }

    fn example(model: &str) -> PresetRecord {
        PresetRecord {
            name: model.into(),
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                options: Default::default(),
                transforms: Vec::new(),
                endpoint: "http://127.0.0.1:8888/v1beta/".into(),
                model: model.into(),
                credential: "MY_TEST_API_KEY".into(),
                temperature: 0.7,
                max_output_tokens: 200,
                timeout_ms: 4000,
            }),
        }
    }

    #[test]
    fn editor_reads_and_replaces_real_key_without_persisting_it_in_the_catalog() {
        let directory = std::env::temp_dir().join(format!("key-editor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let credentials = Arc::new(super::super::ApplicationPresetCredentials::new(
            directory.join("keys.json"),
        ));
        let reference = credentials
            .store(&super::super::SecretKey::new("original-fixture-key"))
            .unwrap();
        let repository = Arc::new(JsonPresetRepository::new(directory.join("presets.json")));
        let catalog =
            PresetCatalog::open(Default::default(), repository.clone(), credentials.clone())
                .unwrap();
        let mut record = example("model");
        let PresetDefinition::Gemini(config) = &mut record.definition else {
            panic!()
        };
        config.credential = reference.clone();
        let id = catalog.create(record).unwrap();
        assert_eq!(
            catalog.editor_key(id).unwrap().unwrap().expose(),
            "original-fixture-key"
        );
        catalog
            .update_connection(
                id,
                PresetConnectionEdit {
                    key: Some(super::super::SecretKey::new("replacement-fixture-key")),
                    name: "edited".into(),
                    connection: Some(PresetGenerationEdit {
                        protocol: PresetProtocol::Gemini,
                        send_temperature: true,
                        send_max_output_tokens: true,
                        endpoint: "http://127.0.0.1:8888/v1beta/".into(),
                        model: "model".into(),
                        credential: None,
                        temperature: 0.7,
                        max_output_tokens: 128,
                        timeout_ms: 1000,
                    }),
                },
            )
            .unwrap();
        let reopened =
            PresetCatalog::open(Default::default(), repository, credentials.clone()).unwrap();
        let key = reopened.editor_key(id).unwrap().unwrap();
        assert_eq!(key.expose(), "replacement-fixture-key");
        assert!(!format!("{key:?}").contains("replacement-fixture-key"));
        let json = std::fs::read_to_string(directory.join("presets.json")).unwrap();
        assert!(
            !json.contains("original-fixture-key") && !json.contains("replacement-fixture-key")
        );
        assert_eq!(
            credentials.resolve(&reference).unwrap(),
            "original-fixture-key"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn connection_edits_preserve_the_existing_provider_pipeline() {
        let catalog = PresetCatalog::new(BTreeMap::new());
        let mut record = example("before");
        let PresetDefinition::Gemini(config) = &mut record.definition else {
            panic!()
        };
        config.transforms = vec![super::super::MessageTransform::EnsureAlternatingRoles];
        config.options.send_temperature = false;
        let id = catalog.create(record).unwrap();
        catalog
            .update_connection(
                id,
                PresetConnectionEdit {
                    key: None,
                    name: "renamed".into(),
                    connection: Some(PresetGenerationEdit {
                        protocol: PresetProtocol::Gemini,
                        send_temperature: false,
                        send_max_output_tokens: true,
                        endpoint: "http://127.0.0.1:9999/v1beta/".into(),
                        model: "after".into(),
                        credential: Some("OTHER_KEY".into()),
                        temperature: 1.0,
                        max_output_tokens: 3000,
                        timeout_ms: 1000,
                    }),
                },
            )
            .unwrap();
        let record = catalog
            .list()
            .into_iter()
            .find(|(key, _)| *key == id)
            .unwrap()
            .1;
        let PresetDefinition::Gemini(config) = record.definition else {
            panic!()
        };
        assert_eq!(config.model, "after");
        assert!(!config.options.send_temperature);
        assert_eq!(
            config.transforms,
            [super::super::MessageTransform::EnsureAlternatingRoles]
        );
    }

    #[test]
    fn catalog_crud_reopen_and_secret_free_storage() {
        let path =
            std::env::temp_dir().join(format!("neuromita-presets-{}.json", uuid::Uuid::new_v4()));
        let credentials = Arc::new(TestCredentials(Mutex::new(BTreeMap::from([(
            "MY_TEST_API_KEY".into(),
            "private-test-key".into(),
        )]))));
        let store: Arc<dyn PresetRepository> = Arc::new(JsonPresetRepository::new(path.clone()));
        let catalog =
            PresetCatalog::open(BTreeMap::new(), Arc::clone(&store), credentials.clone()).unwrap();
        let id = catalog.create(example("alpha")).unwrap();
        assert_eq!(id, PresetId(0));
        assert_eq!(catalog.list().len(), 1);
        assert!(format!("{:?}", catalog.get(id).unwrap().unwrap()).contains("alpha"));
        let prepared = catalog.get(id).unwrap().unwrap();
        catalog.update(id, example("beta")).unwrap();
        assert!(format!("{prepared:?}").contains("alpha"));
        assert!(format!("{:?}", catalog.get(id).unwrap().unwrap()).contains("beta"));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("private-test-key"));
        assert!(saved.contains("MY_TEST_API_KEY"));
        let reopened = PresetCatalog::open(BTreeMap::new(), store, credentials).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert!(format!("{:?}", reopened.get(id).unwrap().unwrap()).contains("beta"));
        reopened.delete(id).unwrap();
        assert!(reopened.get(id).unwrap().is_none());
        let next_id = reopened.create(example("gamma")).unwrap();
        assert_ne!(next_id, id);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_credentials_and_invalid_configs_are_explicit() {
        let credentials = Arc::new(TestCredentials(Mutex::new(BTreeMap::new())));
        let store: Arc<dyn PresetRepository> = Arc::new(MemoryPresetRepository::default());
        let catalog = PresetCatalog::open(BTreeMap::new(), store, credentials).unwrap();
        let id = catalog.create(example("model")).unwrap();
        assert!(matches!(
            catalog.get(id),
            Err(PresetCatalogError::MissingCredential(_))
        ));
        let mut invalid = example("bad/model");
        assert!(catalog.create(invalid.clone()).is_err());
        let PresetDefinition::Gemini(config) = &mut invalid.definition else {
            unreachable!()
        };
        config.model = "good".into();
        config.credential = "BAD-ENV".into();
        assert!(catalog.update(id, invalid).is_err());
        assert_eq!(catalog.list().len(), 1);
    }

    #[test]
    fn failed_persistence_does_not_publish_a_preset_or_notify_observers() {
        let directory = std::env::temp_dir().join(format!(
            "neuromita-no-preset-directory-{}",
            uuid::Uuid::new_v4()
        ));
        let catalog = PresetCatalog::open(
            BTreeMap::new(),
            Arc::new(JsonPresetRepository::new(directory.join("presets.json"))),
            Arc::new(TestCredentials(Mutex::new(BTreeMap::new()))),
        )
        .unwrap();
        let observer = catalog.observe();
        assert!(matches!(
            catalog.create(example("alpha")),
            Err(PresetCatalogError::Storage(_))
        ));
        assert!(catalog.list().is_empty());
        assert_eq!(observer.borrow().revision, 0);
        assert!(!observer.has_changed().unwrap());
    }

    #[derive(Default)]
    struct MemoryPresetRepository(Mutex<Option<StoredPresets>>);
    impl PresetRepository for MemoryPresetRepository {
        fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
            *self.0.lock().unwrap() = Some(value.clone());
            Ok(())
        }
    }
}
