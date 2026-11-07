use std::collections::{HashMap, HashSet};

use crate::identity::{CharacterId, EventId};

use super::model::TimelineError;
use super::{
    knowledge::knowledge_change,
    model::{EventKind, TimelineFact},
};
use crate::{
    character::state::{CharacterBehavior, CharacterWorkingState},
    memory::{CharacterMemories, MemoryChange},
};

/// Read-only facts needed by the single domain append validator.
/// A repository can answer these with indexed storage reads without reconstructing
/// a full world. No storage-specific rule is encoded in this contract.
pub(crate) trait WorldLookup {
    fn memories(&self, character: CharacterId) -> Result<CharacterMemories, TimelineError>;
    fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError>;
    fn has_external(&self, external: &str) -> Result<bool, TimelineError>;
    fn knows(&self, character: CharacterId, event: EventId) -> Result<bool, TimelineError>;
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorldIndex {
    memories: HashMap<CharacterId, CharacterMemories>,
    kinds: HashMap<EventId, EventKind>,
    external_ids: HashSet<String>,
    known: HashMap<CharacterId, HashSet<EventId>>,
}

#[derive(Debug, Clone, Default)]
struct KnowledgeChange {
    reset: bool,
    added: HashSet<EventId>,
}

pub(super) struct AppendIndex<'a> {
    memories: HashMap<CharacterId, CharacterMemories>,
    base: &'a dyn WorldLookup,
    kinds: HashMap<EventId, EventKind>,
    external_ids: HashSet<String>,
    knowledge: HashMap<CharacterId, KnowledgeChange>,
}

impl WorldIndex {
    pub(super) fn from_facts(facts: &[TimelineFact]) -> Result<Self, TimelineError> {
        let mut index = Self::default();
        for fact in facts {
            index.insert(fact)?;
        }
        Ok(index)
    }

    fn insert(&mut self, fact: &TimelineFact) -> Result<(), TimelineError> {
        if fact.draft.kind == EventKind::CharacterMemoryChanged {
            let character = effect_character(&fact.draft)?;
            let change = payload::<MemoryChange>(&fact.draft, "change")?;
            self.memories
                .entry(character)
                .or_default()
                .apply_committed(&change)
                .map_err(TimelineError::Invalid)?;
        }
        self.kinds.insert(fact.draft.id, fact.draft.kind);
        if let Some(external) = &fact.draft.external_message_id {
            self.external_ids.insert(external.clone());
        }
        for observer in &fact.draft.observers {
            let Some(change) = knowledge_change(fact, *observer) else {
                continue;
            };
            let known = self.known.entry(*observer).or_default();
            if change.reset {
                known.clear();
            }
            known.insert(change.acquired);
        }
        Ok(())
    }

    pub(super) fn extend(&mut self, facts: &[TimelineFact]) -> Result<(), TimelineError> {
        for fact in facts {
            self.insert(fact)?;
        }
        Ok(())
    }
}

impl WorldLookup for WorldIndex {
    fn memories(&self, character: CharacterId) -> Result<CharacterMemories, TimelineError> {
        Ok(self.memories.get(&character).cloned().unwrap_or_default())
    }
    fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError> {
        Ok(self.kinds.get(&event).copied())
    }

    fn has_external(&self, external: &str) -> Result<bool, TimelineError> {
        Ok(self.external_ids.contains(external))
    }

    fn knows(&self, character: CharacterId, event: EventId) -> Result<bool, TimelineError> {
        Ok(self
            .known
            .get(&character)
            .is_some_and(|ids| ids.contains(&event)))
    }
}

impl<'a> AppendIndex<'a> {
    pub(super) fn validate_final_effects(&self) -> Result<(), TimelineError> {
        for memories in self.memories.values() {
            memories.ensure_unique().map_err(TimelineError::Invalid)?;
        }
        Ok(())
    }

    pub(super) fn new(base: &'a dyn WorldLookup) -> Self {
        Self {
            memories: HashMap::new(),
            base,
            kinds: HashMap::new(),
            external_ids: HashSet::new(),
            knowledge: HashMap::new(),
        }
    }

