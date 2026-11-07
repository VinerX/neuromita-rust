use crate::localization::{LocalizedText, i18n};
use std::sync::Arc;

use tokio::sync::watch;

use crate::generation::{
    PresetCatalog, PresetCatalogError, PresetCatalogSnapshot, PresetDefinition, PresetId,
    PresetRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOption {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub schema_label: &'static str,
    pub documentation: &'static str,
    pub models_url: &'static str,
    pub credentials_url: &'static str,
    pub pricing: &'static str,
    pub badge_kind: &'static str,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetParameterOption {
    pub id: &'static str,
    pub wire_key: &'static str,
    pub title: LocalizedText,
    pub description: LocalizedText,
    pub editable: bool,
    pub sent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetView {
    pub api_key: Option<crate::generation::SecretKey>,
    pub credential_error: Option<String>,
    pub output_token_wire_key: &'static str,
    pub template_id: &'static str,
    pub send_temperature: bool,
    pub send_max_output_tokens: bool,
    pub id: PresetId,
    pub configured: bool,
    pub name: String,
    pub model: String,
    pub provider: &'static str,
    pub endpoint: String,
    pub credential: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PresetsView {
    pub revision: u64,
    pub records: Vec<PresetView>,
    pub providers: Vec<ProviderOption>,
    pub parameters: Vec<PresetParameterOption>,
}

pub trait PresetsApi: Send + Sync {
    fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>>;
    fn snapshot(&self) -> PresetsView;
    fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError>;
    fn create_unconfigured(&self) -> Result<(PresetId, PresetRecord), PresetCatalogError>;
    fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError>;
    fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError>;
    fn require(&self, id: PresetId) -> Result<(), PresetCatalogError>;
}

pub struct PresetsPresenter {
    catalog: Arc<PresetCatalog>,
}

impl PresetsPresenter {
    pub fn template(id: &str) -> Result<crate::generation::PresetTemplate, PresetCatalogError> {
        crate::generation::PresetTemplate::named(id)
    }

    pub(crate) fn new(catalog: Arc<PresetCatalog>) -> Self {
        Self { catalog }
    }

    pub fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.catalog.observe()
    }

    pub fn list(&self) -> Vec<(PresetId, PresetRecord)> {
        self.catalog.list()
    }

    pub fn snapshot(&self) -> PresetsView {
        let catalog = self.catalog.observe();
        let snapshot = catalog.borrow();
        let records = snapshot
            .entries
            .iter()
            .map(|(&id, record)| {
                let (
                    configured,
                    provider,
                    model,
                    endpoint,
                    credential,
                    temperature,
                    max_output_tokens,
                    timeout_ms,
                ) = match &record.definition {
                    PresetDefinition::Unconfigured => (
                        false,
                        "",
                        String::new(),
                        String::new(),
                        String::new(),
                        0.0,
                        0,
                        0,
                    ),
                    PresetDefinition::OpenAi(definition) => (
                        true,
                        if definition.protocol
                            == crate::generation::openai::OpenAiProtocol::OpenRouter
                        {
                            "openrouter"
                        } else {
                            "openai"
                        },
                        definition.model.clone(),
                        definition.endpoint.clone(),
                        definition
                            .credential
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "none".into()),
                        definition.temperature,
                        definition.max_output_tokens,
                        definition.timeout_ms,
                    ),
                    PresetDefinition::Gemini(definition) => (
                        true,
                        "google",
                        definition.model.clone(),
                        definition.endpoint.clone(),
                        definition.credential.to_string(),
                        definition.temperature,
                        definition.max_output_tokens,
                        definition.timeout_ms,
                    ),
                };
                let key = self.catalog.editor_key(id);
                PresetView {
                    api_key: key.as_ref().ok().cloned().flatten(),
                    credential_error: key.err().map(|error| error.to_string()),
                    output_token_wire_key: match &record.definition {
                        PresetDefinition::Gemini(_) => "maxOutputTokens",
                        PresetDefinition::OpenAi(config)
                            if config.protocol
                                == crate::generation::openai::OpenAiProtocol::Responses =>
                        {
                            "max_output_tokens"
                        }
                        PresetDefinition::OpenAi(_) => "max_tokens",
                        PresetDefinition::Unconfigured => "",
                    },
                    template_id: match &record.definition {
                        PresetDefinition::Gemini(_) => "gemini",
                        PresetDefinition::OpenAi(config) => config.protocol.template_id(),
                        PresetDefinition::Unconfigured => "",
                    },
                    send_temperature: match &record.definition {
                        PresetDefinition::Gemini(config) => config.options.send_temperature,
                        PresetDefinition::OpenAi(config) => config.options.send_temperature,
                        PresetDefinition::Unconfigured => false,
                    },
                    send_max_output_tokens: match &record.definition {
                        PresetDefinition::Gemini(config) => config.options.send_max_output_tokens,
                        PresetDefinition::OpenAi(config) => config.options.send_max_output_tokens,
                        PresetDefinition::Unconfigured => false,
                    },
                    id,
                    configured,
                    name: record.name.clone(),
                    model,
                    provider,
                    endpoint,
                    credential,
                    temperature,
                    max_output_tokens,
                    timeout_ms,
                }
            })
            .collect();
        PresetsView {
            revision: snapshot.revision,
            records,
            providers: Self::providers(),
            parameters: Self::parameters(),
        }
    }

    /// Returns the display-only request URL template for a provider.
    /// The persisted endpoint remains the transport's base address, never the expanded URL.
    pub fn request_url_template(provider_id: &str, endpoint: &str) -> Option<String> {
        if endpoint.is_empty() || Self::template(provider_id).is_err() {
            return None;
        }
        if provider_id != "gemini" {
            return Some(endpoint.into());
        }
        Some(format!(
            "{}/models/{{model}}:generateContent",
            endpoint.trim_end_matches('/')
        ))
    }

    pub fn providers() -> Vec<ProviderOption> {
        const CATALOG: &[(&str, &str, &str, &str, &str, &str)] = &[
            ("compatible", "OpenAI-compatible", "openai", "", "mixed", ""),
            (
                "gemini",
                "Google AI Studio",
                "google",
                "https://ai.google.dev/gemini-api/docs",
                "mixed",
                "",
            ),
            (
                "openrouter",
                "OpenRouter",
                "openrouter",
                "https://openrouter.ai/docs/quickstart",
                "mixed",
                "",
            ),
            (
                "mistral",
                "Mistral AI",
                "mistral",
                "https://docs.mistral.ai/",
                "mixed",
                "",
            ),
            ("aiio", "Ai.iO", "aiio", "", "paid", ""),
            ("proxyapi", "ProxyAPI", "proxyapi", "", "paid", "ru"),
            (
                "groq",
                "Groq",
                "groq",
                "https://console.groq.com/docs/overview",
                "mixed",
                "",
            ),
            (
                "together",
                "Together AI",
                "together",
                "https://docs.together.ai/",
                "mixed",
                "",
            ),
            ("chutes", "Chutes", "chutes", "", "paid", ""),
            (
                "lmstudio",
                "LM Studio",
                "lmstudio",
                "https://lmstudio.ai/docs",
                "free",
                "local",
            ),
            (
                "ollama",
                "Ollama",
                "ollama",
                "https://docs.ollama.com/",
                "free",
                "local",
            ),
            (
                "kodikrouter",
                "KodikRouter",
                "kodikrouter",
                "",
                "paid",
                "ru",
            ),
            (
                "openai",
                "OpenAI Responses (API key)",
                "openai",
                "https://platform.openai.com/docs/api-reference/responses",
                "paid",
                "",
            ),
            (
                "codex",
                "ChatGPT Plan (Codex)",
                "openai",
                "",
                "paid",
                "subscription",
            ),
        ];
        CATALOG
            .iter()
            .map(
                |&(id, name, icon, documentation, pricing, badge_kind)| ProviderOption {
                    id,
                    name,
                    icon,
                    schema_label: if id == "gemini" {
                        "Google Gemini API"
                    } else if id == "openai" {
                        "OpenAI Responses"
                    } else if id == "codex" {
                        "ChatGPT OAuth"
                    } else {
                        "Chat Completions"
                    },
                    documentation,
                    models_url: if id == "gemini" {
                        "https://ai.google.dev/gemini-api/docs/models"
                    } else {
                        ""
                    },
                    credentials_url: if id == "gemini" {
                        "https://aistudio.google.com/app/apikey"
                    } else {
                        ""
                    },
                    pricing,
                    badge_kind,
                    supported: Self::template(id).is_ok(),
                },
            )
            .collect()
    }

    pub fn parameters() -> Vec<PresetParameterOption> {
        vec![
            PresetParameterOption {
                id: "temperature",
                wire_key: "temperature",
                title: i18n("settings.parameter.temperature.title", "Температура"),
                description: i18n(
                    "settings.parameter.temperature.description",
                    "Разнообразие ответов: ниже — предсказуемее, выше — свободнее.",
                ),
                editable: true,
                sent: true,
            },
            PresetParameterOption {
                id: "max_tokens",
                wire_key: "maxOutputTokens",
                title: i18n("settings.parameter.max_tokens.title", "Макс. токенов"),
                description: i18n(
                    "settings.parameter.max_tokens.description",
                    "Лимит выходных токенов, включая токены размышления.",
                ),
                editable: true,
                sent: true,
            },
            PresetParameterOption {
                id: "top_p",
                wire_key: "topP",
                title: i18n("settings.parameter.top_p.title", "Порог Top-P"),
                description: i18n(
                    "settings.parameter.top_p.description",
                    "Ограничивает выбор слов по суммарной вероятности.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "presence_penalty",
                wire_key: "presencePenalty",
                title: i18n(
                    "settings.parameter.presence_penalty.title",
                    "Штраф присутствия",
                ),
                description: i18n(
                    "settings.parameter.presence_penalty.description",
                    "Снижает вероятность повторного использования уже встречавшихся слов.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "frequency_penalty",
                wire_key: "frequencyPenalty",
                title: i18n(
                    "settings.parameter.frequency_penalty.title",
                    "Штраф частоты",
                ),
                description: i18n(
                    "settings.parameter.frequency_penalty.description",
                    "Снижает вероятность слов пропорционально числу их повторений.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "top_k",
                wire_key: "topK",
                title: i18n("settings.parameter.top_k.title", "Лимит Top-K"),
                description: i18n(
                    "settings.parameter.top_k.description",
                    "Число кандидатов при выборе следующего токена.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "thinking_level",
                wire_key: "thinkingLevel",
                title: i18n(
                    "settings.parameter.thinking_level.title",
                    "Глубина размышлений",
                ),
                description: i18n(
                    "settings.parameter.thinking_level.description",
                    "Низкий thinkingLevel сокращает размышления, но не отключает мышление.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "thinking_budget",
                wire_key: "thinkingBudget",
                title: i18n(
                    "settings.parameter.thinking_budget.title",
                    "Бюджет мышления Gemini",
                ),
                description: i18n(
                    "settings.parameter.thinking_budget.description",
                    "Лимит токенов размышления: -1 — автоматически, 0 — отключить, если модель поддерживает.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "include_thoughts",
                wire_key: "includeThoughts",
                title: i18n(
                    "settings.parameter.include_thoughts.title",
                    "Возвращать сводки размышлений",
                ),
                description: i18n(
                    "settings.parameter.include_thoughts.description",
                    "Возвращает сводки размышлений и не управляет объёмом мышления.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "timeout",
                wire_key: "",
                title: i18n("settings.parameter.timeout.title", "Тайм-аут"),
                description: i18n(
                    "settings.parameter.timeout.description",
                    "Время ожидания ответа API в миллисекундах.",
                ),
                editable: true,
                sent: false,
            },
        ]
    }

    pub fn create_unconfigured(&self) -> Result<(PresetId, PresetRecord), PresetCatalogError> {
        let names: std::collections::HashSet<String> = self
            .catalog
            .list()
            .into_iter()
            .map(|(_, record)| record.name)
            .collect();
        let name = (1..)
            .map(|number| format!("Пользовательский API {number}"))
            .find(|name| !names.contains(name))
            .ok_or(PresetCatalogError::IdExhausted)?;
        let record = PresetRecord {
            name,
            definition: PresetDefinition::Unconfigured,
        };
        let id = self.catalog.create(record.clone())?;
        Ok((id, record))
    }

    pub fn require_configured(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        if self.catalog.configured(id)? {
            Ok(())
        } else {
            Err(PresetCatalogError::Invalid(
                "preset is not configured".into(),
            ))
        }
    }

    pub fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        self.catalog.create(record)
    }

    pub fn update_connection(
        &self,
        id: PresetId,
        edit: crate::generation::PresetConnectionEdit,
    ) -> Result<(), PresetCatalogError> {
        self.catalog.update_connection(id, edit)
    }

    pub fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        self.catalog.update(id, record)
    }

    pub fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.catalog.delete(id)
    }

    pub fn require(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        if self.catalog.contains(id) {
            Ok(())
        } else {
            Err(PresetCatalogError::NotFound(id))
        }
    }
}

impl PresetsApi for PresetsPresenter {
    fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.observe()
    }
    fn snapshot(&self) -> PresetsView {
        self.snapshot()
    }
    fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        self.create(record)
    }
    fn create_unconfigured(&self) -> Result<(PresetId, PresetRecord), PresetCatalogError> {
        self.create_unconfigured()
    }
    fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        self.update(id, record)
    }
    fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.delete(id)
    }
    fn require(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.require(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presenter_snapshot_preserves_catalog_values() {
        let presenter = PresetsPresenter::new(Arc::new(PresetCatalog::new(Default::default())));
        let mut draft = PresetRecord {
            name: "test-preset".into(),
            definition: PresetDefinition::Gemini(crate::generation::GeminiPresetDefinition {
                options: Default::default(),
                transforms: Vec::new(),
                endpoint: crate::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT.into(),
                model: "test-model".into(),
                credential: "TEST_KEY_ENV".into(),
                temperature: 0.7,
                max_output_tokens: 3000,
                timeout_ms: 60000,
            }),
        };
        let PresetDefinition::Gemini(definition) = &mut draft.definition else {
            unreachable!()
        };
        definition.model = "test-model".into();
        definition.credential = "TEST_KEY_ENV".into();
        definition.temperature = 1.3;
        definition.max_output_tokens = 2500;
        let created = (&presenter as &dyn PresetsApi).create(draft).unwrap();
        let snapshot = (&presenter as &dyn PresetsApi).snapshot();
        let record = snapshot
            .records
            .iter()
            .find(|record| record.id == created)
            .unwrap();
        assert_eq!(record.name, "test-preset");
        assert_eq!(record.model, "test-model");
        assert_eq!(record.credential, "env:TEST_KEY_ENV");
        assert_eq!(record.temperature, 1.3);
        assert_eq!(record.max_output_tokens, 2500);
        assert!(
            snapshot
                .parameters
                .iter()
                .any(|option| option.id == "temperature" && option.editable)
        );
        assert!(
            snapshot
                .parameters
                .iter()
                .any(|option| option.id == "top_p" && !option.editable)
        );
        (&presenter as &dyn PresetsApi).require(created).unwrap();
        (&presenter as &dyn PresetsApi).delete(created).unwrap();
        assert!(
            (&presenter as &dyn PresetsApi)
                .snapshot()
                .records
                .is_empty()
        );
    }

    #[test]
    fn new_preset_is_immediately_persisted_and_unconfigured() {
        let catalog = Arc::new(PresetCatalog::new(Default::default()));
        let presenter = PresetsPresenter::new(Arc::clone(&catalog));
        let (first, record) = presenter.create_unconfigured().unwrap();
        assert_eq!(record.name, "Пользовательский API 1");
        assert!(matches!(record.definition, PresetDefinition::Unconfigured));
        assert!(catalog.contains(first));
        assert!(presenter.require_configured(first).is_err());
        assert!(
            presenter
                .snapshot()
                .records
                .iter()
                .any(|row| row.id == first && !row.configured)
        );
        let (second, _) = presenter.create_unconfigured().unwrap();
        assert_ne!(first, second);
        assert_eq!(presenter.snapshot().records.len(), 2);
        presenter.delete(first).unwrap();
        let (third, _) = presenter.create_unconfigured().unwrap();
        assert_eq!(
            presenter
                .snapshot()
                .records
                .iter()
                .find(|row| row.id == third)
                .unwrap()
                .name,
            "Пользовательский API 1"
        );
    }

    #[test]
    fn request_url_template_uses_saved_endpoint_without_embedding_a_key() {
        let template =
            PresetsPresenter::request_url_template("gemini", "http://127.0.0.1:12345/v1beta/")
                .unwrap();
        assert_eq!(
            template.replace("{model}", "gemini-2.5-flash"),
            "http://127.0.0.1:12345/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            PresetsPresenter::request_url_template(
                "gemini",
                crate::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT,
            )
            .unwrap()
            .replace("{model}", "gemini-3.6-flash"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.6-flash:generateContent"
        );
        assert_eq!(PresetsPresenter::request_url_template("gemini", ""), None);
        assert_eq!(
            PresetsPresenter::request_url_template("openrouter", "https://openrouter.ai/"),
            Some("https://openrouter.ai/".into())
        );
    }

    #[test]
    fn provider_pricing_and_badges_match_python_templates() {
        let providers = PresetsPresenter::providers();
        for (id, pricing, kind) in [
            ("gemini", "mixed", ""),
            ("openrouter", "mixed", ""),
            ("mistral", "mixed", ""),
            ("aiio", "paid", ""),
            ("proxyapi", "paid", "ru"),
            ("groq", "mixed", ""),
            ("together", "mixed", ""),
            ("chutes", "paid", ""),
            ("lmstudio", "free", "local"),
            ("ollama", "free", "local"),
            ("kodikrouter", "paid", "ru"),
            ("openai", "paid", ""),
            ("codex", "paid", "subscription"),
        ] {
            let option = providers.iter().find(|option| option.id == id).unwrap();
            assert_eq!((option.pricing, option.badge_kind), (pricing, kind));
        }
    }

    #[test]
    fn empty_catalog_does_not_invent_preset_values() {
        let presenter = PresetsPresenter::new(Arc::new(PresetCatalog::new(Default::default())));
        let snapshot = (&presenter as &dyn PresetsApi).snapshot();
        assert!(snapshot.records.is_empty());
        assert!(
            snapshot
                .providers
                .iter()
                .any(|item| item.id == "gemini" && item.supported)
        );
        assert!(
            snapshot
                .providers
                .iter()
                .any(|item| item.id == "compatible" && item.supported)
        );
        assert!(
            snapshot
                .providers
                .iter()
                .any(|item| item.id == "codex" && !item.supported)
        );
        let (id, record) = presenter.create_unconfigured().unwrap();
        assert_eq!(record.name, "Пользовательский API 1");
        assert!(matches!(record.definition, PresetDefinition::Unconfigured));
        assert!(!presenter.catalog.configured(id).unwrap());
        assert_eq!(presenter.snapshot().records.len(), 1);
    }
}
