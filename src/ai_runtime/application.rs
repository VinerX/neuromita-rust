//! Explicit application-owned activation of selected Python AI workers.

use std::{collections::HashSet, sync::Arc};

use tokio::{process::Command, time::timeout};

use thiserror::Error;

use crate::{
    adapters::{python_embedding::PythonEmbeddingProvider, python_reranker::PythonReranker},
    knowledge::embedding::EmbeddingSpace,
    settings::SpeechSettings,
    voice::VoiceProvider,
};

use super::{
    AiRuntimeError, AiService, AiSupervisor, ModelActivation, ProcessTopology, PythonHostConfig,
    PythonVoiceProvider,
    composition::RuntimeComposition,
    speech_catalog::SpeechInventory,
    speech_selection::{
        SpeechSelection, SpeechSelectionError, inspect_speech_selection, select_speech_model,
    },
    voice_profile::VoiceProfileStore,
};

#[derive(Clone)]
pub struct AiWorkerSelection {
    pub service: AiService,
    pub composition: RuntimeComposition,
    pub models: Vec<ModelActivation>,
}

pub struct AiApplicationConfig {
    pub topology: ProcessTopology,
    pub host: PythonHostConfig,
    pub workers: Vec<AiWorkerSelection>,
    pub voice_profiles: Option<Arc<VoiceProfileStore>>,
}

