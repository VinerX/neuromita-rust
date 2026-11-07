use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use thiserror::Error;

use super::{GenerationPreset, ProviderKind, ProviderTemplate, StructuredResponse};
use crate::character::CharacterContext;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum GenerationError {
    #[error("provider is not registered: {0:?}")]
    Unavailable(ProviderKind),
    #[error("provider is registered twice: {0:?}")]
    Duplicate(ProviderKind),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("provider returned HTTP {0}")]
    Http(u16),
    #[error("invalid provider response: {0}")]
    Response(String),
}

pub struct GenerationRequest {
    pub context: CharacterContext,
    pub preset: GenerationPreset,
    pub prepared_knowledge: Option<String>,
}

impl GenerationRequest {
    pub fn new(
        context: CharacterContext,
        preset: GenerationPreset,
        prepared_knowledge: Option<String>,
    ) -> Self {
        Self {
            context,
            preset,
            prepared_knowledge,
        }
    }
}

pub use crate::turn::TurnProposal as ProposedResponse;

/// Providers consume an immutable character snapshot and already selected knowledge.
/// No provider performs retrieval or chooses between semantic and lexical modes.
#[derive(Clone)]
pub struct GenerationInput {
    pub context: CharacterContext,
    pub prepared_knowledge: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub text: Arc<str>,
}

/// Provider-reported token counts. Missing values stay missing; this type is
/// never populated from a local estimate or a previous response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PreparedRequest {
    pub protocol: Arc<str>,
    pub dialect: Arc<str>,
    pub provider: ProviderKind,
    pub model: Arc<str>,
    /// Endpoint without credentials, query or fragment.
    pub endpoint: Arc<str>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    /// The provider-neutral prompt used to produce `payload_json`.
    pub prompt: Arc<[PromptMessage]>,
    /// The exact JSON bytes sent as the request body.
    pub payload_json: Arc<str>,
    pub captured_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct ProviderReply {
    pub status: Option<u16>,
    /// Body received before provider decoding or application validation.
    pub raw_body: Arc<str>,
    /// Provider answer text before application validation, excluding thought parts.
    pub answer_text: Option<Arc<str>>,
    /// False when transport stopped or the bounded response limit was hit.
    pub complete: bool,
    pub finish_reason: Option<Arc<str>>,
    pub usage: Option<ProviderUsage>,
    pub elapsed: Duration,
}

/// Best-effort synchronous observation at the provider boundary. Observer
/// failures are not represented here and cannot change the generation result.
pub trait GenerationObserver: Send {
    fn request_prepared(&mut self, _request: PreparedRequest) {}
    fn response_received(&mut self, _response: ProviderReply) {}
    fn failed(&mut self, _error: &GenerationError) {}
}

impl std::ops::Deref for GenerationInput {
    type Target = CharacterContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl ProposedResponse {
    pub fn from_structured(structured: StructuredResponse) -> Result<Self, GenerationError> {
        structured.validate()?;
        structured.ensure_supported_effects()?;
        let text = structured.full_text();
        let effects = structured.proposed_effects()?;
        let segments = structured
            .segments
            .iter()
            .map(crate::turn::ResponseSegment::sanitized_for_persistence)
            .collect();
        Ok(Self::validated(text, segments, effects))
    }
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StructuredResponse, GenerationError>> + Send + 'a>>;

pub trait GenerationProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_>;

    fn generate_observed<'a>(
        &'a self,
        context: GenerationInput,
        template: ProviderTemplate,
        _observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        self.generate(context, template)
    }
}

pub struct ResponseGenerator {
    providers: HashMap<ProviderKind, Arc<dyn GenerationProvider>>,
}

impl ResponseGenerator {
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn GenerationProvider>>,
    ) -> Result<Self, GenerationError> {
        let mut registered = HashMap::new();
        for provider in providers {
            let kind = provider.kind();
            if registered.insert(kind, provider).is_some() {
                return Err(GenerationError::Duplicate(kind));
            }
        }
        Ok(Self {
            providers: registered,
        })
    }

    pub async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<ProposedResponse, GenerationError> {
        self.generate_observed(request, &mut NoopObserver).await
    }

    pub async fn generate_observed(
        &self,
        request: GenerationRequest,
        observer: &mut dyn GenerationObserver,
    ) -> Result<ProposedResponse, GenerationError> {
        let template = request.preset.template().clone();
        let kind = template.kind();
        let provider = self
            .providers
            .get(&kind)
            .ok_or(GenerationError::Unavailable(kind))?;
        let input = GenerationInput {
            context: request.context,
            prepared_knowledge: request.prepared_knowledge,
        };
        let structured = provider
            .generate_observed(input, template, observer)
            .await?;
        match ProposedResponse::from_structured(structured) {
            Ok(response) => Ok(response),
            Err(error) => {
                observer.failed(&error);
                Err(error)
            }
        }
    }
}

struct NoopObserver;

impl GenerationObserver for NoopObserver {}
