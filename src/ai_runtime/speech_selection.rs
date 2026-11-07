use std::path::PathBuf;

use serde_json::Value;
use thiserror::Error;

use crate::settings::{SpeechSettings, TtsBackend};

use super::{
    AiRuntimeError, AiService, ModelActivation, PythonHostConfig, fetch_speech_plugin_inspection,
    speech_catalog::{SpeechInventory, SpeechModelOption},
    speech_inspection::SpeechPluginInspection,
};

#[derive(Debug, Error)]
pub enum SpeechSelectionError {
    #[error("unsupported speech service")]
    UnsupportedService,
    #[error("{0:?} has no selected speech plugin")]
    Unconfigured(AiService),
    #[error("selected {service:?} plugin {plugin_id} is absent from the inventory")]
    UnknownPlugin {
        service: AiService,
        plugin_id: String,
    },
    #[error("selected plugin {0} has multiple verified revisions; select one explicitly")]
    AmbiguousRevision(String),
    #[error("selected plugin {plugin_id} is not ready: {reasons:?}")]
    NotReady {
        plugin_id: String,
        reasons: Vec<String>,
    },
    #[error(transparent)]
    Inspection(#[from] AiRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSelection {
    pub service: AiService,
    pub plugin_id: String,
    pub asset_revision: Option<String>,
    pub language_hint: Option<String>,
    pub overrides: Value,
}

pub struct InspectedSpeechActivation {
    pub activation: ModelActivation,
    pub inspection: SpeechPluginInspection,
}

fn model_for<'a>(
    catalog: &'a SpeechInventory,
    service: AiService,
    id: &str,
) -> Result<&'a SpeechModelOption, SpeechSelectionError> {
    let items = match service {
        AiService::Tts => &catalog.tts,
        AiService::Asr => &catalog.asr,
        _ => return Err(SpeechSelectionError::UnsupportedService),
    };
    items
        .iter()
        .find(|option| option.id == id)
        .ok_or_else(|| SpeechSelectionError::UnknownPlugin {
            service,
            plugin_id: id.into(),
        })
}

pub fn select_speech_model(
    settings: &SpeechSettings,
    inventory: &SpeechInventory,
    service: AiService,
) -> Result<Option<SpeechSelection>, SpeechSelectionError> {
    let (plugin_id, language_hint) = match service {
        AiService::Tts => {
            if !settings.tts.enabled || settings.tts.backend != TtsBackend::Local {
                return Ok(None);
            }
            (
                &settings.tts.local_model_id,
                Some(settings.tts.local_language.as_str()),
            )
        }
        AiService::Asr => (
            &settings.asr.recognizer_id,
            settings
                .asr
                .model_languages
                .get(&settings.asr.recognizer_id)
                .map(String::as_str),
        ),
        _ => return Err(SpeechSelectionError::UnsupportedService),
    };
    if plugin_id.is_empty() {
        return if service == AiService::Tts {
            Err(SpeechSelectionError::Unconfigured(service))
        } else {
            Ok(None)
        };
    }
    let option = model_for(inventory, service, plugin_id)?;
    let overrides = match service {
        AiService::Tts => settings.tts_model_options.get(plugin_id),
        AiService::Asr => settings.asr_model_options.get(plugin_id),
        _ => None,
    }
    .cloned()
    .unwrap_or_else(|| serde_json::json!({}));
    let asset_revision = match option.installed_revisions.as_slice() {
        [] => None,
        [revision] => Some(revision.clone()),
        _ => return Err(SpeechSelectionError::AmbiguousRevision(plugin_id.clone())),
    };
    Ok(Some(SpeechSelection {
        service,
        plugin_id: plugin_id.clone(),
        asset_revision,
        language_hint: language_hint.filter(|s| !s.is_empty()).map(str::to_owned),
        overrides,
    }))
}

pub async fn inspect_speech_selection(
    selected: SpeechSelection,
    config: &PythonHostConfig,
    dependency_layers: &[PathBuf],
) -> Result<InspectedSpeechActivation, SpeechSelectionError> {
    let inspection = fetch_speech_plugin_inspection(
        config,
        selected.service,
        &selected.plugin_id,
        selected.overrides.clone(),
        selected.language_hint.as_deref(),
        selected.asset_revision.as_deref(),
        dependency_layers,
    )
    .await?;
    let mut reasons = inspection.install_probe.missing_reasons();
    if !inspection.install_probe.installed && reasons.is_empty() {
        reasons.push("plugin installation probe returned not installed".into());
    }
    if inspection.requires_assets() && selected.asset_revision.is_none() {
        reasons.push("required assets have no verified model revision".into());
    }
    if !reasons.is_empty() {
        return Err(SpeechSelectionError::NotReady {
            plugin_id: selected.plugin_id,
            reasons,
        });
    }
    let activation = ModelActivation {
        service: selected.service,
        plugin_id: selected.plugin_id,
        settings: inspection.effective_settings.clone(),
        asset_revision: selected.asset_revision,
    };
    Ok(InspectedSpeechActivation {
        activation,
        inspection,
    })
}

