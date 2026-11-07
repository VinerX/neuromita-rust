use super::{
    CredentialReference, GenerationError, GenerationInput, GenerationObserver, GenerationProvider,
    MessageTransform, PreparedRequest, PromptMessage, PromptRole, ProviderFuture, ProviderKind,
    ProviderTemplate, StructuredResponse, apply_message_transforms,
    conversation::{LlmConversation, LlmRole},
    http::{ResponseMetadata, send_json},
    structured::gemini_message_schema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiProtocol {
    Compatible,
    Mistral,
    OpenRouter,
    Aiio,
    LmStudio,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonOutput {
    Schema,
    Object,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiOptions {
    pub send_temperature: bool,
    pub send_max_output_tokens: bool,
    pub top_p: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub output: JsonOutput,
    pub reasoning_enabled: Option<bool>,
}

impl OpenAiProtocol {
    pub fn id(self) -> &'static str {
        match self {
            Self::Compatible => "openai_compatible_default",
            Self::Mistral => "mistral_default",
            Self::OpenRouter => "openrouter_default",
            Self::Aiio => "aiio_default",
            Self::LmStudio => "lmstudio_default",
            Self::Responses => "openai_responses_default",
        }
    }
    pub fn template_id(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Mistral => "mistral",
            Self::OpenRouter => "openrouter",
            Self::Aiio => "aiio",
            Self::LmStudio => "lmstudio",
            Self::Responses => "openai",
        }
    }
    pub fn dialect(self) -> &'static str {
        if self == Self::Responses {
            "openai_responses"
        } else {
            "openai_chat_completions"
        }
    }

    pub fn options(self) -> OpenAiOptions {
        OpenAiOptions {
            send_temperature: true,
            send_max_output_tokens: true,
            top_p: None,
            presence_penalty: None,
            frequency_penalty: None,
            output: match self {
                Self::Aiio => JsonOutput::Prompt,
                Self::OpenRouter => JsonOutput::Object,
                _ => JsonOutput::Schema,
            },
            reasoning_enabled: None,
        }
    }
    pub fn transforms(self) -> Vec<MessageTransform> {
        match self {
            Self::Mistral => vec![
                MessageTransform::EnsureAlternatingRoles,
                MessageTransform::EnsureLastMessageUser {
                    fallback_user_text: ".".into(),
                },
            ],
            Self::Aiio => vec![MessageTransform::SystemToUserPrefix {
                tag: "[SYSTEM CONTEXT]".into(),
            }],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiPresetDefinition {
    pub protocol: OpenAiProtocol,
    pub endpoint: String,
    pub model: String,
    pub credential: Option<CredentialReference>,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
    pub options: OpenAiOptions,
    pub transforms: Vec<MessageTransform>,
}

#[derive(Clone)]
pub struct OpenAiTemplate {
    definition: OpenAiPresetDefinition,
    endpoint: reqwest::Url,
    key: Option<Arc<str>>,
}
impl fmt::Debug for OpenAiTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiTemplate")
            .field("definition", &self.definition)
            .field("key", &"[REDACTED]")
            .finish()
    }
}
impl OpenAiTemplate {
    pub fn new(
        definition: OpenAiPresetDefinition,
        key: Option<&str>,
    ) -> Result<Self, super::PresetCatalogError> {
        let invalid = |reason: &str| super::PresetCatalogError::Invalid(reason.into());
        let endpoint = reqwest::Url::parse(&definition.endpoint)
            .map_err(|_| invalid("invalid provider endpoint"))?;
        let local = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if (endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && local))
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid(
                "endpoint requires HTTPS or loopback HTTP, without credentials/query/fragment",
            ));
        }
        if definition.model.is_empty()
            || definition.model.len() > 256
            || definition
                .model
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(invalid("invalid model identifier"));
        }
        if definition.credential.is_some() != key.is_some()
            || key.is_some_and(|k| {
                k.is_empty() || k.chars().any(|c| c.is_whitespace() || c.is_control())
            })
        {
            return Err(invalid(
                "explicit authentication configuration does not match credential",
            ));
        }
        if let Some(reference) = &definition.credential {
            reference.validate()?;
        }
        super::gemini::GenerationSettings::new(
            definition.temperature,
            definition.max_output_tokens,
            Duration::from_millis(definition.timeout_ms),
        )
        .map_err(|error| invalid(&error.to_string()))?;
        let options = &definition.options;
        if options
            .top_p
            .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
            || [options.presence_penalty, options.frequency_penalty]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite() || !(-2.0..=2.0).contains(&v))
        {
            return Err(invalid("invalid sampling options"));
        }
        if options.reasoning_enabled.is_some() && definition.protocol != OpenAiProtocol::OpenRouter
        {
            return Err(invalid("reasoning_enabled belongs to OpenRouter"));
        }
        if definition.protocol == OpenAiProtocol::Responses
            && (options.presence_penalty.is_some() || options.frequency_penalty.is_some())
        {
            return Err(invalid(
                "penalties are not supported by the Responses codec",
            ));
        }
        Ok(Self {
            definition,
            endpoint,
            key: key.map(Arc::from),
        })
    }
    pub fn model(&self) -> &str {
        &self.definition.model
    }
    pub(crate) fn diagnostic_redaction_values(&self) -> Vec<String> {
        self.key.iter().map(ToString::to_string).collect()
    }
}

