use crate::{
    character::{
        CharacterState,
        state::{CharacterBehavior, CharacterWorkingState},
    },
    identity::CharacterId,
    memory::{CharacterMemories, MemoryChange},
    timeline::domain::{self, EventKind, TimelineError, TimelineFact},
};

pub(crate) fn character_state(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterState, TimelineError> {
    let count = domain::accepted_reply_count(facts, character)?;
    let behavior = character_behavior_at(facts, character)?;
    Ok(CharacterState {
        accepted_replies: count,
        revision: count,
        attitude: behavior.attitude,
        boredom: behavior.boredom,
        stress: behavior.stress,
    })
}

pub(crate) fn character_behavior_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterBehavior, TimelineError> {
    let mut current = CharacterBehavior::default();
    for fact in facts.iter() {
        if fact.draft.kind != EventKind::CharacterStateChanged
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        current = serde_json::from_value(
            fact.draft
                .payload
                .get("state")
                .cloned()
                .ok_or(TimelineError::Invalid("state change has no state"))?,
        )
        .map_err(|_| TimelineError::Invalid("invalid state change"))?;
        if !current.is_valid() {
            return Err(TimelineError::Invalid("invalid accepted character state"));
        }
    }
    Ok(current)
}

pub(crate) fn character_working_state_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterWorkingState, TimelineError> {
    let mut current = CharacterWorkingState::default();
    for fact in facts.iter() {
        if fact.draft.kind == EventKind::CharacterContextReset
            && fact.draft.observers.contains(&character)
        {
            current = CharacterWorkingState::default();
        }
        if fact.draft.kind != EventKind::CharacterWorkingStateReplaced
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        let next: CharacterWorkingState =
            serde_json::from_value(fact.draft.payload.get("working_state").cloned().ok_or(
                TimelineError::Invalid("working state effect has no payload"),
            )?)
            .map_err(|_| TimelineError::Invalid("invalid accepted working state"))?;
        if !next.is_valid() {
            return Err(TimelineError::Invalid("invalid accepted working state"));
        }
        current = next;
    }
    Ok(current)
}

pub(crate) fn character_memories_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterMemories, TimelineError> {
    let mut book = CharacterMemories::default();
    for fact in facts.iter() {
        if fact.draft.kind != EventKind::CharacterMemoryChanged
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        let change: MemoryChange = serde_json::from_value(
            fact.draft
                .payload
                .get("change")
                .cloned()
                .ok_or(TimelineError::Invalid("missing memory change"))?,
        )
        .map_err(|_| TimelineError::Invalid("invalid committed memory change"))?;
        book.apply_committed(&change)
            .map_err(TimelineError::Invalid)?;
    }
    Ok(book)
}
