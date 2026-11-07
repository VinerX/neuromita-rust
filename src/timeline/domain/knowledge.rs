use crate::identity::{CharacterId, EventId};

use super::model::{EventKind, TimelineFact};

/// Only the directly observed event is acquired; links never transfer knowledge.
pub(crate) struct KnowledgeChange {
    pub reset: bool,
    pub acquired: EventId,
}

pub(crate) fn knowledge_change(
    fact: &TimelineFact,
    observer: CharacterId,
) -> Option<KnowledgeChange> {
    if fact.draft.kind == EventKind::GameStateCaptured {
        return None;
    }
    fact.draft
        .observers
        .contains(&observer)
        .then_some(KnowledgeChange {
            reset: fact.draft.kind == EventKind::CharacterContextReset,
            acquired: fact.draft.id,
        })
}
