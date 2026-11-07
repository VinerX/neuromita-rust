use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    identity::{CharacterId, EventId, RequestId},
    timeline::ContextAnchor,
};

use super::domain::{
    AppendGuard, BranchId, CausalLink, Event, EventDraft, EventKind, TimelineError, TimelineFact,
    TimelineRepository,
};

/// A speaker's stable identity, independent of how each listener renders its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSpeaker {
    Player,
    Character(CharacterId),
}

impl ConversationSpeaker {
    fn identity(self) -> String {
        match self {
            Self::Player => "player".to_owned(),
            Self::Character(id) => id.to_string(),
        }
    }
}

/// Routing metadata is distinct from knowledge. Only seen_by controls MitaView.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationAudience {
    pub targets: BTreeSet<ConversationSpeaker>,
    pub participants: BTreeSet<ConversationSpeaker>,
    pub seen_by: BTreeSet<CharacterId>,
}

impl ConversationAudience {
    pub fn private_to(character: CharacterId) -> Self {
        Self {
            seen_by: [character].into(),
            ..Self::default()
        }
    }

    pub fn player_to(character: CharacterId) -> Self {
        Self {
            targets: [ConversationSpeaker::Character(character)].into(),
            participants: [
                ConversationSpeaker::Player,
                ConversationSpeaker::Character(character),
            ]
            .into(),
            seen_by: [character].into(),
        }
    }
}

/// A semantic utterance is an event with an author and separate routing metadata.
/// A recipient is not necessarily a witness; witnessing never follows causal links.
pub trait Message: Event {
    fn speaker(&self) -> ConversationSpeaker;
    fn targets(&self) -> &BTreeSet<ConversationSpeaker>;
    fn participants(&self) -> &BTreeSet<ConversationSpeaker>;
    fn content(&self) -> &Value;
}

/// One conversation message, regardless of whether player, character or generation produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub id: EventId,
    pub message_id: String,
    pub speaker: ConversationSpeaker,
    pub audience: ConversationAudience,
    pub content: Value,
    pub links: Vec<CausalLink>,
    pub occurred_at_unix_ms: u64,
    pub request_id: Option<RequestId>,
    pub reply_to: Option<EventId>,
    pub context: Option<ContextAnchor>,
}

impl Event for ConversationMessage {
    fn event_id(&self) -> EventId {
        self.id
    }
    fn kind(&self) -> EventKind {
        match self.speaker {
            ConversationSpeaker::Player => EventKind::PlayerMessage,
            ConversationSpeaker::Character(_) => EventKind::CharacterMessage,
        }
    }
    fn witnesses(&self) -> BTreeSet<CharacterId> {
        let mut witnesses = self.audience.seen_by.clone();
        if let ConversationSpeaker::Character(character) = self.speaker {
            witnesses.insert(character);
        }
        witnesses
    }
    fn links(&self) -> &[CausalLink] {
        &self.links
    }
}

impl Message for ConversationMessage {
    fn speaker(&self) -> ConversationSpeaker {
        self.speaker
    }
    fn targets(&self) -> &BTreeSet<ConversationSpeaker> {
        &self.audience.targets
    }
    fn participants(&self) -> &BTreeSet<ConversationSpeaker> {
        &self.audience.participants
    }
    fn content(&self) -> &Value {
        &self.content
    }
}

pub(crate) fn spoken_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(segments) => {
            let parts = segments
                .iter()
                .map(|segment| segment.get("text").and_then(Value::as_str))
                .collect::<Option<Vec<_>>>()?;
            Some(
                parts
                    .iter()
                    .map(|text| text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        _ => None,
    }
}

impl ConversationMessage {
    pub fn new(
        message_id: impl Into<String>,
        speaker: ConversationSpeaker,
        content: Value,
        seen_by: impl IntoIterator<Item = CharacterId>,
    ) -> Self {
        let kind = match speaker {
            ConversationSpeaker::Player => EventKind::PlayerMessage,
            ConversationSpeaker::Character(_) => EventKind::CharacterMessage,
        };
        let draft = EventDraft::new(kind);
        Self {
            id: draft.id,
            message_id: message_id.into(),
            speaker,
            audience: ConversationAudience {
                seen_by: seen_by.into_iter().collect(),
                ..ConversationAudience::default()
            },
            content,
            links: Vec::new(),
            occurred_at_unix_ms: draft.occurred_at_unix_ms,
            request_id: None,
            reply_to: None,
            context: None,
        }
    }

    pub(crate) fn into_draft(self) -> Result<EventDraft, TimelineError> {
        if self.message_id.is_empty()
            || self.message_id.trim() != self.message_id
            || self.message_id.len() > 256
        {
            return Err(TimelineError::Invalid(
                "invalid conversation message identity",
            ));
        }
        if self.content.is_null() {
            return Err(TimelineError::Invalid(
                "conversation message requires content",
            ));
        }
        let kind = self.kind();
        let mut witnesses = self.audience.seen_by;
        if let ConversationSpeaker::Character(character) = self.speaker {
            witnesses.insert(character);
        }
        if witnesses.is_empty() {
            return Err(TimelineError::Invalid(
                "conversation message requires witnesses",
            ));
        }
        let mut draft = EventDraft::new(kind);
        draft.id = self.id;
        draft.actor = Some(self.speaker.identity());
        draft.observers = witnesses;
        draft.external_message_id = Some(self.message_id.clone());
        draft.links = self.links;
        draft.occurred_at_unix_ms = self.occurred_at_unix_ms;
        draft
            .payload
            .insert("message_id".into(), json!(self.message_id));
        draft.payload.insert("content".into(), self.content.clone());
        draft.payload.insert("message".into(), json!({
            "content": self.content,
            "targets": self.audience.targets.into_iter().map(ConversationSpeaker::identity).collect::<Vec<_>>(),
            "participants": self.audience.participants.into_iter().map(ConversationSpeaker::identity).collect::<Vec<_>>(),
        }));
        if let Some(request_id) = self.request_id {
            draft
                .payload
                .insert("request_id".into(), json!(request_id.to_string()));
        }
        if let Some(reply_to) = self.reply_to {
            draft
                .payload
                .insert("reply_to".into(), json!(reply_to.0.to_string()));
        }
        if let Some(context) = self.context {
            draft.payload.insert("context".into(), json!(context));
        }
        Ok(draft)
    }
}

/// Commit a single canonical message. Replayed identities cannot change the
/// original audience, payload, author, source time or causal links.
pub fn append_conversation_message(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
    expected_head: u64,
    guard: AppendGuard,
    message: ConversationMessage,
) -> Result<TimelineFact, TimelineError> {
    let timestamp = message.occurred_at_unix_ms;
    let draft = message.into_draft()?;
    let key = format!(
        "conversation-message:{}",
        draft.external_message_id.as_deref().unwrap()
    );
    let facts = repository.append_guarded(
        branch,
        expected_head,
        &key,
        json!({"source_time": timestamp}),
        vec![draft],
        guard,
    )?;
    facts.into_iter().next().ok_or(TimelineError::Invalid(
        "conversation message must commit one fact",
    ))
}
