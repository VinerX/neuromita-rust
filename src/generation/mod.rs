mod conversation;
pub(crate) use conversation::LlmConversation;
mod credentials;
mod effects;
pub mod gemini;
mod http;
pub mod openai;
mod presets;
pub use credentials::{ApplicationPresetCredentials, CredentialReference, SecretKey};
mod provider;
pub mod python_import;
mod structured;
mod transforms;
pub use effects::ResponseEffect;
pub use presets::{
    EnvironmentPresetCredentials, GeminiPresetDefinition, JsonPresetRepository, PresetCatalog,
    PresetCatalogError, PresetCatalogSnapshot, PresetConnectionEdit, PresetCredentials,
    PresetDefinition, PresetGenerationEdit, PresetId, PresetProtocol, PresetRecord,
    PresetRepository, PresetTemplate,
};
pub use structured::{
    RESPONSE_PROTOCOL_VERSION, ResponseSegment, SegmentIntent, StructuredResponse, ToolCall,
    WorkingState,
};
pub use transforms::{MessageTransform, apply_message_transforms};

pub use provider::{
    GenerationError, GenerationInput, GenerationObserver, GenerationProvider, GenerationRequest,
    PreparedRequest, PromptMessage, PromptRole, ProposedResponse, ProviderFuture, ProviderReply,
    ProviderUsage, ResponseGenerator,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Gemini,
    OpenAi,
}

#[derive(Debug, Clone)]
pub enum ProviderTemplate {
    Gemini(gemini::GeminiTemplate),
    OpenAi(openai::OpenAiTemplate),
}

impl ProviderTemplate {
    pub fn model(&self) -> &str {
        match self {
            Self::Gemini(t) => t.model(),
            Self::OpenAi(t) => t.model(),
        }
    }
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Gemini(_) => ProviderKind::Gemini,
            Self::OpenAi(_) => ProviderKind::OpenAi,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenerationPreset {
    template: ProviderTemplate,
}

impl GenerationPreset {
    pub fn new(template: ProviderTemplate) -> Self {
        Self { template }
    }
    pub fn template(&self) -> &ProviderTemplate {
        &self.template
    }
    pub(crate) fn diagnostic_redaction_values(&self) -> Vec<String> {
        match &self.template {
            ProviderTemplate::Gemini(template) => template.diagnostic_redaction_values(),
            ProviderTemplate::OpenAi(template) => template.diagnostic_redaction_values(),
        }
    }
}
