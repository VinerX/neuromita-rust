use super::*;
use domain::{BranchInfo, KnownEvent, TimelineError, TimelineFact};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Counted {
    inner: TimelineStore,
    reads: Arc<AtomicUsize>,
}

impl TimelineRepository for Counted {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::create_session(&mut self.inner, session, root)
    }
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        TimelineStore::branch(&self.inner, id)
    }
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        TimelineStore::world(&self.inner, id)
    }
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        TimelineStore::event(&self.inner, branch, id)
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        TimelineStore::append(&mut self.inner, branch, expected_head, key, command, drafts)
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
            if TimelineStore::context_epoch(&self.inner, branch, character)? != epoch {
                return Err(TimelineError::ConcurrentWrite);
            }
            if let Some(event) = trigger
                && !TimelineStore::knows(&self.inner, branch, character, event, None)?
            {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        if let Some((character, expected_revision)) = guard.accepted_reply_revision {
            let facts = TimelineStore::world(&self.inner, branch)?;
            if domain::accepted_reply_count(&facts, character)? != expected_revision {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        TimelineStore::append(&mut self.inner, branch, expected_head, key, command, drafts)
    }
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError> {
        TimelineStore::context_epoch_at(&self.inner, branch, character, through)
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::fork(&mut self.inner, parent, checkpoint, child)
    }
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        TimelineStore::view(&self.inner, branch, character, through)
    }
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        TimelineStore::knows(&self.inner, branch, character, event, through)
    }
}

#[test]
fn context_uses_one_prefix_read_for_history_state_and_memory() {
    let mut timeline = SessionTimeline::new();
    let branch = timeline.branch;
    let mut inner = TimelineStore::new();
    inner.create_session(SessionId::new(), branch).unwrap();
    let mut observed = EventDraft::new(EventKind::CharacterObservation);
    observed.actor = Some("Crazy".into());
    observed.observers.insert(CharacterId::Crazy);
    observed
        .payload
        .insert("content".into(), json!("known event"));
    let mut memory = EventDraft::new(EventKind::CharacterMemoryChanged);
    memory.actor = Some("Crazy".into());
    memory.observers.insert(CharacterId::Crazy);
    memory.payload.insert(
        "change".into(),
        json!(crate::memory::MemoryChange::Upsert(
            crate::memory::MemoryRecord {
                id: crate::memory::MemoryId::new(),
                priority: crate::memory::MemoryPriority::High,
                content: "known memory".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            }
        )),
    );
    let mut private = EventDraft::new(EventKind::CharacterObservation);
    private.actor = Some("Kind".into());
    private.observers.insert(CharacterId::Kind);
    private
        .payload
        .insert("content".into(), json!("private event"));
    inner
        .append(
            branch,
            0,
            "setup",
            json!({}),
            vec![observed, memory, private],
        )
        .unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    timeline.store = Box::new(Counted {
        inner,
        reads: Arc::clone(&reads),
    });
    let capture = timeline
        .capture_context(CharacterId::Crazy, ContextPosition::Latest)
        .unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert_eq!(capture.history.at.through, 3);
    assert_eq!(capture.state.accepted_replies, 0);
    assert_eq!(capture.memories.list()[0].content, "known memory");
    let rendered = format!("{:?}", capture.view.context_entries());
    assert!(rendered.contains("known event"));
    assert!(!rendered.contains("private event"));
}

#[cfg(not(feature = "sqlite-system"))]
#[test]
fn persistent_timeline_never_falls_back_to_transient_storage() {
    let result = SessionTimeline::from_config(TimelineConfig::Persistent {
        database: "unused.sqlite3".into(),
    });
    assert!(matches!(result, Err(TimelineError::Storage(_))));
}
