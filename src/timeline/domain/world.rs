use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::identity::EventId;

use super::{EventKind, KnownEvent};

/// A world fact which the character knows, in the order it was learned.
/// It is a read-model value, not another source of session truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedWorldEvent {
    pub event_id: EventId,
    pub kind: EventKind,
    pub actor: Option<String>,
    pub sequence: u64,
    pub learned_at: u64,
    pub occurred_at_unix_ms: u64,
    pub payload: Map<String, Value>,
}

impl ProjectedWorldEvent {
    fn from_known(item: &KnownEvent) -> Option<Self> {
        let fact = &item.fact;
        if !matches!(
            fact.draft.kind,
            EventKind::UnityEvent
                | EventKind::GameStarted
                | EventKind::GameAction
                | EventKind::GameEnded
                | EventKind::PlayerInactive
                | EventKind::CharacterJoined
                | EventKind::CharacterLeft
        ) {
            return None;
        }
        Some(Self {
            event_id: fact.draft.id,
            kind: fact.draft.kind,
            actor: fact.draft.actor.clone(),
            sequence: fact.sequence,
            learned_at: item.learned_at,
            occurred_at_unix_ms: fact.draft.occurred_at_unix_ms,
            payload: fact.draft.payload.clone(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct CharacterWorldProjection {
    known_ids: HashSet<EventId>,
    events: Vec<ProjectedWorldEvent>,
}

impl CharacterWorldProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.known_ids.clear();
        self.events.clear();
    }

    pub fn observe(&mut self, item: &KnownEvent) {
        if self.known_ids.insert(item.fact.draft.id)
            && let Some(event) = ProjectedWorldEvent::from_known(item)
        {
            self.events.push(event);
        }
    }

    pub fn events(&self) -> Vec<ProjectedWorldEvent> {
        self.events.clone()
    }
}
