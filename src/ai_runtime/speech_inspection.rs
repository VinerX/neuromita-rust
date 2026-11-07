use serde::Deserialize;
use serde_json::Value;

use super::{AiRuntimeError, AiService, install::PluginInstallPlan};

#[derive(Debug, Clone, Deserialize)]
pub struct InstallProbe {
    pub installed: bool,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub missing_assets: Vec<String>,
    #[serde(default)]
    pub missing_distributions: Vec<String>,
    #[serde(default)]
    pub wrong_versions: Vec<Value>,
}

impl InstallProbe {
    pub fn missing_reasons(&self) -> Vec<String> {
        let mut reasons = self.missing.clone();
        reasons.extend(
            self.missing_assets
                .iter()
                .map(|item| format!("asset:{item}")),
        );
        reasons.extend(
            self.missing_distributions
                .iter()
                .map(|item| format!("package:{item}")),
        );
        reasons.extend(
            self.wrong_versions
                .iter()
                .map(|item| format!("version:{item}")),
        );
        reasons
    }

    pub(super) fn validate(&self) -> Result<(), AiRuntimeError> {
        let too_many = [
            self.missing.len(),
            self.missing_assets.len(),
            self.missing_distributions.len(),
            self.wrong_versions.len(),
        ]
        .into_iter()
        .any(|count| count > 128);
        let oversized = self
            .missing_reasons()
            .iter()
            .any(|reason| reason.len() > 1024);
        if too_many || oversized || (self.installed && !self.missing_reasons().is_empty()) {
            return Err(AiRuntimeError::Protocol(
                "inconsistent or unbounded plugin installation probe".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SpeechPluginInspection {
    pub service: AiService,
    pub plugin_id: String,
    pub settings_schema: Value,
    pub effective_settings: Value,
    pub install_plan: PluginInstallPlan,
    pub install_probe: InstallProbe,
}

impl SpeechPluginInspection {
    pub fn requires_assets(&self) -> bool {
        self.install_plan.assets.iter().any(|asset| asset.required)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use serde_json::json;

    use super::*;
    use crate::ai_runtime::{PythonHostConfig, fetch_speech_plugin_inspection};

    fn fixture_host(root: &std::path::Path) -> PythonHostConfig {
        PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" })),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: Some(root.join("models")),
            request_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn inspects_schema_and_probe_without_running_model() {
        let temp = std::env::temp_dir().join(format!("speech-inspect-{}", uuid::Uuid::new_v4()));
        let plugin = temp.join("plugins/demo_asr");
        tokio::fs::create_dir_all(&plugin).await.unwrap();
        tokio::fs::write(plugin.join("plugin.json"), r#"{"id":"demo_asr","name":"Demo ASR","kind":"asr","entrypoint":"plugin.py","api_version":1}"#).await.unwrap();
        tokio::fs::write(
            plugin.join("installplan.py"),
            r#"from neuromita_ai_sdk import InstallPlan
class InstallContract:
    def settings_schema(self): return {'language': {'type': 'string', 'default': 'ru', 'choices': ['ru', 'en']}}
    def install_plan(self): return InstallPlan()
    def install_plan_for_settings(self, settings): return InstallPlan()
    def probe_install(self): return {'installed': False, 'missing': ['language:en']}
    def probe_install_for_settings(self, settings):
        if settings['language'] == 'en': return {'installed': True}
        return {'installed': False, 'missing': ['language:en']}
"#,
        ).await.unwrap();
        let host = fixture_host(&temp);
        let ready = fetch_speech_plugin_inspection(
            &host,
            AiService::Asr,
            "demo_asr",
            json!({}),
            Some("en"),
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(ready.effective_settings, json!({"language":"en"}));
        assert_eq!(ready.settings_schema["language"]["default"], "ru");
        assert!(ready.install_probe.installed);
        let missing = fetch_speech_plugin_inspection(
            &host,
            AiService::Asr,
            "demo_asr",
            json!({}),
            None,
            None,
            &[],
        )
        .await
        .unwrap();
        assert!(!missing.install_probe.installed);
        assert_eq!(missing.install_probe.missing, ["language:en"]);
        assert!(
            fetch_speech_plugin_inspection(
                &host,
                AiService::Asr,
                "demo_asr",
                json!({}),
                Some("invalid"),
                None,
                &[],
            )
            .await
            .is_err()
        );
        assert!(
            fetch_speech_plugin_inspection(
                &host,
                AiService::Tts,
                "demo_asr",
                json!({}),
                Some("en"),
                None,
                &[],
            )
            .await
            .is_err()
        );
        assert!(
            fetch_speech_plugin_inspection(
                &host,
                AiService::Asr,
                "demo_asr",
                json!({"language":"ru"}),
                Some("en"),
                None,
                &[]
            )
            .await
            .is_err()
        );
        tokio::fs::remove_dir_all(temp).await.unwrap();
    }

    #[test]
    fn malformed_or_contradictory_probe_never_claims_ready() {
        let probe: InstallProbe = serde_json::from_value(json!({
            "installed": true,
            "missing_assets": ["weights.bin"]
        }))
        .unwrap();
        assert!(probe.validate().is_err());
        assert!(serde_json::from_value::<InstallProbe>(json!({"missing":[]})).is_err());
    }
}