#[derive(Debug, Error)]
pub enum AiApplicationError {
    #[error("invalid AI application selection: {0}")]
    Invalid(String),
    #[error("Python interpreter check failed: {0}")]
    Interpreter(String),
    #[error(transparent)]
    Runtime(#[from] AiRuntimeError),
}

#[derive(Debug, Error)]
pub enum AiSpeechBindingError {
    #[error(transparent)]
    Application(#[from] AiApplicationError),
    #[error(transparent)]
    Selection(#[from] SpeechSelectionError),
    #[error("{0:?} speech selection does not match the prepared worker plan")]
    Unprepared(AiService),
}

pub struct RunningAiApplication {
    supervisor: Arc<AiSupervisor>,
    active: Vec<AiService>,
    selected: HashSet<AiService>,
    worker_services: std::collections::HashMap<AiService, Vec<AiService>>,
    voice_profiles: Option<Arc<VoiceProfileStore>>,
}

fn matches_prepared_selection(
    selected: Option<&SpeechSelection>,
    prepared: Option<&ModelActivation>,
) -> bool {
    match (selected, prepared) {
        (None, None) => true,
        (Some(selected), Some(prepared)) => {
            selected.service == prepared.service
                && selected.plugin_id == prepared.plugin_id
                && selected.asset_revision == prepared.asset_revision
        }
        _ => false,
    }
}

impl AiApplicationConfig {
    /// Bind saved TTS/ASR settings to the already prepared runtime composition.
    /// A plugin or asset revision switch requires a newly prepared composition;
    /// this method never installs a model or starts inference.
    pub async fn with_speech_settings(
        mut self,
        settings: &SpeechSettings,
        inventory: &SpeechInventory,
    ) -> Result<Self, AiSpeechBindingError> {
        self.validate()?;
        let mut resolved = Vec::new();
        for service in [AiService::Tts, AiService::Asr] {
            let selection = select_speech_model(settings, inventory, service)?;
            let prepared = self
                .workers
                .iter()
                .enumerate()
                .find_map(|(worker_index, worker)| {
                    worker
                        .models
                        .iter()
                        .enumerate()
                        .find(|(_, model)| model.service == service)
                        .map(|(model_index, model)| (worker_index, model_index, model))
                });
            if !matches_prepared_selection(selection.as_ref(), prepared.map(|(_, _, model)| model))
            {
                return Err(AiSpeechBindingError::Unprepared(service));
            }
            match (selection, prepared) {
                (None, None) => (),
                (Some(selection), Some((worker_index, model_index, _))) => {
                    let inspected = inspect_speech_selection(
                        selection,
                        &self.host,
                        self.workers[worker_index].composition.paths(),
                    )
                    .await?;
                    resolved.push((worker_index, model_index, inspected.activation.settings));
                }
                _ => return Err(AiSpeechBindingError::Unprepared(service)),
            }
        }
        for (worker_index, model_index, settings) in resolved {
            self.workers[worker_index].models[model_index].settings = settings;
        }
        Ok(self)
    }

    fn validate(&self) -> Result<(), AiApplicationError> {
        if self.workers.is_empty() {
            return Err(AiApplicationError::Invalid("no AI workers selected".into()));
        }
        let mut selected = HashSet::new();
        for worker in &self.workers {
            if !worker.composition.services().contains(&worker.service) {
                return Err(AiApplicationError::Invalid(
                    "worker service absent from composition".into(),
                ));
            }
            let models: HashSet<_> = worker.models.iter().map(|model| model.service).collect();
            let services: HashSet<_> = worker.composition.services().iter().copied().collect();
            if models != services || models.len() != worker.models.len() {
                return Err(AiApplicationError::Invalid(
                    "composition must select exactly one model per service".into(),
                ));
            }
            for service in services {
                if !selected.insert(service) {
                    return Err(AiApplicationError::Invalid(
                        "a service is selected by multiple workers".into(),
                    ));
                }
            }
            if self.topology == ProcessTopology::Split
                && worker.composition.services() != [worker.service]
            {
                return Err(AiApplicationError::Invalid(
                    "split workers must contain exactly one service".into(),
                ));
            }
        }
        if self.topology == ProcessTopology::Shared && self.workers.len() != 1 {
            return Err(AiApplicationError::Invalid(
                "shared topology requires one composed worker".into(),
            ));
        }
        Ok(())
    }

    pub async fn activate(self) -> Result<RunningAiApplication, AiApplicationError> {
        self.activate_selected(None).await
    }

    pub async fn activate_with_hardware(
        self,
        config: &super::hardware::HardwareConfig,
        store: &super::hardware::HardwareConfigStore,
    ) -> Result<RunningAiApplication, AiApplicationError> {
        config
            .validate()
            .map_err(|error| AiApplicationError::Invalid(error.to_string()))?;
        self.activate_selected(Some((config, store))).await
    }

    async fn activate_selected(
        self,
        hardware: Option<(
            &super::hardware::HardwareConfig,
            &super::hardware::HardwareConfigStore,
        )>,
    ) -> Result<RunningAiApplication, AiApplicationError> {
        self.validate()?;
        let output = timeout(
            self.host.request_timeout,
            Command::new(&self.host.python_executable)
                .arg("--version")
                .output(),
        )
        .await
        .map_err(|_| AiApplicationError::Interpreter("version probe timed out".into()))?
        .map_err(|error| AiApplicationError::Interpreter(error.to_string()))?;
        let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let reported = if reported.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            reported
        };
        if !output.status.success()
            || !self
                .workers
                .iter()
                .all(|worker| reported == format!("Python {}", worker.composition.python_version()))
        {
            return Err(AiApplicationError::Interpreter(format!(
                "interpreter reports {reported:?}, which does not match selected locked Python"
            )));
        }
        let selected: HashSet<_> = self
            .workers
            .iter()
            .flat_map(|worker| worker.models.iter().map(|model| model.service))
            .collect();
        let supervisor = Arc::new(AiSupervisor::new(self.topology, self.host));
        let worker_services = self
            .workers
            .iter()
            .flat_map(|worker| {
                worker
                    .composition
                    .services()
                    .iter()
                    .map(|service| (*service, worker.composition.services().to_vec()))
            })
            .collect();
        let mut active = Vec::new();
        if let Some((config, _)) = hardware {
            let worker = self
                .workers
                .iter()
                .find(|worker| {
                    worker
                        .models
                        .iter()
                        .any(|model| model.service == config.selection.service)
                })
                .ok_or_else(|| {
                    AiApplicationError::Invalid(
                        "hardware service absent from application plan".into(),
                    )
                })?;
            super::hardware::verify_worker_plan(worker, config)
                .map_err(|error| AiApplicationError::Invalid(error.to_string()))?;
        }
        for worker in self.workers {
            let activation = if let Some((config, store)) = hardware
                && worker
                    .models
                    .iter()
                    .any(|model| model.service == config.selection.service)
            {
                supervisor
                    .activate_hardware(
                        config.selection.service,
                        &worker.composition,
                        &worker.models,
                        &config.selection.profile(),
                        &|| store.verify_current(config),
                    )
                    .await
            } else {
                supervisor
                    .activate_composed(worker.service, &worker.composition, &worker.models)
                    .await
            };
            if let Err(error) = activation {
                for service in active.into_iter().rev() {
                    let _ = supervisor.stop(service).await;
                }
                return Err(error.into());
            }
            active.push(worker.service);
        }
        let _selection_guard = if let Some((config, store)) = hardware {
            match store.verify_current(config) {
                Ok(guard) => Some(guard),
                Err(error) => {
                    for service in active.into_iter().rev() {
                        let _ = supervisor.stop(service).await;
                    }
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        Ok(RunningAiApplication {
            supervisor,
            active,
            selected,
            worker_services,
            voice_profiles: self.voice_profiles,
        })
    }
}

impl RunningAiApplication {
    pub fn observe_status(
        &self,
    ) -> tokio::sync::watch::Receiver<super::runtime_status::RuntimeSnapshot> {
        self.supervisor.observe_status()
    }

    pub fn status_snapshot(&self) -> super::runtime_status::RuntimeSnapshot {
        self.supervisor.status_snapshot()
    }

    pub async fn refresh_status(&self) -> super::runtime_status::RuntimeSnapshot {
        self.supervisor.refresh_status().await
    }

    pub(crate) fn worker_services(&self, service: AiService) -> Option<&[AiService]> {
        self.worker_services.get(&service).map(Vec::as_slice)
    }
    pub(crate) fn supervisor(&self) -> &AiSupervisor {
        &self.supervisor
    }
    pub(crate) fn supervisor_weak(&self) -> std::sync::Weak<AiSupervisor> {
        Arc::downgrade(&self.supervisor)
    }
    pub fn has_service(&self, service: AiService) -> bool {
        self.selected.contains(&service)
    }

    pub fn speech_recognizer(&self) -> Option<super::speech::PythonSpeechRecognizer> {
        self.has_service(AiService::Asr)
            .then(|| super::speech::PythonSpeechRecognizer::new(Arc::clone(&self.supervisor)))
    }

    pub fn embedding_provider(&self, space: EmbeddingSpace) -> Option<PythonEmbeddingProvider> {
        self.has_service(AiService::Embedding)
            .then(|| PythonEmbeddingProvider::new(Arc::clone(&self.supervisor), space))
    }

    pub fn reranker(
        &self,
        model_id: String,
        protocol: String,
    ) -> Option<Result<PythonReranker, crate::adapters::python_reranker::RerankError>> {
        self.has_service(AiService::Reranker)
            .then(|| PythonReranker::new(Arc::clone(&self.supervisor), model_id, protocol))
    }

    pub fn voice_provider(&self) -> Arc<dyn VoiceProvider> {
        let mut provider = PythonVoiceProvider::new(Arc::clone(&self.supervisor));
        if let Some(profiles) = &self.voice_profiles {
            provider = provider.with_profiles(Arc::clone(profiles));
        }
        Arc::new(provider)
    }

    pub async fn shutdown(self) -> Result<(), AiApplicationError> {
        let mut first_error = None;
        for service in self.active.into_iter().rev() {
            if let Err(error) = self.supervisor.stop(service).await {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod speech_binding_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepared_worker_must_match_exact_plugin_and_verified_revision() {
        let selected = SpeechSelection {
            service: AiService::Asr,
            plugin_id: "whisper".into(),
            asset_revision: Some("rev2".into()),
            language_hint: Some("en".into()),
            overrides: json!({"beam_size": 5}),
        };
        let prepared = ModelActivation {
            service: AiService::Asr,
            plugin_id: "whisper".into(),
            settings: json!({}),
            asset_revision: Some("rev2".into()),
        };
        assert!(matches_prepared_selection(Some(&selected), Some(&prepared)));
        assert!(matches_prepared_selection(None, None));
        assert!(!matches_prepared_selection(Some(&selected), None));
        assert!(!matches_prepared_selection(None, Some(&prepared)));
        let mismatched = ModelActivation {
            asset_revision: Some("rev1".into()),
            ..prepared.clone()
        };
        assert!(!matches_prepared_selection(
            Some(&selected),
            Some(&mismatched)
        ));
        let mismatched = ModelActivation {
            plugin_id: "gigaam".into(),
            ..prepared
        };
        assert!(!matches_prepared_selection(
            Some(&selected),
            Some(&mismatched)
        ));
    }
}
