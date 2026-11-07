use serde_json::Value;
use uuid::Uuid;

use crate::identity::{CharacterId, EventId};

use super::{
    AppendGuard, BranchId, BranchInfo, EventDraft, KnownEvent, RequestEvidence, SessionId,
    TimelineError, TimelineFact, TimelineStore,
};

/// Semantic timeline operations; SQLite and future CFMD must obey identical laws.
/// Database revisions must never substitute for explicit branch heads.
pub trait TimelineRepository {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError>;
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError>;
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError>;
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError>;
    /// Reconcile an uncertain request after restart using committed source facts.
    /// None means no accepted reply in this branch prefix, not that the request
    /// failed or was cancelled. This does not restore in-memory request status.
    fn accepted_reply_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        request: Uuid,
        through: Option<u64>,
    ) -> Result<Option<TimelineFact>, TimelineError> {
        let info = self.branch(branch)?;
        let bound = through.unwrap_or(info.head_sequence);
        if bound > info.head_sequence {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let mut accepted = None;
        for fact in self.world(branch)? {
            if fact.sequence > bound {
                break;
            }
            if super::accepted_reply_request(&fact, character) == Some(request) {
                if accepted.is_some() {
                    return Err(TimelineError::Conflict);
                }
                accepted = Some(fact);
            }
        }
        Ok(accepted)
    }
    /// Inspect canonical evidence without claiming the request is still running.
    /// An absent record does not establish cancellation, failure or non-admission.
    fn request_evidence_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        request: Uuid,
        through: Option<u64>,
    ) -> Result<RequestEvidence, TimelineError> {
        let info = self.branch(branch)?;
        let bound = through.unwrap_or(info.head_sequence);
        if bound > info.head_sequence {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let facts = self.world(branch)?;
        super::request_evidence::request_evidence(
            facts.iter().take_while(|fact| fact.sequence <= bound),
            character,
            request,
        )
    }
    /// Recover an admitted request identity from an input/event identity known
    /// before submission. Only canonical PlayerMessage/ProactiveOpportunity
    /// events carrying a RequestId qualify; no process-local state is inferred.
    fn request_evidence_for_input_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        input: EventId,
        through: Option<u64>,
    ) -> Result<Option<(Uuid, RequestEvidence)>, TimelineError> {
        let info = self.branch(branch)?;
        let bound = through.unwrap_or(info.head_sequence);
        if bound > info.head_sequence {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let fact = match self.event(branch, input) {
            Ok(fact) if fact.sequence <= bound => fact,
            Ok(_) | Err(TimelineError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let addressed = match fact.draft.kind {
            super::EventKind::PlayerMessage => {
                fact.draft.actor.as_deref() == Some("player")
                    && fact.draft.observers.contains(&character)
            }
            super::EventKind::ProactiveOpportunity => {
                fact.draft.actor.as_deref().and_then(CharacterId::parse) == Some(character)
                    && fact.draft.observers.contains(&character)
            }
            _ => false,
        };
        if !addressed {
            return Ok(None);
        }
        let Some(request) = fact
            .draft
            .payload
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            return Ok(None);
        };
        let evidence = self.request_evidence_at(branch, character, request, Some(bound))?;
        Ok(Some((request, evidence)))
    }
    /// Stable ascending page of direct facts; observer filters direct witnesses,
    /// not derived knowledge or later disclosures.
    fn read(
        &self,
        branch: BranchId,
        after: u64,
        through: Option<u64>,
        limit: usize,
        observer: Option<CharacterId>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if limit == 0 {
            return Err(TimelineError::Invalid("read limit must be positive"));
        }
        let head = self.branch(branch)?.head_sequence;
        let ceiling = through.unwrap_or(head);
        if ceiling > head {
            return Err(TimelineError::Invalid("read position exceeds branch head"));
        }
        Ok(self
            .world(branch)?
            .into_iter()
            .filter(|fact| {
                fact.sequence > after
                    && fact.sequence <= ceiling
                    && observer.is_none_or(|id| fact.draft.observers.contains(&id))
            })
            .take(limit)
            .collect())
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError>;
    /// Append only when the supplied selection and optional turn context remain valid.
    /// Persistent implementations must validate all preconditions atomically with writes.
    fn append_guarded(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: AppendGuard,
    ) -> Result<Vec<TimelineFact>, TimelineError>;
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError>;
    fn context_epoch(
        &self,
        branch: BranchId,
        character: CharacterId,
    ) -> Result<u64, TimelineError> {
        self.context_epoch_at(branch, character, None)
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError>;
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError>;
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError>;
}

impl TimelineRepository for TimelineStore {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::create_session(self, session, root)
    }
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        TimelineStore::branch(self, id)
    }
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        TimelineStore::world(self, id)
    }
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        TimelineStore::event(self, branch, id)
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        TimelineStore::append(self, branch, expected_head, key, command, drafts)
    }
    fn append_guarded(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: AppendGuard,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if let Some((character, epoch, trigger)) = guard.context {
            if TimelineStore::context_epoch(self, branch, character)? != epoch {
                return Err(TimelineError::ConcurrentWrite);
            }
            if let Some(event) = trigger
                && !TimelineStore::knows(self, branch, character, event, None)?
            {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        if let Some((character, expected_revision)) = guard.accepted_reply_revision {
            let facts = TimelineStore::world(self, branch)?;
            if super::accepted_reply_count(&facts, character)? != expected_revision {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        TimelineStore::append(self, branch, expected_head, key, command, drafts)
    }
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError> {
        TimelineStore::context_epoch_at(self, branch, character, through)
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::fork(self, parent, checkpoint, child)
    }
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        TimelineStore::view(self, branch, character, through)
    }
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        TimelineStore::knows(self, branch, character, event, through)
    }
}
