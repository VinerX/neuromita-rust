//! Single embedded language catalog shared with the Python ASR install contracts.

use std::{collections::BTreeMap, sync::OnceLock};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AsrLanguageOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AsrLanguageCapability {
    pub default: String,
    pub hint: String,
    pub choices: Vec<AsrLanguageOption>,
}

pub fn asr_language_capability(plugin_id: &str) -> Option<&'static AsrLanguageCapability> {
    static LANGUAGES: OnceLock<BTreeMap<String, AsrLanguageCapability>> = OnceLock::new();
    LANGUAGES
        .get_or_init(|| {
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/python/neuromita_asr/languages.json"
            )))
            .expect("bundled ASR languages must be valid")
        })
        .get(plugin_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_catalog_matches_python_options() {
        let google = asr_language_capability("google").unwrap();
        assert_eq!(google.default, "ru-RU");
        assert_eq!(google.choices.len(), 102);
        assert!(google.choices.iter().any(|choice| choice.id == "en-US"));
        let whisper = asr_language_capability("whisper").unwrap();
        assert_eq!(whisper.choices.len(), 101);
        assert_eq!(whisper.choices[0].id, "auto");
        assert_eq!(asr_language_capability("gigaam").unwrap().choices.len(), 1);
        assert!(asr_language_capability("unknown").is_none());
    }
}
