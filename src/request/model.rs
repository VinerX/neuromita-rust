use std::{fmt, sync::Arc};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    character::{AcceptedResponse, CharacterId},
    finetune::SampleId,
    game_context::GameRuntimeContext,
    generation::GenerationError,
    identity::RequestId,
    interaction::TurnIntent,
    timeline::{ContextPosition, EventId, conversation::ConversationAudience},
    voice::VoiceError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RequestSource {
    Presenter,
    Scheduler,
    Client(Uuid),
    Game(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    User,
    Shutdown,
    SelectionChanged,
    ContextChanged,
    QueueDrain,
}

pub trait GenerationCommand: Send + Sync + fmt::Debug {
    fn character_id(&self) -> CharacterId;
    fn guidance(&self) -> &str;
    fn source(&self) -> RequestSource;
    fn intent(&self) -> TurnIntent;
    fn at(&self) -> ContextPosition;
    fn audience(&self) -> &ConversationAudience;
    fn game_context(&self) -> Option<&GameRuntimeContext> {
        None
    }
}

#[derive(Debug)]
pub struct ReplyRequest {
    character_id: CharacterId,
    message: EventId,
    source: RequestSource,
    at: ContextPosition,
    audience: ConversationAudience,
    game_context: Option<Arc<GameRuntimeContext>>,
}

impl ReplyRequest {
    pub fn new(character_id: CharacterId, message: EventId, source: RequestSource) -> Self {
        Self {
            character_id,
            message,
            source,
            at: ContextPosition::Latest,
            audience: ConversationAudience::private_to(character_id),
            game_context: None,
        }
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.at = at;
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.audience = audience;
        self
    }
    pub fn with_game_context(mut self, context: Arc<GameRuntimeContext>) -> Self {
        self.game_context = Some(context);
        self
    }
}

impl GenerationCommand for ReplyRequest {
    fn character_id(&self) -> CharacterId {
        self.character_id
    }
    fn guidance(&self) -> &str {
        ""
    }
    fn source(&self) -> RequestSource {
        self.source
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::Reply(self.message)
    }
    fn at(&self) -> ContextPosition {
        self.at
    }
    fn audience(&self) -> &ConversationAudience {
        &self.audience
    }
    fn game_context(&self) -> Option<&GameRuntimeContext> {
        self.game_context.as_deref()
    }
}

#[derive(Debug)]
pub struct ReactRequest {
    character_id: CharacterId,
    event: EventId,
    guidance: Arc<str>,
    source: RequestSource,
    at: ContextPosition,
    audience: ConversationAudience,
    game_context: Option<Arc<GameRuntimeContext>>,
}
impl ReactRequest {
    pub fn new(
        character: CharacterId,
        event: EventId,
        instruction: impl Into<Arc<str>>,
        source: RequestSource,
    ) -> Self {
        Self {
            character_id: character,
            event,
            guidance: instruction.into(),
            source,
            at: ContextPosition::Latest,
            audience: ConversationAudience::private_to(character),
            game_context: None,
        }
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.at = at;
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.audience = audience;
        self
    }
    pub fn with_game_context(mut self, context: Arc<GameRuntimeContext>) -> Self {
        self.game_context = Some(context);
        self
    }
}
impl GenerationCommand for ReactRequest {
    fn character_id(&self) -> CharacterId {
        self.character_id
    }
    fn guidance(&self) -> &str {
        &self.guidance
    }
    fn source(&self) -> RequestSource {
        self.source
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::React(self.event)
    }
    fn at(&self) -> ContextPosition {
        self.at
    }
    fn audience(&self) -> &ConversationAudience {
        &self.audience
    }
    fn game_context(&self) -> Option<&GameRuntimeContext> {
        self.game_context.as_deref()
    }
}

#[derive(Debug)]
pub struct InitiateRequest {
    character_id: CharacterId,
    guidance: Arc<str>,
    source: RequestSource,
    at: ContextPosition,
    audience: ConversationAudience,
    game_context: Option<Arc<GameRuntimeContext>>,
}
impl InitiateRequest {
    pub fn new(
        character: CharacterId,
        instruction: impl Into<Arc<str>>,
        source: RequestSource,
    ) -> Self {
        Self {
            character_id: character,
            guidance: instruction.into(),
            source,
            at: ContextPosition::Latest,
            audience: ConversationAudience::private_to(character),
            game_context: None,
        }
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.at = at;
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.audience = audience;
        self
    }
    pub fn with_game_context(mut self, context: Arc<GameRuntimeContext>) -> Self {
        self.game_context = Some(context);
        self
    }
}
impl GenerationCommand for InitiateRequest {
    fn character_id(&self) -> CharacterId {
        self.character_id
    }
    fn guidance(&self) -> &str {
        &self.guidance
    }
    fn source(&self) -> RequestSource {
        self.source
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::Initiate
    }
    fn at(&self) -> ContextPosition {
        self.at
    }
    fn audience(&self) -> &ConversationAudience {
        &self.audience
    }
    fn game_context(&self) -> Option<&GameRuntimeContext> {
        self.game_context.as_deref()
    }
}

#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub id: RequestId,
    pub character_id: CharacterId,
    pub source: RequestSource,
    pub intent: TurnIntent,
}

