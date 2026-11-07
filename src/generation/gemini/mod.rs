mod options;
mod template;
pub use options::{GeminiRequestOptions, GeminiThinking, SystemInstructionMode, ThinkingLevel};
mod wire;

pub use template::{GeminiTemplate, GenerationSettings, PresetError};

use std::time::SystemTime;

use super::provider::GenerationInput;
use super::{
    GenerationError, GenerationObserver, GenerationProvider, PreparedRequest, ProviderFuture,
    ProviderKind, ProviderTemplate,
};

pub struct GeminiProvider {
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new() -> Result<Self, GenerationError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| GenerationError::Transport(error.without_url().to_string()))?;
        Ok(Self { client })
    }

    async fn execute(
        &self,
        context: GenerationInput,
        template: GeminiTemplate,
        observer: &mut dyn GenerationObserver,
    ) -> Result<super::StructuredResponse, GenerationError> {
        let encoded =
            wire::encode_observed(context.context, &template, &context.prepared_knowledge);
        let payload_json = serde_json::to_string(&encoded.body).map_err(|error| {
            GenerationError::Response(format!("cannot encode provider request: {error}"))
        })?;
        let mut endpoint = template.request_url();
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        observer.request_prepared(PreparedRequest {
            protocol: "google_gemini_default".into(),
            dialect: "gemini_generate_content".into(),
            provider: ProviderKind::Gemini,
            model: template.model().into(),
            endpoint: endpoint.as_str().into(),
            temperature: template
                .options()
                .send_temperature
                .then(|| template.settings().temperature()),
            max_output_tokens: template
                .options()
                .send_max_output_tokens
                .then(|| template.settings().max_output_tokens()),
            prompt: encoded.prompt.into(),
            payload_json: payload_json.as_str().into(),
            captured_at: SystemTime::now(),
        });
        let bytes = super::http::send_json(
            self.client
                .post(template.request_url())
                .timeout(template.settings().timeout())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(payload_json),
            observer,
            wire::observation_metadata,
        )
        .await?;
        match wire::decode(&bytes) {
            Ok(response) => Ok(response),
            Err(error) => {
                observer.failed(&error);
                Err(error)
            }
        }
    }
}

impl GenerationProvider for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            match template {
                ProviderTemplate::Gemini(template) => {
                    self.execute(context, template, &mut NoopObserver).await
                }
                other => Err(GenerationError::Unavailable(other.kind())),
            }
        })
    }

    fn generate_observed<'a>(
        &'a self,
        context: GenerationInput,
        template: ProviderTemplate,
        observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            match template {
                ProviderTemplate::Gemini(template) => {
                    self.execute(context, template, observer).await
                }
                other => Err(GenerationError::Unavailable(other.kind())),
            }
        })
    }
}

struct NoopObserver;
impl GenerationObserver for NoopObserver {}
