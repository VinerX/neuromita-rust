use uuid::Uuid;

use crate::identity::CharacterId;

use super::{EventKind, TimelineError, TimelineFact};

/// The strongest durable evidence about a request found in a branch prefix.
/// None of these variants reports a process-local RequestState.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestEvidence {
    /// No matching canonical fact exists in the inspected prefix.
    NoRecord,
    /// Player input reached the timeline; generation may never have started.
    PlayerInput(TimelineFact),
    /// A proactive trigger reached the timeline; generation may not have finished.
    ProactiveTrigger(TimelineFact),
    /// A character reply was committed; no claim about external effects is implied.
    AcceptedReply(TimelineFact),
}

pub(crate) fn request_evidence<'a>(
    facts: impl IntoIterator<Item = &'a TimelineFact>,
    character: CharacterId,
    request: Uuid,
) -> Result<RequestEvidence, TimelineError> {
    let mut input = None;
    let mut proactive = None;
    let mut accepted = None;
    for fact in facts {
        let matches_request = fact
            .draft
            .payload
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            == Some(request);
        if !matches_request {
            continue;
        }
        let slot = match fact.draft.kind {
            EventKind::PlayerMessage
                if fact.draft.actor.as_deref() == Some("player")
                    && fact.draft.observers.contains(&character) =>
            {
                &mut input
            }
            EventKind::ProactiveOpportunity
                if fact.draft.actor.as_deref().and_then(CharacterId::parse) == Some(character)
                    && fact.draft.observers.contains(&character) =>
            {
                &mut proactive
            }
            EventKind::CharacterMessage
                if fact.draft.actor.as_deref().and_then(CharacterId::parse) == Some(character) =>
            {
                &mut accepted
            }
            _ => continue,
        };
        if slot.replace(fact.clone()).is_some() {
            return Err(TimelineError::Conflict);
        }
    }
    if input.is_some() && proactive.is_some() {
        return Err(TimelineError::Conflict);
    }
    Ok(if let Some(fact) = accepted {
        RequestEvidence::AcceptedReply(fact)
    } else if let Some(fact) = input {
        RequestEvidence::PlayerInput(fact)
    } else if let Some(fact) = proactive {
        RequestEvidence::ProactiveTrigger(fact)
    } else {
        RequestEvidence::NoRecord
    })
}
