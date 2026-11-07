use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::identity::{CharacterId, EventId};

use super::{
    index::{AppendIndex, WorldIndex, WorldLookup},
    model::{
        BranchId, BranchInfo, CausalRelation, EventDraft, EventKind, KnownEvent, SessionId,
        TimelineError, TimelineFact,
    },
    projection::{character_view, present_characters},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    command: Value,
    drafts: Vec<EventDraft>,
    result: Vec<TimelineFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Branch {
    pub(crate) info: BranchInfo,
    pub(crate) events: Vec<TimelineFact>,
    pub(crate) receipts: HashMap<String, Receipt>,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineStore {
    branches: HashMap<BranchId, Branch>,
    indexes: HashMap<BranchId, WorldIndex>,
}

impl Receipt {
    pub(crate) fn new(command: Value, drafts: Vec<EventDraft>, result: Vec<TimelineFact>) -> Self {
        Self {
            command,
            drafts,
            result,
        }
    }

    pub(crate) fn matches(&self, command: &Value, drafts: &[EventDraft]) -> bool {
        &self.command == command
            && self.drafts.len() == drafts.len()
            && self
                .drafts
                .iter()
                .zip(drafts)
                .all(|(left, right)| left.same_intent(right))
    }

    #[cfg(feature = "sqlite-system")]
    pub(crate) fn result(&self) -> &[TimelineFact] {
        &self.result
    }
}

impl TimelineStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(feature = "sqlite-system")]
    pub(crate) fn persisted_branches(&self) -> Vec<Branch> {
        let mut branches: Vec<_> = self.branches.values().cloned().collect();
        branches.sort_by_key(|branch| branch.info.id.0);
        branches
    }

    #[cfg(feature = "sqlite-system")]
    pub(crate) fn load_persisted(branches: Vec<Branch>) -> Result<Self, TimelineError> {
        let mut map = HashMap::new();
        let mut roots = HashMap::new();
        for branch in branches {
            let info = branch.info;
            if info.parent.is_none() && roots.insert(info.session_id, info.id).is_some() {
                return Err(TimelineError::Storage("duplicate session root".into()));
            }
            if info.fork_sequence.checked_add(branch.events.len() as u64)
                != Some(info.head_sequence)
            {
                return Err(TimelineError::Storage("invalid branch event count".into()));
            }
            for (index, event) in branch.events.iter().enumerate() {
                if event.sequence != info.fork_sequence + index as u64 + 1
                    || event.session_id != info.session_id
                    || event.branch_id != info.id
                {
                    return Err(TimelineError::Storage(
                        "invalid persisted event identity".into(),
                    ));
                }
            }
            if map.insert(info.id, branch).is_some() {
                return Err(TimelineError::Storage("duplicate branch identity".into()));
            }
        }
        for branch in map.values() {
            let info = branch.info;
            if let Some(parent) = info.parent {
                let parent_info = map
                    .get(&parent)
                    .ok_or_else(|| TimelineError::Storage("missing parent branch".into()))?
                    .info;
                if parent_info.session_id != info.session_id
                    || info.fork_sequence == 0
                    || info.fork_sequence > parent_info.head_sequence
                    || info.checkpoint.is_none()
                {
                    return Err(TimelineError::Storage("invalid branch lineage".into()));
                }
            } else if info.fork_sequence != 0 || info.checkpoint.is_some() {
                return Err(TimelineError::Storage("invalid root branch".into()));
            }
        }
        let store = Self {
            branches: map,
            indexes: HashMap::new(),
        };
        for info in store.branches.values().map(|branch| branch.info) {
            let mut visited = HashSet::new();
            let mut cursor = Some(info.id);
            while let Some(id) = cursor {
                if !visited.insert(id) {
                    return Err(TimelineError::Storage("cyclic branch lineage".into()));
                }
                cursor = store
                    .branches
                    .get(&id)
                    .and_then(|branch| branch.info.parent);
            }
        }
        Ok(store)
    }

    pub fn create_session(
        &mut self,
        session_id: SessionId,
        branch_id: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        if let Some(branch) = self.branches.get(&branch_id) {
            return if branch.info.session_id == session_id && branch.info.parent.is_none() {
                Ok(branch.info)
            } else {
                Err(TimelineError::Conflict)
            };
        }
        if self
            .branches
            .values()
            .any(|branch| branch.info.session_id == session_id)
        {
            return Err(TimelineError::Conflict);
        }
        let info = BranchInfo {
            id: branch_id,
            session_id,
            parent: None,
            fork_sequence: 0,
            head_sequence: 0,
            checkpoint: None,
        };
        self.branches.insert(
            branch_id,
            Branch {
                info,
                events: Vec::new(),
                receipts: HashMap::new(),
            },
        );
        Ok(info)
    }

    pub fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        Ok(self.branches.get(&id).ok_or(TimelineError::NotFound)?.info)
    }

    pub fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        let branch = self.branches.get(&id).ok_or(TimelineError::NotFound)?;
        let mut facts = if let Some(parent) = branch.info.parent {
            self.world(parent)?
                .into_iter()
                .take_while(|fact| fact.sequence <= branch.info.fork_sequence)
                .collect()
        } else {
            Vec::new()
        };
        facts.extend(branch.events.iter().cloned());
        Ok(facts)
    }

    pub fn event(&self, branch_id: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        self.world(branch_id)?
            .into_iter()
            .find(|fact| fact.draft.id == id)
            .ok_or(TimelineError::NotFound)
    }

    pub fn view(
        &self,
        branch_id: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        let info = self.branch(branch_id)?;
        let bound = through.unwrap_or(info.head_sequence);
        if bound > info.head_sequence {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let facts = self.world(branch_id)?;
        Ok(character_view(
            &facts
                .into_iter()
                .filter(|fact| fact.sequence <= bound)
                .collect::<Vec<_>>(),
            character,
        ))
    }

    pub fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError> {
        let head = self.branch(branch)?.head_sequence;
        let ceiling = through.unwrap_or(head);
        if ceiling > head {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        Ok(self
            .world(branch)?
            .iter()
            .filter(|fact| {
                fact.sequence <= ceiling
                    && fact.draft.kind == EventKind::CharacterContextReset
                    && fact.draft.observers.contains(&character)
            })
            .map(|fact| fact.sequence)
            .max()
            .unwrap_or(0))
    }

    pub fn context_epoch(
        &self,
        branch: BranchId,
        character: CharacterId,
    ) -> Result<u64, TimelineError> {
        self.context_epoch_at(branch, character, None)
    }

    pub fn knows(
        &self,
        branch_id: BranchId,
        character: CharacterId,
        event_id: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        Ok(self
            .view(branch_id, character, through)?
            .iter()
            .any(|known| known.fact.draft.id == event_id))
    }

    pub fn present(
        &self,
        branch_id: BranchId,
    ) -> Result<std::collections::BTreeSet<CharacterId>, TimelineError> {
        Ok(present_characters(&self.world(branch_id)?))
    }

    pub fn append(
        &mut self,
        branch_id: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if key.is_empty() || key.trim() != key {
            return Err(TimelineError::Invalid(
                "operation key must be canonical and nonempty",
            ));
        }
        if !command.is_object() {
            return Err(TimelineError::Invalid(
                "operation command must be an object",
            ));
        }
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(TimelineError::NotFound)?;
        if let Some(receipt) = branch.receipts.get(key) {
            return if receipt.matches(&command, &drafts) {
                Ok(receipt.result.clone())
            } else {
                Err(TimelineError::Conflict)
            };
        }
        if expected_head != branch.info.head_sequence {
            return Err(TimelineError::ConcurrentWrite);
        }
        if !self.indexes.contains_key(&branch_id) {
            let world = self.world(branch_id)?;
            self.indexes
                .insert(branch_id, WorldIndex::from_facts(&world)?);
        }
        let index = self
            .indexes
            .get(&branch_id)
            .expect("branch index initialized");
        let result = Self::validate_batch(branch.info, expected_head, &drafts, index)?;
        let sequence = expected_head + result.len() as u64;
        let branch = self
            .branches
            .get_mut(&branch_id)
            .ok_or(TimelineError::NotFound)?;
        branch.info.head_sequence = sequence;
        branch.events.extend(result.iter().cloned());
        branch.receipts.insert(
            key.to_string(),
            Receipt::new(command, drafts, result.clone()),
        );
        self.indexes
            .get_mut(&branch_id)
            .expect("branch index initialized")
            .extend(&result)?;
        Ok(result)
    }

    /// The only semantic append validator. Both in-memory and storage-backed
    /// repositories provide a read-only index over the same visible branch world.
    pub(crate) fn validate_batch(
        branch: BranchInfo,
        expected_head: u64,
        drafts: &[EventDraft],
        base: &dyn WorldLookup,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if branch.head_sequence != expected_head {
            return Err(TimelineError::ConcurrentWrite);
        }
        let mut pending = AppendIndex::new(base);
        let mut result = Vec::with_capacity(drafts.len());
        let mut sequence = expected_head;
        for draft in drafts {
            Self::validate(&pending, draft)?;
            pending.validate_effect(draft)?;
            sequence = sequence
                .checked_add(1)
                .ok_or(TimelineError::Invalid("sequence overflow"))?;
            let fact = TimelineFact {
                session_id: branch.session_id,
                branch_id: branch.id,
                sequence,
                draft: draft.clone(),
            };
            pending.record(&fact);
            result.push(fact);
        }
        pending.validate_final_effects()?;
        Ok(result)
    }

    pub fn fork(
        &mut self,
        branch_id: BranchId,
        checkpoint_id: EventId,
        child_id: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        if let Some(existing) = self.branches.get(&child_id) {
            return if existing.info.parent == Some(branch_id)
                && existing.info.checkpoint == Some(checkpoint_id)
            {
                Ok(existing.info)
            } else {
                Err(TimelineError::Conflict)
            };
        }
        let parent = self.branch(branch_id)?;
        let checkpoint = self.event(branch_id, checkpoint_id)?;
        if checkpoint.draft.kind != EventKind::Checkpoint {
            return Err(TimelineError::Invalid("fork requires a checkpoint"));
        }
        let marker_sequence = checkpoint
            .sequence
            .checked_add(1)
            .ok_or(TimelineError::Invalid("sequence overflow"))?;
        let mut payload = Map::new();
        payload.insert(
            "parent_branch_id".into(),
            Value::String(branch_id.0.to_string()),
        );
        payload.insert(
            "checkpoint_id".into(),
            Value::String(checkpoint_id.0.to_string()),
        );
        let mut marker = EventDraft::new(EventKind::BranchCreated);
        marker.payload = payload;
        let info = BranchInfo {
            id: child_id,
            session_id: parent.session_id,
            parent: Some(branch_id),
            fork_sequence: checkpoint.sequence,
            head_sequence: marker_sequence,
            checkpoint: Some(checkpoint_id),
        };
        let fact = TimelineFact {
            session_id: parent.session_id,
            branch_id: child_id,
            sequence: marker_sequence,
            draft: marker,
        };
        self.branches.insert(
            child_id,
            Branch {
                info,
                events: vec![fact],
                receipts: HashMap::new(),
            },
        );
        Ok(info)
    }

    fn validate(index: &AppendIndex<'_>, draft: &EventDraft) -> Result<(), TimelineError> {
        if draft
            .actor
            .as_deref()
            .is_some_and(|actor| actor.trim() != actor || actor.is_empty())
        {
            return Err(TimelineError::Invalid("invalid actor identity"));
        }
        if draft.schema_version != 1 {
            return Err(TimelineError::Invalid("unsupported event schema version"));
        }
        if index.kind(draft.id)?.is_some() {
            return Err(TimelineError::Conflict);
        }
        if let Some(ref external) = draft.external_message_id {
            if external.is_empty() || external.trim() != external {
                return Err(TimelineError::Invalid("invalid external message identity"));
            }
            if index.has_external(external)? {
                return Err(TimelineError::Conflict);
            }
        }
        if matches!(
            draft.kind,
            EventKind::PlayerMessage
                | EventKind::CharacterMessage
                | EventKind::CharacterObservation
        ) && draft.actor.is_none()
        {
            return Err(TimelineError::Invalid("message requires an author"));
        }
        if matches!(
            draft.kind,
            EventKind::CharacterMessage | EventKind::CharacterObservation
        ) && !draft
            .actor
            .as_deref()
            .and_then(CharacterId::parse)
            .is_some_and(|actor| draft.observers.contains(&actor))
        {
            return Err(TimelineError::Invalid(
                "character must know its own message",
            ));
        }
        if matches!(
            draft.kind,
            EventKind::CharacterJoined | EventKind::CharacterLeft
        ) && !draft
            .actor
            .as_deref()
            .and_then(CharacterId::parse)
            .is_some_and(|actor| draft.observers.contains(&actor))
        {
            return Err(TimelineError::Invalid("presence change requires witness"));
        }
        if matches!(draft.kind, EventKind::CharacterContextReset) && draft.observers.is_empty() {
            return Err(TimelineError::Invalid("context reset requires an observer"));
        }
        if draft.kind == EventKind::Checkpoint
            && !draft.payload.get("snapshot").is_some_and(Value::is_object)
        {
            return Err(TimelineError::Invalid(
                "checkpoint requires a snapshot object",
            ));
        }
        if draft.kind == EventKind::BranchCreated {
            return Err(TimelineError::Invalid("branch markers are created by fork"));
        }
        if matches!(
            draft.kind,
            EventKind::MessageHidden | EventKind::MessageRevised | EventKind::MessageRestored
        ) {
            if draft.observers.is_empty() {
                return Err(TimelineError::Invalid(
                    "presentation change requires recipients",
                ));
            }
            let ids: Vec<EventId> = if draft.kind == EventKind::MessageHidden {
                draft
                    .payload
                    .get("event_ids")
                    .and_then(Value::as_array)
                    .filter(|ids| !ids.is_empty())
                    .ok_or(TimelineError::Invalid("hidden messages require targets"))?
                    .iter()
                    .map(parse_event_id)
                    .collect::<Result<_, _>>()?
            } else {
                vec![parse_event_id(draft.payload.get("event_id").ok_or(
                    TimelineError::Invalid("presentation change requires event_id"),
                )?)?]
            };
            for id in ids {
                let target = index.kind(id)?.ok_or(TimelineError::NotFound)?;
                if !matches!(
                    target,
                    EventKind::PlayerMessage
                        | EventKind::CharacterMessage
                        | EventKind::CharacterObservation
                        | EventKind::SessionNote
                ) || draft
                    .observers
                    .iter()
                    .try_fold(false, |missing, &observer| {
                        Ok::<_, TimelineError>(missing || !index.knows(observer, id)?)
                    })?
                {
                    return Err(TimelineError::Invalid("cannot modify unknown message"));
                }
                if draft.kind == EventKind::MessageRevised {
                    let replacement = draft
                        .payload
                        .get("message")
                        .and_then(Value::as_object)
                        .filter(|message| message.contains_key("content"))
                        .ok_or(TimelineError::Invalid(
                            "revision requires replacement content",
                        ))?;
                    for forbidden in [
                        "role",
                        "speaker",
                        "sender",
                        "timeline_event_id",
                        "timeline_sequence",
                        "timeline_learned_at",
                        "_history_row_id",
                        "time",
                        "message_id",
                    ] {
                        if replacement.contains_key(forbidden) {
                            return Err(TimelineError::Invalid(
                                "revision cannot replace message identity",
                            ));
                        }
                    }
                }
            }
        }
        let mut relations = HashSet::new();
        let mut reply = false;
        let mut react = false;
        for link in &draft.links {
            if !relations.insert((link.relation, link.event_id)) {
                return Err(TimelineError::Invalid("duplicate causal link"));
            }
            let target = index.kind(link.event_id)?.ok_or(TimelineError::NotFound)?;
            match link.relation {
                CausalRelation::ReplyTo | CausalRelation::ReactTo => {
                    let actor_knows = match draft.actor.as_deref().and_then(CharacterId::parse) {
                        Some(actor) => index.knows(actor, link.event_id)?,
                        None => false,
                    };
                    if draft.kind != EventKind::CharacterMessage || !actor_knows {
                        return Err(TimelineError::Invalid(
                            "response target is not known to actor",
                        ));
                    }
                    if link.relation == CausalRelation::ReplyTo {
                        if !target.is_message() {
                            return Err(TimelineError::Invalid("reply target must be a message"));
                        }
                        reply = true;
                    } else {
                        react = true;
                    }
                }
                CausalRelation::CausedBy => {}
            }
        }
        if reply && react {
            return Err(TimelineError::Invalid(
                "turn cannot reply and react together",
            ));
        }
        for relation in [CausalRelation::ReplyTo, CausalRelation::ReactTo] {
            if draft
                .links
                .iter()
                .filter(|link| link.relation == relation)
                .count()
                > 1
            {
                return Err(TimelineError::Invalid("multiple causal targets"));
            }
        }
        Ok(())
    }
}

fn parse_event_id(value: &Value) -> Result<EventId, TimelineError> {
    let value = value
        .as_str()
        .ok_or(TimelineError::Invalid("event identity must be a string"))?;
    let id = uuid::Uuid::parse_str(value)
        .map_err(|_| TimelineError::Invalid("invalid event identity"))?;
    Ok(EventId(id))
}