pub struct OpenAiProvider {
    client: reqwest::Client,
}
impl OpenAiProvider {
    pub fn new() -> Result<Self, GenerationError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| GenerationError::Transport(error.without_url().to_string()))?;
        Ok(Self { client })
    }
    async fn execute(
        &self,
        input: GenerationInput,
        template: OpenAiTemplate,
        observer: &mut dyn GenerationObserver,
    ) -> Result<StructuredResponse, GenerationError> {
        let conversation =
            LlmConversation::from_context_with_memory(&input.context, &input.prepared_knowledge);
        let mut prompt = vec![PromptMessage {
            role: PromptRole::System,
            text: conversation.system_instruction.into(),
        }];
        prompt.extend(conversation.messages.into_iter().map(|m| PromptMessage {
            role: match m.role {
                LlmRole::User => PromptRole::User,
                LlmRole::Assistant => PromptRole::Assistant,
            },
            text: m.text.into(),
        }));
        let (prompt, body) = encode(prompt, &template.definition);
        let payload = serde_json::to_string(&body)
            .map_err(|_| GenerationError::Response("cannot encode provider request".into()))?;
        let config = &template.definition;
        observer.request_prepared(PreparedRequest {
            provider: ProviderKind::OpenAi,
            protocol: config.protocol.id().into(),
            dialect: config.protocol.dialect().into(),
            model: config.model.as_str().into(),
            endpoint: template.endpoint.as_str().into(),
            temperature: config
                .options
                .send_temperature
                .then_some(config.temperature),
            max_output_tokens: config
                .options
                .send_max_output_tokens
                .then_some(config.max_output_tokens),
            prompt: prompt.into(),
            payload_json: payload.as_str().into(),
            captured_at: SystemTime::now(),
        });
        let mut request = self
            .client
            .post(template.endpoint.clone())
            .timeout(Duration::from_millis(config.timeout_ms))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload);
        if let Some(key) = &template.key {
            request = request.bearer_auth(key.as_ref());
        }
        if config.protocol == OpenAiProtocol::OpenRouter {
            request = request
                .header("HTTP-Referer", "https://github.com/Atm4x/NeuroMita")
                .header("X-Title", "NeuroMita");
        }
        let bytes = send_json(request, observer, |bytes| observe(bytes, config.protocol)).await?;
        let result = decode(&bytes, config.protocol);
        if let Err(error) = &result {
            observer.failed(error);
        }
        result
    }
}

impl GenerationProvider for OpenAiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }
    fn generate(&self, input: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            self.generate_observed(input, template, &mut NoopObserver)
                .await
        })
    }
    fn generate_observed<'a>(
        &'a self,
        input: GenerationInput,
        template: ProviderTemplate,
        observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            match template {
                ProviderTemplate::OpenAi(template) => self.execute(input, template, observer).await,
                other => Err(GenerationError::Unavailable(other.kind())),
            }
        })
    }
}
struct NoopObserver;
impl GenerationObserver for NoopObserver {}

fn role(role: PromptRole) -> &'static str {
    match role {
        PromptRole::System => "system",
        PromptRole::User => "user",
        PromptRole::Assistant => "assistant",
    }
}