#[derive(Debug, Clone)]
pub enum RequestOutcome {
    Success(AcceptedResponse),
    TextGenerationFailed(GenerationError),
    VoiceGenerationFailed {
        response: AcceptedResponse,
        error: VoiceError,
    },
    InternalError(InternalFailure),
}

#[derive(Debug, Clone)]
pub struct InternalFailure {
    pub message: Arc<str>,
    pub partial_text: Option<Arc<str>>,
}
impl fmt::Display for InternalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl From<Arc<str>> for InternalFailure {
    fn from(message: Arc<str>) -> Self {
        Self {
            message,
            partial_text: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProcessingStage {
    GeneratingText,
    TextReady { text: Arc<str> },
    GeneratingVoice { text: Arc<str> },
    Accepting { text: Arc<str> },
}

#[derive(Debug, Clone)]
pub enum RequestState {
    Pending,
    Processing(ProcessingStage),
    Finished(RequestOutcome),
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct DatasetCapture {
    pub sample_id: Option<SampleId>,
    pub warning: Option<Arc<str>>,
}

impl RequestState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished(_) | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct RequestSnapshot {
    pub metadata: RequestMetadata,
    pub revision: u64,
    pub state: RequestState,
    pub dataset: DatasetCapture,
    pub cancel_reason: Option<CancelReason>,
}

impl RequestSnapshot {
    pub fn provisional_text(&self) -> Option<&str> {
        match &self.state {
            RequestState::Processing(
                ProcessingStage::TextReady { text }
                | ProcessingStage::GeneratingVoice { text }
                | ProcessingStage::Accepting { text },
            ) => Some(text),
            _ => None,
        }
    }

    pub fn can_cancel(&self) -> bool {
        matches!(
            self.state,
            RequestState::Pending
                | RequestState::Processing(
                    ProcessingStage::GeneratingText
                        | ProcessingStage::TextReady { .. }
                        | ProcessingStage::GeneratingVoice { .. }
                )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EffectPhase {
    Working,
    Accepting,
    Applied,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    Cancelled,
    Requested,
    TooLate,
    AlreadyFinished,
}

#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("ChatApi is not initialized")]
    Uninitialized,
    #[error("application is stopping")]
    Stopped,
    #[error("request queue is full")]
    QueueFull,
    #[error("request queue is closed")]
    QueueClosed,
    #[error("character is unavailable")]
    CharacterUnavailable,
    #[error("timeline could not accept the request input")]
    TimelineUnavailable,
    #[error("target event is not observable or not a message when replying")]
    InvalidTargetEvent,
}

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("request observation closed before completion")]
    ClosedBeforeCompletion,
}
