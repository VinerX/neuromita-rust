mod catalog;
#[path = "../../localization/id.rs"]
mod id;
mod message;

pub use catalog::{FallbackPolicy, LanguageInfo, LocaleId, Localizer};
pub use message::{Argument, LocalizedText, i18n};

/// Exports the declarations extracted from code, including untranslated ID entries.
pub fn source_template_json() -> Result<String, LocalizationError> {
    let sources: std::collections::BTreeMap<String, String> = serde_json::from_str(include_str!(
        concat!(env!("OUT_DIR"), "/localization-messages.json")
    ))
    .map_err(|e| LocalizationError::Catalog(e.to_string()))?;
    let entries = sources
        .into_iter()
        .map(|(id, source)| (id, serde_json::json!({"source": source, "translation": ""})))
        .collect::<std::collections::BTreeMap<_, _>>();
    serde_json::to_string_pretty(&entries).map_err(|e| LocalizationError::Catalog(e.to_string()))
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LocalizationError {
    #[error("invalid locale: {0}")]
    InvalidLocale(String),
    #[error("unknown locale: {0}")]
    UnknownLocale(String),
    #[error("localization catalog: {0}")]
    Catalog(String),
    #[error("unknown localization ID: {0}")]
    UnknownId(String),
    #[error("source text differs from the declaration of {0}")]
    SourceMismatch(String),
    #[error("invalid template: {0}")]
    Template(String),
    #[error("localization arguments: {0}")]
    Arguments(String),
}