fn encode(
    mut prompt: Vec<PromptMessage>,
    config: &OpenAiPresetDefinition,
) -> (Vec<PromptMessage>, Value) {
    if config.options.output != JsonOutput::Schema {
        prompt.insert(
            0,
            PromptMessage {
                role: PromptRole::System,
                text: format!(
                    "Return only JSON conforming to this response schema:\n{}",
                    gemini_message_schema()
                )
                .into(),
            },
        );
    }
    let prompt = apply_message_transforms(prompt, &config.transforms);
    let responses = config.protocol == OpenAiProtocol::Responses;
    let messages = prompt.iter().map(|m| if responses { json!({"role":role(m.role),"content":[{"type": if m.role == PromptRole::Assistant {"output_text"} else {"input_text"},"text":m.text}]}) } else { json!({"role":role(m.role),"content":m.text}) }).collect::<Vec<_>>();
    let mut body = json!({"model":config.model,"stream":false});
    body[if responses { "input" } else { "messages" }] = messages.into();
    let options = &config.options;
    if options.send_temperature {
        body["temperature"] = config.temperature.into();
    }
    if options.send_max_output_tokens {
        body[if responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        }] = config.max_output_tokens.into();
    }
    if let Some(value) = options.top_p {
        body["top_p"] = value.into();
    }
    if let Some(value) = options.presence_penalty {
        body["presence_penalty"] = value.into();
    }
    if let Some(value) = options.frequency_penalty {
        body["frequency_penalty"] = value.into();
    }
    if let Some(value) = options.reasoning_enabled {
        body["reasoning"] = json!({"enabled":value});
    }
    let format = match options.output {
        JsonOutput::Schema => Some(if responses {
            json!({"type":"json_schema","name":"neuromita_response","schema":gemini_message_schema(),"strict":false})
        } else {
            json!({"type":"json_schema","json_schema":{"name":"neuromita_response","schema":gemini_message_schema(),"strict":false}})
        }),
        JsonOutput::Object => Some(json!({"type":"json_object"})),
        JsonOutput::Prompt => None,
    };
    if let Some(format) = format {
        if responses {
            body["text"] = json!({"format":format});
        } else {
            body["response_format"] = format;
        }
    }
    (prompt, body)
}

fn observe(bytes: &[u8], protocol: OpenAiProtocol) -> ResponseMetadata {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return ResponseMetadata::default();
    };
    let responses = protocol == OpenAiProtocol::Responses;
    let answer = if responses {
        body["output"].as_array().map(|items| {
            items
                .iter()
                .filter(|item| item["type"] == "message" && item["role"] == "assistant")
                .filter_map(|item| item["content"].as_array())
                .flatten()
                .filter(|part| part["type"] == "output_text")
                .filter_map(|part| part["text"].as_str())
                .collect::<String>()
        })
    } else {
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_owned)
    };
    ResponseMetadata {
        answer_text: answer.map(Arc::from),
        finish_reason: (if responses {
            body["status"].as_str()
        } else {
            body["choices"][0]["finish_reason"].as_str()
        })
        .map(Arc::from),
        usage: body.get("usage").map(|usage| super::ProviderUsage {
            input_tokens: usage[if responses {
                "input_tokens"
            } else {
                "prompt_tokens"
            }]
            .as_u64(),
            output_tokens: usage[if responses {
                "output_tokens"
            } else {
                "completion_tokens"
            }]
            .as_u64(),
            total_tokens: usage["total_tokens"].as_u64(),
        }),
    }
}

fn decode(bytes: &[u8], protocol: OpenAiProtocol) -> Result<StructuredResponse, GenerationError> {
    let body: Value = serde_json::from_slice(bytes)
        .map_err(|_| GenerationError::Response("provider returned invalid JSON".into()))?;
    let metadata = observe(bytes, protocol);
    let valid = if protocol == OpenAiProtocol::Responses {
        body["status"] == "completed" && body.get("error").is_none_or(Value::is_null)
    } else {
        body["choices"]
            .as_array()
            .is_some_and(|choices| choices.len() == 1)
            && body["choices"][0]["finish_reason"] == "stop"
            && body["choices"][0]["message"]
                .get("refusal")
                .is_none_or(Value::is_null)
    };
    let unsupported_calls = if protocol == OpenAiProtocol::Responses {
        body["output"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                (item["type"] != "message" && item["type"] != "reasoning")
                    || item["content"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|part| part["type"] != "output_text"))
            })
        })
    } else {
        body["choices"][0]["message"]["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
    };
    if !valid || unsupported_calls {
        return Err(GenerationError::Response(
            "provider did not complete one answer successfully".into(),
        ));
    }
    let text = metadata
        .answer_text
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| GenerationError::Response("provider has no answer text".into()))?;
    StructuredResponse::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_refused_tool_and_incomplete_outputs_are_rejected() {
        let answer = r#"{"segments":[{"text":"answer"}]}"#;
        for body in [
            json!({"choices":[{"finish_reason":"length","message":{"content":answer}}]}),
            json!({"choices":[{"finish_reason":"stop","message":{"content":answer,"refusal":"refused"}}]}),
            json!({"choices":[{"finish_reason":"stop","message":{"content":answer,"tool_calls":[{"id":"unexpected"}]}}]}),
        ] {
            assert!(
                decode(
                    &serde_json::to_vec(&body).unwrap(),
                    OpenAiProtocol::Compatible
                )
                .is_err()
            );
        }
        for body in [
            json!({"status":"incomplete","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}]}),
            json!({"status":"completed","output":[{"type":"function_call","name":"unexpected"}]}),
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"refused"}]}]}),
        ] {
            assert!(
                decode(
                    &serde_json::to_vec(&body).unwrap(),
                    OpenAiProtocol::Responses
                )
                .is_err()
            );
        }
    }
}
