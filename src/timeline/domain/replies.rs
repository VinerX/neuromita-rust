use uuid::Uuid;

use crate::identity::CharacterId;

use super::{EventKind, TimelineError, TimelineFact};

/// The narrow revision of accepted character replies is derived from immutable facts.
pub(crate) fn accepted_reply_count<'a>(
    facts: impl IntoIterator<Item = &'a TimelineFact>,
    character: CharacterId,
) -> Result<u64, TimelineError> {
    let mut count = 0_u64;
    for fact in facts {
        if accepted_reply_request(fact, character).is_some() {
            count = count
                .checked_add(1)
                .ok_or(TimelineError::Invalid("reply count overflow"))?;
        }
    }
    Ok(count)
}

/// Only a canonically accepted character reply has a durable request identity.
/// Player ingress and proactive opportunities are not acceptance receipts.
pub(crate) fn accepted_reply_request(fact: &TimelineFact, character: CharacterId) -> Option<Uuid> {
    if fact.draft.kind != EventKind::CharacterMessage
        || fact.draft.actor.as_deref().and_then(CharacterId::parse) != Some(character)
    {
        return None;
    }
    fact.draft
        .payload
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}
