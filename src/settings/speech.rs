use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::SettingsError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsBackend {
    #[default]
    Telegram,
    Local,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsPreferences {
    pub enabled: bool,
    pub backend: TtsBackend,
    pub telegram_auto_connect: bool,
    pub telegram_service: String,
    pub telegram_wait_seconds: u16,
    pub telegram_interval_seconds: u16,
    pub local_model_id: String,
    pub local_language: String,
    pub local_auto_load: bool,
    pub local_lazy_init: bool,
    pub api_profile: String,
    pub api_model: String,
    pub api_speed_percent: u16,
    pub default_voice_id: String,
    pub character_voice_ids: BTreeMap<crate::character::CharacterId, String>,
}

impl Default for TtsPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: TtsBackend::Telegram,
            telegram_auto_connect: false,
            telegram_service: "@silero_voice_bot".into(),
            telegram_wait_seconds: 12,
            telegram_interval_seconds: 2,
            local_model_id: String::new(),
            local_language: "ru".into(),
            local_auto_load: false,
            local_lazy_init: true,
            api_profile: String::new(),
            api_model: String::new(),
            api_speed_percent: 100,
            default_voice_id: String::new(),
            character_voice_ids: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsrPreferences {
    pub recognizer_id: String,
    pub device_id: String,
    pub audio_backend: String,
    pub instant_send: bool,
    pub delayed_send: bool,
    pub merge_chat: bool,
    pub pause_while_speaking: bool,
    pub send_delay_ms: u32,
    pub input_mode: String,
    pub model_languages: BTreeMap<String, String>,
    pub chunk_size: u32,
    pub vad_threshold_milli: u16,
    pub silence_ms: u32,
    pub pre_buffer_ms: u32,
    pub max_speech_ms: u32,
    pub min_speech_ms: u32,
}

impl Default for AsrPreferences {
    fn default() -> Self {
        Self {
            recognizer_id: String::new(),
            device_id: String::new(),
            audio_backend: String::new(),
            instant_send: false,
            delayed_send: true,
            merge_chat: true,
            pause_while_speaking: false,
            send_delay_ms: 3000,
            input_mode: "radio".into(),
            model_languages: BTreeMap::new(),
            chunk_size: 512,
            vad_threshold_milli: 500,
            silence_ms: 600,
            pre_buffer_ms: 400,
            max_speech_ms: 30_000,
            min_speech_ms: 350,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechSettings {
    pub tts: TtsPreferences,
    pub asr: AsrPreferences,
    pub tts_model_options: BTreeMap<String, serde_json::Value>,
    pub asr_model_options: BTreeMap<String, serde_json::Value>,
}

impl SpeechSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        let t = &self.tts;
        if t.telegram_wait_seconds == 0
            || t.telegram_wait_seconds > 600
            || t.telegram_interval_seconds > 600
            || !(50..=200).contains(&t.api_speed_percent)
            || t.telegram_service.len() > 256
            || [
                t.local_model_id.as_str(),
                t.api_profile.as_str(),
                t.api_model.as_str(),
            ]
            .iter()
            .any(|value| value.len() > 128)
            || !matches!(t.local_language.as_str(), "ru" | "en" | "auto" | "")
            || t.default_voice_id.len() > 512
            || t.character_voice_ids.len() > crate::character::CharacterId::ALL.len()
            || t.character_voice_ids
                .iter()
                .any(|(_, value)| value.len() > 512)
        {
            return Err(SettingsError::Validation("invalid TTS preferences".into()));
        }
        for models in [&self.tts_model_options, &self.asr_model_options] {
            if models.len() > 64 {
                return Err(SettingsError::Validation(
                    "too many speech model options".into(),
                ));
            }
            for (plugin_id, options) in models {
                if options.get("language").is_some() {
                    return Err(SettingsError::Validation(
                        "language belongs to the typed language preference, not model overrides"
                            .into(),
                    ));
                }
                if plugin_id.is_empty()
                    || plugin_id.len() > 128
                    || !plugin_id.is_ascii()
                    || !plugin_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                    || !options.as_object().is_some_and(|fields| {
                        fields.len() <= 128
                            && fields.iter().all(|(key, value)| {
                                !key.is_empty()
                                    && key.len() <= 128
                                    && match value {
                                        serde_json::Value::String(text) => text.len() <= 4096,
                                        serde_json::Value::Bool(_)
                                        | serde_json::Value::Number(_)
                                        | serde_json::Value::Null => true,
                                        _ => false,
                                    }
                            })
                    })
                    || serde_json::to_vec(options).is_ok_and(|bytes| bytes.len() > 32_768)
                {
                    return Err(SettingsError::Validation(
                        "invalid speech model options".into(),
                    ));
                }
            }
        }
        for (plugin, language) in &self.asr.model_languages {
            if let Some(capability) =
                crate::ai_runtime::speech_languages::asr_language_capability(plugin)
                && !capability
                    .choices
                    .iter()
                    .any(|choice| choice.id == *language)
            {
                return Err(SettingsError::Validation(format!(
                    "unsupported ASR language {language} for {plugin}"
                )));
            }
        }
        let a = &self.asr;
        if a.recognizer_id.len() > 128
            || a.device_id.len() > 512
            || a.audio_backend.len() > 64
            || a.send_delay_ms > 120_000
            || a.chunk_size == 0
            || a.chunk_size > 65_536
            || a.vad_threshold_milli > 1000
            || a.silence_ms > 120_000
            || a.pre_buffer_ms > 30_000
            || !(1000..=600_000).contains(&a.max_speech_ms)
            || a.min_speech_ms > a.max_speech_ms
            || a.model_languages.len() > 64
            || a.model_languages
                .iter()
                .any(|(key, value)| key.len() > 128 || value.len() > 32)
            || !matches!(
                a.input_mode.as_str(),
                "radio" | "continuous" | "push_to_talk"
            )
        {
            return Err(SettingsError::Validation("invalid ASR preferences".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_asr_languages_are_checked_before_persistence() {
        let mut speech = SpeechSettings::default();
        speech
            .asr
            .model_languages
            .insert("google".into(), "en-US".into());
        assert!(speech.validate().is_ok());
        speech
            .asr
            .model_languages
            .insert("google".into(), "en".into());
        assert!(speech.validate().is_err());
        speech
            .asr
            .model_languages
            .insert("google".into(), "ru-RU".into());
        speech
            .asr
            .model_languages
            .insert("whisper".into(), "auto".into());
        assert!(speech.validate().is_ok());
    }

    #[test]
    fn strict_settings_reject_missing_fields_and_index_keys() {
        let settings = crate::settings::AppSettings::initial_profile();
        assert!(settings.validate().is_ok());
        let mut json = serde_json::to_value(&settings).unwrap();
        json.as_object_mut().unwrap().remove("speech");
        assert!(serde_json::from_value::<crate::settings::AppSettings>(json).is_err());
        let mut json = serde_json::to_value(&settings.speech).unwrap();
        json["tts"]["character_voice_ids"] = serde_json::json!({"3":"wrong"});
        assert!(serde_json::from_value::<SpeechSettings>(json).is_err());
        let mut json = serde_json::to_value(&settings.speech).unwrap();
        json["tts"].as_object_mut().unwrap().remove("enabled");
        assert!(serde_json::from_value::<SpeechSettings>(json).is_err());
    }
}

#[cfg(test)]
mod model_options_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_options_are_required_and_bounded() {
        let mut json = serde_json::to_value(SpeechSettings::default()).unwrap();
        json.as_object_mut().unwrap().remove("tts_model_options");
        json.as_object_mut().unwrap().remove("asr_model_options");
        assert!(serde_json::from_value::<SpeechSettings>(json).is_err());
        let mut invalid = SpeechSettings::default();
        invalid
            .asr_model_options
            .insert("whisper".into(), json!({"nested": {"unsafe": 1}}));
        assert!(invalid.validate().is_err());
        invalid
            .asr_model_options
            .insert("whisper".into(), json!({"beam_size": 5}));
        assert!(invalid.validate().is_ok());
        invalid
            .asr_model_options
            .insert("whisper".into(), json!({"language":"en"}));
        assert!(invalid.validate().is_err());
    }
}
