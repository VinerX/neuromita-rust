use serde_json::{Map, Value, json};

use crate::identity::{CharacterId, EventId};

use super::domain::{EventDraft, EventKind};

/// A change to one character's presentation of messages already known to them.
/// Original message facts remain immutable and other characters are unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageAction {
    Hide {
        targets: Vec<EventId>,
    },
    Revise {
        target: EventId,
        changes: Map<String, Value>,
    },
    Restore {
        target: EventId,
    },
}

impl MessageAction {
    pub(crate) fn into_draft(self, id: EventId, character: CharacterId) -> EventDraft {
        let mut draft = match self {
            Self::Hide { targets } => {
                let mut draft = EventDraft::new(EventKind::MessageHidden);
                draft.payload.insert(
                    "event_ids".into(),
                    json!(
                        targets
                            .into_iter()
                            .map(|id| id.0.to_string())
                            .collect::<Vec<_>>()
                    ),
                );
                draft
            }
            Self::Revise { target, changes } => {
                let mut draft = EventDraft::new(EventKind::MessageRevised);
                draft
                    .payload
                    .insert("event_id".into(), json!(target.0.to_string()));
                draft
                    .payload
                    .insert("message".into(), Value::Object(changes));
                draft
            }
            Self::Restore { target } => {
                let mut draft = EventDraft::new(EventKind::MessageRestored);
                draft
                    .payload
                    .insert("event_id".into(), json!(target.0.to_string()));
                draft
            }
        };
        draft.id = id;
        draft.observers.insert(character);
        draft
    }
}
