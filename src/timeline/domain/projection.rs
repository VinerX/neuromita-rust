use std::collections::{BTreeSet, HashMap};

use crate::identity::{CharacterId, EventId};

use super::{
    knowledge::knowledge_change,
    model::{EventKind, KnownEvent, TimelineFact},
};

pub fn character_view(facts: &[TimelineFact], character: CharacterId) -> Vec<KnownEvent> {
    let mut known = HashMap::<EventId, KnownEvent>::new();
    for fact in facts {
        if let Some(change) = knowledge_change(fact, character) {
            if change.reset {
                known.clear();
            }
            known.insert(
                change.acquired,
                KnownEvent {
                    fact: fact.clone(),
                    learned_at: fact.sequence,
                },
            );
        }
    }
    let mut output: Vec<_> = known.into_values().collect();
    output.sort_by_key(|known| (known.learned_at, known.fact.sequence));
    output
}

pub fn knows(facts: &[TimelineFact], character: CharacterId, event: EventId) -> bool {
    character_view(facts, character)
        .iter()
        .any(|known| known.fact.draft.id == event)
}

pub fn present_characters(facts: &[TimelineFact]) -> BTreeSet<CharacterId> {
    let mut present = BTreeSet::new();
    for fact in facts {
        match fact.draft.kind {
            EventKind::CharacterJoined => {
                if let Some(character) = fact.draft.actor.as_deref().and_then(CharacterId::parse) {
                    present.insert(character);
                }
            }
            EventKind::CharacterLeft => {
                if let Some(character) = fact.draft.actor.as_deref().and_then(CharacterId::parse) {
                    present.remove(&character);
                }
            }
            _ => {}
        }
    }
    present
}