    pub(super) fn validate_effect(
        &mut self,
        draft: &super::EventDraft,
    ) -> Result<(), TimelineError> {
        if !matches!(
            draft.kind,
            EventKind::GameStateCaptured
                | EventKind::CharacterMemoryChanged
                | EventKind::CharacterStateChanged
                | EventKind::CharacterWorkingStateReplaced
                | EventKind::ReminderScheduled
                | EventKind::ReminderUpdated
                | EventKind::ReminderDeleted
                | EventKind::ReminderFired
                | EventKind::ReminderDeliveryFailed
                | EventKind::ReminderDeliverySettled
                | EventKind::ReminderDeliveryRetried
                | EventKind::ReminderToolResult
        ) {
            return Ok(());
        }
        let character = effect_character(draft)?;
        match draft.kind {
            EventKind::GameStateCaptured => {
                let state: crate::games::GameSlot = payload(draft, "game_state")?;
                state.validate().map_err(TimelineError::Invalid)?;
            }
            EventKind::CharacterStateChanged => {
                if !payload::<CharacterBehavior>(draft, "state")?.is_valid() {
                    return Err(TimelineError::Invalid("invalid character behavior"));
                }
            }
            EventKind::CharacterWorkingStateReplaced => {
                if !payload::<CharacterWorkingState>(draft, "working_state")?.is_valid() {
                    return Err(TimelineError::Invalid("invalid working state"));
                }
            }
            EventKind::CharacterMemoryChanged => {
                let change = payload::<MemoryChange>(draft, "change")?;
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.memories.entry(character)
                {
                    entry.insert(self.base.memories(character)?);
                }
                self.memories
                    .get_mut(&character)
                    .expect("loaded memory projection")
                    .apply_committed(&change)
                    .map_err(TimelineError::Invalid)?;
            }
            EventKind::ReminderScheduled | EventKind::ReminderUpdated => {
                let record: crate::reminders::ReminderRecord = payload(draft, "reminder")?;
                record
                    .validate()
                    .map_err(|_| TimelineError::Invalid("invalid reminder record"))?;
                if record.character != character {
                    return Err(TimelineError::Invalid(
                        "reminder belongs to another character",
                    ));
                }
            }
            EventKind::ReminderDeleted => {
                let _: crate::reminders::ReminderDeletion = payload(draft, "deleted")?;
            }
            EventKind::ReminderFired => {
                let fired: crate::reminders::FiredOccurrence = payload(draft, "fired")?;
                if fired.character != character {
                    return Err(TimelineError::Invalid(
                        "fired reminder belongs to another character",
                    ));
                }
            }
            EventKind::ReminderDeliveryFailed => {
                let _: crate::reminders::DeliveryFailure = payload(draft, "failure")?;
            }
            EventKind::ReminderDeliverySettled => {
                let _: crate::reminders::DeliverySettlement = payload(draft, "settlement")?;
            }
            EventKind::ReminderDeliveryRetried => {
                let _: crate::reminders::DeliveryRetry = payload(draft, "retry")?;
            }
            EventKind::ReminderToolResult => {
                let content: String = payload(draft, "content")?;
                if content.trim().is_empty() {
                    return Err(TimelineError::Invalid("empty reminder tool result"));
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    pub(super) fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError> {
        if let Some(kind) = self.kinds.get(&event) {
            Ok(Some(*kind))
        } else {
            self.base.kind(event)
        }
    }

    pub(super) fn has_external(&self, external: &str) -> Result<bool, TimelineError> {
        if self.external_ids.contains(external) {
            Ok(true)
        } else {
            self.base.has_external(external)
        }
    }

    pub(super) fn knows(
        &self,
        character: CharacterId,
        event: EventId,
    ) -> Result<bool, TimelineError> {
        if let Some(change) = self.knowledge.get(&character) {
            if change.added.contains(&event) {
                return Ok(true);
            }
            if change.reset {
                return Ok(false);
            }
        }
        self.base.knows(character, event)
    }

    pub(super) fn record(&mut self, fact: &TimelineFact) {
        self.kinds.insert(fact.draft.id, fact.draft.kind);
        if let Some(external) = &fact.draft.external_message_id {
            self.external_ids.insert(external.clone());
        }
        for observer in &fact.draft.observers {
            let Some(effect) = knowledge_change(fact, *observer) else {
                continue;
            };
            let change = self.knowledge.entry(*observer).or_default();
            if effect.reset {
                change.reset = true;
                change.added.clear();
            }
            change.added.insert(effect.acquired);
        }
    }
}

fn effect_character(draft: &super::EventDraft) -> Result<CharacterId, TimelineError> {
    let character = draft
        .actor
        .as_deref()
        .and_then(CharacterId::parse)
        .ok_or(TimelineError::Invalid("effect requires a character actor"))?;
    if draft.observers.len() != 1 || !draft.observers.contains(&character) {
        return Err(TimelineError::Invalid(
            "character effect must remain private to its actor",
        ));
    }
    Ok(character)
}

fn payload<T: serde::de::DeserializeOwned>(
    draft: &super::EventDraft,
    key: &str,
) -> Result<T, TimelineError> {
    if draft.payload.len() != 1 {
        return Err(TimelineError::Invalid(
            "effect requires exactly one typed payload",
        ));
    }
    let value = draft
        .payload
        .get(key)
        .ok_or(TimelineError::Invalid("missing typed effect payload"))?;
    serde_json::from_value(value.clone())
        .map_err(|_| TimelineError::Invalid("invalid typed effect payload"))
}
