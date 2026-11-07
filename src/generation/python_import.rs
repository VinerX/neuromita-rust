use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::gemini::{
    GeminiRequestOptions, GeminiTemplate, GeminiThinking, SystemInstructionMode, ThinkingLevel,
};
use super::{
    CredentialReference, GeminiPresetDefinition, PresetCatalogError, PresetDefinition, PresetId,
    PresetRecord,
};

#[derive(Debug, Clone, Serialize)]
pub struct ImportedTemplate {
    pub source_id: String,
    pub name: String,
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportBlocker {
    pub source_id: String,
    pub reason: String,
}

/// An explicit import plan. Secrets are intentionally excluded from Debug and
/// serialization; the caller decides where to persist the private credential map.
pub struct PythonPresetImport {
    pub records: BTreeMap<PresetId, PresetRecord>,
    pub templates: Vec<ImportedTemplate>,
    pub blockers: Vec<ImportBlocker>,
    credentials: BTreeMap<String, String>,
}

fn invalid(reason: &str) -> PresetCatalogError {
    PresetCatalogError::Invalid(reason.into())
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, PresetCatalogError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("missing string field {field}")))
}

fn resolve_protocol<'a>(
    preset: &'a Value,
    template: Option<&'a Value>,
) -> Result<&'a str, PresetCatalogError> {
    if let Some(protocol) = template
        .and_then(|t| t.get("protocol_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(protocol);
    }
    if let Some(protocol) = preset
        .get("protocol_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(protocol);
    }
    template
        .and_then(|t| t.get("protocol_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("protocol is not configured"))
}

fn has_entries(value: Option<&Value>) -> bool {
    value.is_some_and(|v| match v {
        Value::Null => false,
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    })
}

impl PythonPresetImport {
    pub fn parse(presets: &[u8], templates: &[u8]) -> Result<Self, PresetCatalogError> {
        let presets: Value =
            serde_json::from_slice(presets).map_err(|_| invalid("invalid Python preset JSON"))?;
        let templates: Value = serde_json::from_slice(templates)
            .map_err(|_| invalid("invalid Python template JSON"))?;
        let presets = presets
            .get("presets")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("missing Python presets object"))?;
        let source_templates = templates
            .get("templates")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("missing Python templates object"))?;
        let mut plan = Self {
            records: BTreeMap::new(),
            templates: Vec::new(),
            blockers: Vec::new(),
            credentials: BTreeMap::new(),
        };
        for (id, template) in source_templates {
            let protocol = text(template, "protocol_id")?;
            let endpoint = text(template, "url")?;
            plan.templates.push(ImportedTemplate {
                source_id: id.clone(),
                name: text(template, "name")?.into(),
                protocol: protocol.into(),
                endpoint: if endpoint.is_empty() && protocol == "google_gemini_default" {
                    GeminiTemplate::GOOGLE_ENDPOINT.into()
                } else {
                    endpoint.into()
                },
                model: text(template, "default_model")?.into(),
            });
        }
        for (id, preset) in presets {
            let result = plan.convert(id, preset, source_templates);
            if let Err(error) = result {
                plan.blockers.push(ImportBlocker {
                    source_id: id.clone(),
                    reason: error.to_string(),
                });
            }
        }
        Ok(plan)
    }

    fn convert(
        &mut self,
        id: &str,
        preset: &Value,
        templates: &serde_json::Map<String, Value>,
    ) -> Result<(), PresetCatalogError> {
        let base = match preset.get("base") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if id.is_empty() => None,
            Some(Value::String(id)) => Some(id.clone()),
            Some(Value::Number(id)) if id.as_u64().is_some() => Some(id.to_string()),
            _ => return Err(invalid("invalid Python template ID")),
        };
        let template = base
            .as_ref()
            .map(|id| {
                templates
                    .get(id)
                    .ok_or_else(|| invalid("selected Python template does not exist"))
            })
            .transpose()?;
        let protocol = resolve_protocol(preset, template)?;
        if protocol != "google_gemini_default" {
            return self.convert_openai(id, preset, template, protocol);
        }
        for field in [
            "reserve_keys",
            "fallbacks",
            "generation_overrides",
            "model_profile_overrides",
        ] {
            if has_entries(preset.get(field)) {
                return Err(invalid(&format!("import requires support for {field}")));
            }
        }
        let overrides = preset
            .get("protocol_overrides")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("protocol overrides must be an object"))?;
        if overrides.keys().any(|key| key != "transforms") {
            return Err(invalid("unsupported protocol override"));
        }
        let transforms = overrides
            .get("transforms")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|_| invalid("invalid or unsupported message transforms"))
            })
            .transpose()?
            .unwrap_or_default();
        let settings = preset
            .get("model_settings")
            .ok_or_else(|| invalid("missing model settings"))?;
        let values = settings
            .get("values")
            .ok_or_else(|| invalid("missing model setting values"))?;
        let enabled = settings
            .get("enabled")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("missing enabled model settings"))?;
        let enabled = enabled
            .iter()
            .map(|v| v.as_str().ok_or_else(|| invalid("invalid enabled setting")))
            .collect::<Result<Vec<_>, _>>()?;
        if enabled.iter().any(|id| {
            ![
                "temperature",
                "max_tokens",
                "top_p",
                "top_k",
                "thinking_level",
                "thinking_budget",
                "include_thoughts",
            ]
            .contains(id)
        }) {
            return Err(invalid("unsupported enabled model setting"));
        }
        let number = |key: &str| {
            values
                .get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| invalid(&format!("missing number {key}")))
        };
        let integer = |key: &str| {
            values
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| invalid(&format!("invalid integer {key}")))
        };
        let include_thoughts = if enabled.contains(&"include_thoughts") {
            Some(
                values
                    .get("include_thoughts")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("invalid include_thoughts"))?,
            )
        } else {
            None
        };
        if enabled.contains(&"thinking_level") && enabled.contains(&"thinking_budget") {
            return Err(invalid("thinking level and budget are mutually exclusive"));
        }
        let thinking = if enabled.contains(&"thinking_level") {
            let level: ThinkingLevel = serde_json::from_value(
                values
                    .get("thinking_level")
                    .cloned()
                    .ok_or_else(|| invalid("missing thinking level"))?,
            )
            .map_err(|_| invalid("invalid thinking level"))?;
            Some(GeminiThinking::Level {
                level,
                include_thoughts,
            })
        } else if enabled.contains(&"thinking_budget") {
            Some(GeminiThinking::Budget {
                tokens: values
                    .get("thinking_budget")
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| invalid("invalid thinking budget"))?,
                include_thoughts,
            })
        } else if include_thoughts.is_some() {
            return Err(invalid(
                "include_thoughts requires an explicit thinking mode",
            ));
        } else {
            None
        };
        let endpoint = preset
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                template
                    .and_then(|t| t.get("url"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(GeminiTemplate::GOOGLE_ENDPOINT);
        let model = text(preset, "default_model")?;
        let secret = text(preset, "key")?;
        if secret.is_empty() {
            return Err(invalid("missing API key"));
        }
        let credential_name = format!("python_preset_{id}");
        let lower_model = model.to_ascii_lowercase();
        let options = GeminiRequestOptions {
            system_instruction: if lower_model.contains("gemma") && !lower_model.contains("gemini")
            {
                SystemInstructionMode::UserPrefix {
                    tag: "[SYSTEM INFO]".into(),
                }
            } else {
                SystemInstructionMode::Native
            },
            send_temperature: enabled.contains(&"temperature"),
            send_max_output_tokens: enabled.contains(&"max_tokens"),
            top_p: if enabled.contains(&"top_p") {
                Some(number("top_p")? as f32)
            } else {
                None
            },
            top_k: if enabled.contains(&"top_k") {
                Some(integer("top_k")?)
            } else {
                None
            },
            thinking,
            structured_output: settings
                .get("support_overrides")
                .and_then(|v| v.get("structured_output"))
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| invalid("invalid structured output support"))
                })
                .transpose()?
                .unwrap_or(true),
        };
        let record = PresetRecord {
            name: text(preset, "name")?.into(),
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                endpoint: endpoint.into(),
                model: model.into(),
                credential: CredentialReference::Local(credential_name.clone()),
                temperature: number("temperature")? as f32,
                max_output_tokens: integer("max_tokens")?,
                timeout_ms: 60_000,
                transforms,
                options,
            }),
        };
        record.validate()?;
        if let PresetDefinition::Gemini(config) = &record.definition {
            config.template(secret)?;
        }
        let id = PresetId(
            id.parse()
                .map_err(|_| invalid("invalid Python preset ID"))?,
        );
        self.records.insert(id, record);
        self.credentials.insert(credential_name, secret.into());
        Ok(())
    }

    pub fn credential_bytes(&self) -> Result<Vec<u8>, PresetCatalogError> {
        serde_json::to_vec_pretty(&self.credentials)
            .map_err(|_| invalid("cannot encode credential store"))
    }

    fn convert_openai(
        &mut self,
        id: &str,
        preset: &Value,
        template: Option<&Value>,
        protocol: &str,
    ) -> Result<(), PresetCatalogError> {
        use super::openai::{JsonOutput, OpenAiPresetDefinition, OpenAiProtocol};
        let protocol = match protocol {
            "openai_compatible_default" => OpenAiProtocol::Compatible,
            "mistral_default" => OpenAiProtocol::Mistral,
            "openrouter_default" => OpenAiProtocol::OpenRouter,
            "aiio_default" => OpenAiProtocol::Aiio,
            "lmstudio_default" => OpenAiProtocol::LmStudio,
            "openai_responses_default" => OpenAiProtocol::Responses,
            _ => return Err(invalid(&format!("unsupported import protocol: {protocol}"))),
        };
        for field in [
            "reserve_keys",
            "fallbacks",
            "generation_overrides",
            "model_profile_overrides",
        ] {
            if has_entries(preset.get(field)) {
                return Err(invalid(&format!("import requires support for {field}")));
            }
        }
        let overrides = preset
            .get("protocol_overrides")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("protocol overrides must be an object"))?;
        if overrides.keys().any(|key| key != "transforms") {
            return Err(invalid("unsupported protocol override"));
        }
        let transforms = match overrides.get("transforms") {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| invalid("invalid message transforms"))?,
            None => protocol.transforms(),
        };
        let settings = preset
            .get("model_settings")
            .ok_or_else(|| invalid("missing model settings"))?;
        let values = settings
            .get("values")
            .ok_or_else(|| invalid("missing model setting values"))?;
        let enabled = settings
            .get("enabled")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("missing enabled model settings"))?
            .iter()
            .map(|v| v.as_str().ok_or_else(|| invalid("invalid enabled setting")))
            .collect::<Result<Vec<_>, _>>()?;
        if enabled.iter().any(|id| {
            ![
                "temperature",
                "max_tokens",
                "top_p",
                "presence_penalty",
                "frequency_penalty",
                "reasoning_enabled",
            ]
            .contains(id)
        }) {
            return Err(invalid("unsupported enabled model setting"));
        }
        let number = |key: &str| {
            values
                .get(key)
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .ok_or_else(|| invalid(&format!("missing number {key}")))
        };
        let optional = |key: &str| {
            if enabled.contains(&key) {
                number(key).map(Some)
            } else {
                Ok(None)
            }
        };
        let mut options = protocol.options();
        options.send_temperature = enabled.contains(&"temperature");
        options.send_max_output_tokens = enabled.contains(&"max_tokens");
        options.top_p = optional("top_p")?;
        options.presence_penalty = optional("presence_penalty")?;
        options.frequency_penalty = optional("frequency_penalty")?;
        if enabled.contains(&"reasoning_enabled") {
            options.reasoning_enabled = Some(
                values
                    .get("reasoning_enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("invalid reasoning_enabled"))?,
            );
        }
        if settings
            .get("support_overrides")
            .and_then(|v| v.get("structured_output"))
            == Some(&Value::Bool(false))
        {
            options.output = JsonOutput::Prompt;
        }
        let endpoint = preset
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                template
                    .and_then(|t| t.get("url"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .ok_or_else(|| invalid("explicit provider endpoint required"))?;
        let key = text(preset, "key")?;
        let credential_name = format!("python_preset_{id}");
        let config = OpenAiPresetDefinition {
            protocol,
            endpoint: endpoint.into(),
            model: text(preset, "default_model")?.into(),
            credential: (!key.is_empty())
                .then(|| CredentialReference::Local(credential_name.clone())),
            temperature: number("temperature")?,
            max_output_tokens: values
                .get("max_tokens")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| invalid("invalid max_tokens"))?,
            timeout_ms: 60_000,
            options,
            transforms,
        };
        super::openai::OpenAiTemplate::new(config.clone(), (!key.is_empty()).then_some(key))?;
        let record = PresetRecord {
            name: text(preset, "name")?.into(),
            definition: PresetDefinition::OpenAi(config),
        };
        let id = PresetId(
            id.parse()
                .map_err(|_| invalid("invalid Python preset ID"))?,
        );
        self.records.insert(id, record);
        if !key.is_empty() {
            self.credentials.insert(credential_name, key.into());
        }
        Ok(())
    }

    pub fn catalog_bytes(&self) -> Result<Vec<u8>, PresetCatalogError> {
        let stored = super::presets::StoredPresets::from_records(self.records.clone())?;
        serde_json::to_vec_pretty(&stored)
            .map_err(|_| invalid("cannot encode imported preset catalog"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn template_protocol_wins_over_stale_preset_field_and_secret_is_separated() {
        let templates = json!({"templates":{"3":{"name":"Google", "protocol_id":"google_gemini_default", "url":"", "default_model":"model"}}});
        let base = json!({"name":"Main","base":3,"protocol_id":"","protocol_overrides":{},"url":"","default_model":"model","key":"private-fixture-key","model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},"enabled":["max_tokens","thinking_level"]}});
        let mut other = base.clone();
        other["protocol_id"] = "openai_compatible_default".into();
        let mut custom = other.clone();
        custom["base"] = Value::Null;
        let plan = PythonPresetImport::parse(
            &serde_json::to_vec(&json!({"presets":{"18":base,"17":other,"20":custom}})).unwrap(),
            &serde_json::to_vec(&templates).unwrap(),
        )
        .unwrap();
        assert_eq!(plan.records.len(), 2);
        assert_eq!(plan.blockers[0].source_id, "20");
        let json = serde_json::to_string(&plan.records).unwrap();
        assert!(!json.contains("private-fixture-key"));
        assert!(json.contains("python_preset_18"));
        assert!(
            String::from_utf8(plan.credential_bytes().unwrap())
                .unwrap()
                .contains("private-fixture-key")
        );
        let PresetDefinition::Gemini(config) = &plan.records[&PresetId(18)].definition else {
            panic!()
        };
        assert!(!config.options.send_temperature);
        assert!(config.options.send_max_output_tokens);
        assert!(matches!(
            config.options.thinking,
            Some(GeminiThinking::Level {
                level: ThinkingLevel::Minimal,
                ..
            })
        ));
    }
}
