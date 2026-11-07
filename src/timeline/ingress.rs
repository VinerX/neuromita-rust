use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::identity::{CharacterId, EventId};

use super::domain::{
    AppendGuard, BranchId, EventDraft, EventKind, TimelineError, TimelineFact, TimelineRepository,
};

/// An already identified semantic event, independent of any transport envelope.
/// Keep the ID and timestamp unchanged when retrying the same operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEvent {
    pub id: EventId,
    pub kind: EventKind,
    pub actor: Option<String>,
    pub observers: BTreeSet<CharacterId>,
    pub payload: Map<String, Value>,
    pub occurred_at_unix_ms: u64,
}

impl SemanticEvent {
    pub fn new(kind: EventKind, observers: impl IntoIterator<Item = CharacterId>) -> Self {
        let draft = EventDraft::new(kind);
        Self {
            id: draft.id,
            kind,
            actor: None,
            observers: observers.into_iter().collect(),
            payload: Map::new(),
            occurred_at_unix_ms: draft.occurred_at_unix_ms,
        }
    }

    fn into_draft(self) -> Result<EventDraft, TimelineError> {
        if !matches!(
            self.kind,
            EventKind::UnityEvent
                | EventKind::GameStarted
                | EventKind::GameAction
                | EventKind::GameEnded
                | EventKind::PlayerInactive
                | EventKind::ProactiveOpportunity
                | EventKind::CharacterJoined
                | EventKind::CharacterLeft
        ) {
            return Err(TimelineError::Invalid(
                "semantic ingress accepts world and presence events only",
            ));
        }
        Ok(EventDraft {
            id: self.id,
            kind: self.kind,
            payload: self.payload,
            actor: self.actor,
            observers: self.observers,
            links: Vec::new(),
            external_message_id: None,
            occurred_at_unix_ms: self.occurred_at_unix_ms,
            schema_version: 1,
        })
    }
}

/// One externally identified semantic operation. Retrying with the same key
/// and the same event identities must return the original facts, not append again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBatch {
    pub operation_id: String,
    pub events: Vec<SemanticEvent>,
}

impl SemanticBatch {
    pub fn new(operation_id: impl Into<String>, events: Vec<SemanticEvent>) -> Self {
        Self {
            operation_id: operation_id.into(),
            events,
        }
    }
}

/// Atomically records a semantic batch using the canonical repository laws.
/// The caller owns transport decoding, authentication and observer assignment.
pub fn append_semantic_batch(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
    expected_head: u64,
    guard: AppendGuard,
    batch: SemanticBatch,
) -> Result<Vec<TimelineFact>, TimelineError> {
    if batch.operation_id.is_empty()
        || batch.operation_id.trim() != batch.operation_id
        || batch.operation_id.len() > 256
    {
        return Err(TimelineError::Invalid(
            "invalid semantic operation identity",
        ));
    }
    if batch.events.is_empty() {
        return Err(TimelineError::Invalid("semantic batch must contain events"));
    }
    let source_times: Vec<u64> = batch
        .events
        .iter()
        .map(|event| event.occurred_at_unix_ms)
        .collect();
    let drafts = batch
        .events
        .into_iter()
        .map(SemanticEvent::into_draft)
        .collect::<Result<Vec<_>, _>>()?;
    repository.append_guarded(
        branch,
        expected_head,
        &format!("semantic:{}", batch.operation_id),
        json!({"kind": "semantic_world_ingress", "source_times": source_times}),
        drafts,
        guard,
    )
}
