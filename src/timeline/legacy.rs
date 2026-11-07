use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{CharacterId, RequestId};

use super::{
    EventContent, EventId, TimelineEvent,
    domain::{CausalRelation, EventDraft, EventKind, TimelineFact},
};

pub(super) fn legacy_event(fact: &TimelineFact, observer: CharacterId) -> Option<TimelineEvent> {
    let draft = &fact.draft;
    let text: Arc<str> = super::conversation::spoken_text(draft.payload.get("content")?)?.into();
    let content = match draft.kind {
        EventKind::PlayerMessage => EventContent::PlayerMessage { text },
        EventKind::CharacterObservation => EventContent::Observation { text },
        EventKind::ProactiveOpportunity => {
            match draft.payload.get("legacy_intent").and_then(Value::as_str)? {
                "initiate" => EventContent::Initiation { text },
                "react" => EventContent::Reaction {
                    text,
                    target: EventId(Uuid::parse_str(draft.payload.get("target")?.as_str()?).ok()?),
                },
                _ => return None,
            }
        }
        EventKind::CharacterMessage => {
            let context = serde_json::from_value(draft.payload.get("context")?.clone()).ok()?;
            if let Some(link) = draft
                .links
                .iter()
                .find(|link| link.relation == CausalRelation::ReplyTo)
            {
                EventContent::CharacterReply {
                    text,
                    reply_to: link.event_id,
                    context,
                }
            } else if let Some(link) = draft
                .links
                .iter()
                .find(|link| link.relation == CausalRelation::ReactTo)
            {
                EventContent::Reaction {
                    text,
                    target: link.event_id,
                }
            } else {
                EventContent::Initiation { text }
            }
        }
        _ => return None,
    };
    Some(TimelineEvent {
        id: draft.id,
        sequence: fact.sequence,
        observer,
        request_id: draft
            .payload
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(RequestId),
        content,
    })
}

pub(super) fn make_draft(
    id: EventId,
    kind: EventKind,
    character: CharacterId,
    text: Arc<str>,
    request: Option<RequestId>,
) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.id = id;
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft.payload.insert("content".into(), json!(text.as_ref()));
    if let Some(request) = request {
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
    }
    draft
}
