use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::identity::{CharacterId, EventId, RequestId};

use super::{
    character_view,
    model::{EventKind, KnownEvent, TimelineFact},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// The character's semantic message; the message fields are application data,
/// not a provider-specific projection or another authoritative event store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMessage {
    pub event_id: EventId,
    pub request_id: Option<RequestId>,
    pub kind: EventKind,
    pub message_id: String,
    pub external_message_id: Option<String>,
    pub sequence: u64,
    pub learned_at: u64,
    pub role: MessageRole,
    pub speaker: Option<String>,
    pub content: String,
    pub fields: Map<String, Value>,
}

fn record(known: &KnownEvent, character: CharacterId) -> Option<ProjectedMessage> {
    let fact = &known.fact;
    let draft = &fact.draft;
    let role = match draft.kind {
        EventKind::PlayerMessage | EventKind::CharacterObservation => MessageRole::User,
        EventKind::CharacterMessage => {
            if draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                == Some(character)
            {
                MessageRole::Assistant
            } else {
                MessageRole::User
            }
        }
        EventKind::SessionNote | EventKind::ReminderToolResult => MessageRole::System,
        _ => return None,
    };
    let mut fields = draft
        .payload
        .get("message")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(content) = draft.payload.get("content") {
        fields.insert("content".into(), content.clone());
    }
    let message_id = draft
        .payload
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or_default();
    let message_id = if message_id.is_empty() {
        draft.id.0.to_string()
    } else {
        message_id.to_owned()
    };
    let speaker = if draft.kind == EventKind::CharacterObservation {
        Some("World".to_string())
    } else {
        draft.actor.clone()
    };
    let content = fields
        .get("content")
        .and_then(crate::timeline::conversation::spoken_text)
        .unwrap_or_default();
    Some(ProjectedMessage {
        event_id: draft.id,
        request_id: draft
            .payload
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(RequestId),
        kind: draft.kind,
        message_id,
        external_message_id: draft.external_message_id.clone(),
        sequence: fact.sequence,
        learned_at: known.learned_at,
        role,
        speaker,
        content,
        fields,
    })
}

/// A deterministic fold of only the knowledge available to one character.
/// This is a disposable read model, never an authority for world state.
#[derive(Debug, Clone)]
pub struct CharacterMessageProjection {
    character: CharacterId,
    seen: HashSet<EventId>,
    known: HashMap<EventId, ProjectedMessage>,
    visible: HashMap<EventId, ProjectedMessage>,
}

impl CharacterMessageProjection {
    pub fn new(character: CharacterId) -> Self {
        Self {
            character,
            seen: HashSet::new(),
            known: HashMap::new(),
            visible: HashMap::new(),
        }
    }

    pub fn observe(&mut self, item: &KnownEvent) {
        let draft = &item.fact.draft;
        let direct =
            draft.observers.contains(&self.character) && item.learned_at == item.fact.sequence;
        if direct && draft.kind == EventKind::CharacterContextReset {
            self.seen.clear();
            self.known.clear();
            self.visible.clear();
        }
        if !self.seen.insert(draft.id) {
            return;
        }
        if let Some(entry) = record(item, self.character) {
            self.known.insert(draft.id, entry.clone());
            self.visible.insert(draft.id, entry);
        } else if direct && draft.kind == EventKind::MessageHidden {
            if let Some(ids) = draft.payload.get("event_ids").and_then(Value::as_array) {
                for id in ids {
                    if let Some(id) = id.as_str().and_then(|id| uuid::Uuid::parse_str(id).ok()) {
                        self.visible.remove(&EventId(id));
                    }
                }
            }
        } else if direct
            && matches!(
                draft.kind,
                EventKind::MessageRevised | EventKind::MessageRestored
            )
            && let Some(target) = draft
                .payload
                .get("event_id")
                .and_then(Value::as_str)
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
            && let Some(original) = self.known.get_mut(&EventId(target))
        {
            if draft.kind == EventKind::MessageRevised
                && let Some(changes) = draft.payload.get("message").and_then(Value::as_object)
            {
                for (field, value) in changes {
                    original.fields.insert(field.clone(), value.clone());
                }
                original.content = original
                    .fields
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
            self.visible.insert(EventId(target), original.clone());
        }
    }

    fn ordered(&self) -> Vec<&ProjectedMessage> {
        let mut messages: Vec<_> = self.visible.values().collect();
        messages.sort_by_key(|item| (item.learned_at, item.sequence));
        messages
    }

    pub fn count(&self) -> usize {
        self.visible.len()
    }

    pub fn contains(&self, message_id: &str) -> bool {
        self.visible
            .values()
            .any(|message| message.message_id == message_id)
    }

    pub fn messages(&self) -> Vec<ProjectedMessage> {
        self.ordered().into_iter().cloned().collect()
    }

    pub fn page(&self, limit: usize, offset: usize) -> Vec<ProjectedMessage> {
        if limit == 0 {
            return Vec::new();
        }
        let entries = self.ordered();
        let end = entries.len().saturating_sub(offset);
        let start = end.saturating_sub(limit);
        entries[start..end]
            .iter()
            .map(|item| (*item).clone())
            .collect()
    }
}

pub fn character_messages(facts: &[TimelineFact], character: CharacterId) -> Vec<ProjectedMessage> {
    let mut projection = CharacterMessageProjection::new(character);
    for item in character_view(facts, character) {
        projection.observe(&item);
    }
    projection.messages()
}