pub async fn resolve_speech_activations(
    settings: &SpeechSettings,
    inventory: &SpeechInventory,
    config: &PythonHostConfig,
    tts_layers: &[PathBuf],
    asr_layers: &[PathBuf],
) -> Result<Vec<InspectedSpeechActivation>, SpeechSelectionError> {
    let mut resolved = Vec::new();
    for (service, layers) in [(AiService::Tts, tts_layers), (AiService::Asr, asr_layers)] {
        if let Some(selection) = select_speech_model(settings, inventory, service)? {
            resolved.push(inspect_speech_selection(selection, config, layers).await?);
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, revisions: &[&str]) -> SpeechModelOption {
        SpeechModelOption {
            id: id.into(),
            name: id.into(),
            package_detected: false,
            verified_assets: !revisions.is_empty(),
            installed_revisions: revisions.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn selection_is_explicit_and_keeps_per_model_language() {
        let mut speech = SpeechSettings::default();
        let inventory = SpeechInventory {
            tts: vec![entry("f5_tts_en", &["rev1"])],
            asr: vec![entry("whisper", &[])],
            errors: vec![],
        };
        assert!(
            select_speech_model(&speech, &inventory, AiService::Tts)
                .unwrap()
                .is_none()
        );
        speech.tts.enabled = true;
        speech.tts.backend = TtsBackend::Local;
        speech.tts.local_model_id = "f5_tts_en".into();
        let tts = select_speech_model(&speech, &inventory, AiService::Tts)
            .unwrap()
            .unwrap();
        assert_eq!(tts.asset_revision.as_deref(), Some("rev1"));
        speech.asr.recognizer_id = "whisper".into();
        speech
            .asr
            .model_languages
            .insert("whisper".into(), "en".into());
        speech
            .asr
            .model_languages
            .insert("gigaam".into(), "ru".into());
        speech
            .asr_model_options
            .insert("whisper".into(), serde_json::json!({"beam_size": 5}));
        let asr = select_speech_model(&speech, &inventory, AiService::Asr)
            .unwrap()
            .unwrap();
        assert_eq!(asr.language_hint.as_deref(), Some("en"));
        assert_eq!(asr.overrides["beam_size"], 5);
        assert_eq!(speech.asr.model_languages["gigaam"], "ru");
    }

    #[tokio::test]
    async fn resolves_installed_mock_tts_with_python_validated_defaults() {
        let mut settings = SpeechSettings::default();
        settings.tts.enabled = true;
        settings.tts.backend = TtsBackend::Local;
        settings.tts.local_model_id = "mock_tts".into();
        let inventory = SpeechInventory {
            tts: vec![entry("mock_tts", &[])],
            asr: vec![],
            errors: vec![],
        };
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let config = PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into()),
            sdk_root: root.clone(),
            plugins_directory: root.join("plugins/builtin"),
            models_directory: None,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let selected = resolve_speech_activations(&settings, &inventory, &config, &[], &[])
            .await
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].activation.plugin_id, "mock_tts");
        assert_eq!(
            selected[0].activation.settings,
            serde_json::json!({"speaker":"default"})
        );
        assert!(selected[0].inspection.install_probe.installed);
    }

    #[test]
    fn selection_fails_closed_on_unknown_or_ambiguous_revision() {
        let mut speech = SpeechSettings::default();
        speech.asr.recognizer_id = "missing".into();
        let mut inventory = SpeechInventory::default();
        assert!(matches!(
            select_speech_model(&speech, &inventory, AiService::Asr),
            Err(SpeechSelectionError::UnknownPlugin { .. })
        ));
        inventory.asr.push(entry("missing", &["v1", "v2"]));
        assert!(matches!(
            select_speech_model(&speech, &inventory, AiService::Asr),
            Err(SpeechSelectionError::AmbiguousRevision(_))
        ));
    }
}
