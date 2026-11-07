use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

pub use crate::character::state::{CharacterBehavior, CharacterStateDelta, CharacterWorkingState};
use crate::identity::{CharacterId, EventId};
use crate::memory::MemoryChange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchId(pub Uuid);

impl BranchId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for BranchId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventKind {
    PlayerMessage,
    CharacterMessage,
    CharacterObservation,
    CharacterStateChanged,
    CharacterWorkingStateReplaced,
    CharacterMemoryChanged,
    ReminderScheduled,
    ReminderUpdated,
    ReminderDeleted,
    ReminderFired,
    ReminderDeliveryFailed,
    ReminderDeliverySettled,
    ReminderDeliveryRetried,
    ReminderToolResult,
    CharacterJoined,
    CharacterLeft,
    UnityEvent,
    GameStarted,
    GameAction,
    GameEnded,
    GameStateCaptured,
    PlayerInactive,
    ProactiveOpportunity,
    SessionNote,
    MessageHidden,
    MessageRevised,
    MessageRestored,
    CharacterContextReset,
    Checkpoint,
    BranchCreated,
}

impl EventKind {
    pub(crate) fn is_message(self) -> bool {
        matches!(self, Self::PlayerMessage | Self::CharacterMessage)
    }
}

/// Materialized, committed effect owned by the canonical Timeline domain.
/// Other modules produce this value but Timeline never depends on the generator.
#[derive(Debug, Clone, PartialEq)]
pub enum AppliedEffect {
    CharacterStateChanged(CharacterBehavior),
    CharacterWorkingStateReplaced(CharacterWorkingState),
    CharacterMemoryChanged(MemoryChange),
    /// Sanitized public match fact committed together with the reply.
    GameAdvanced(crate::games::GameFact),
    ReminderChanged(crate::reminders::ReminderChange),
    ReminderToolResult(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalRelation {
    ReplyTo,
    ReactTo,
    CausedBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CausalLink {
    pub relation: CausalRelation,
    pub event_id: EventId,
}

/// Domain interface for an immutable occurrence, independent of its storage representation.
/// The Timeline decides effective knowledge using actual witnesses and reset history.
pub trait Event {
    fn event_id(&self) -> EventId;
    fn kind(&self) -> EventKind;
    fn witnesses(&self) -> BTreeSet<CharacterId>;
    fn links(&self) -> &[CausalLink];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDraft {
    pub id: EventId,
    pub kind: EventKind,
    pub payload: Map<String, Value>,
    pub actor: Option<String>,
    pub observers: BTreeSet<CharacterId>,
    pub links: Vec<CausalLink>,
    pub external_message_id: Option<String>,
    pub occurred_at_unix_ms: u64,
    pub schema_version: u32,
}

impl EventDraft {
    pub fn new(kind: EventKind) -> Self {
        Self {
            id: EventId::new(),
            kind,
            payload: Map::new(),
            actor: None,
            observers: BTreeSet::new(),
            links: Vec::new(),
            external_message_id: None,
            occurred_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            schema_version: 1,
        }
    }

    pub(crate) fn same_intent(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.occurred_at_unix_ms = 0;
        right.occurred_at_unix_ms = 0;
        left.links
            .sort_by_key(|link| (link.relation as u8, link.event_id.0));
        right
            .links
            .sort_by_key(|link| (link.relation as u8, link.event_id.0));
        left == right
    }
}

impl Event for EventDraft {
    fn event_id(&self) -> EventId {
        self.id
    }
    fn kind(&self) -> EventKind {
        self.kind
    }
    fn witnesses(&self) -> BTreeSet<CharacterId> {
        self.observers.clone()
    }
    fn links(&self) -> &[CausalLink] {
        &self.links
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineFact {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub sequence: u64,
    pub draft: EventDraft,
}

impl Event for TimelineFact {
    fn event_id(&self) -> EventId {
        self.draft.event_id()
    }
    fn kind(&self) -> EventKind {
        self.draft.kind()
    }
    fn witnesses(&self) -> BTreeSet<CharacterId> {
        self.draft.witnesses()
    }
    fn links(&self) -> &[CausalLink] {
        self.draft.links()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownEvent {
    pub fact: TimelineFact,
    pub learned_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub id: BranchId,
    pub session_id: SessionId,
    pub parent: Option<BranchId>,
    pub fork_sequence: u64,
    pub head_sequence: u64,
    pub checkpoint: Option<EventId>,
}

/// Preconditions for accepting an event into the currently selected timeline.
/// A durable repository checks these in the same transaction as the append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendGuard {
    pub selection_generation: u64,
    pub context: Option<(CharacterId, u64, Option<EventId>)>,
    /// Expected revision of canonical accepted replies, checked with the append.
    pub accepted_reply_revision: Option<(CharacterId, u64)>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TimelineError {
    #[error("timeline branch or event was not found in this branch world")]
    NotFound,
    #[error("timeline identity is already used for different semantic content")]
    Conflict,
    #[error("timeline branch head advanced since the command was prepared")]
    ConcurrentWrite,
    #[error("invalid timeline operation: {0}")]
    Invalid(&'static str),
    #[error("timeline storage failed: {0}")]
    Storage(String),
}

#[cfg(test)]
mod character_behavior_tests {
    use super::*;

    #[test]
    fn proposed_deltas_use_bounded_python_default_scale() {
        let initial = CharacterBehavior::default();
        assert_eq!(initial.attitude, 60.0);
        assert_eq!(initial.boredom, 10.0);
        assert_eq!(initial.stress, 5.0);
        let delta = CharacterStateDelta {
            attitude: 1000.0,
            boredom: -1000.0,
            stress: -1.0,
        };
        let next = initial.apply(delta).unwrap();
        assert_eq!(next.attitude, 66.0);
        assert_eq!(next.boredom, 4.0);
        assert_eq!(next.stress, 4.0);
        let capped = CharacterBehavior {
            attitude: 100.0,
            boredom: 0.0,
            stress: 100.0,
        };
        assert_eq!(capped.apply(delta).unwrap().attitude, 100.0);
        assert_eq!(capped.apply(delta).unwrap().boredom, 0.0);
        assert!(
            initial
                .apply(CharacterStateDelta {
                    attitude: f64::NAN,
                    boredom: 0.0,
                    stress: 0.0,
                })
                .is_none()
        );
    }
}
