use neuromita::{
    character::CharacterId,
    memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord},
    timeline::domain::{
        BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore,
    },
};
use serde_json::{Value, json};

fn effect(kind: EventKind, payload: Value) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.actor = Some(CharacterId::Crazy.to_string());
    draft.observers.insert(CharacterId::Crazy);
    draft.payload = payload.as_object().unwrap().clone();
    draft
}

fn push(store: &mut dyn TimelineRepository, branch: BranchId, drafts: Vec<EventDraft>) -> bool {
    let head = store.branch(branch).unwrap().head_sequence;
    let key = uuid::Uuid::new_v4().to_string();
    store.append(branch, head, &key, json!({}), drafts).is_ok()
}

fn check(store: &mut dyn TimelineRepository) {
    let branch = BranchId::new();
    store.create_session(SessionId::new(), branch).unwrap();
    for kind in [
        EventKind::CharacterStateChanged,
        EventKind::CharacterWorkingStateReplaced,
        EventKind::CharacterMemoryChanged,
    ] {
        assert!(!push(store, branch, vec![effect(kind, json!({}))]));
        assert!(!push(
            store,
            branch,
            vec![effect(kind, json!({"wrong":{}}))]
        ));
    }
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterStateChanged,
            json!({"state":{"attitude":101,"boredom":10,"stress":5}})
        )]
    ));
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterStateChanged,
            json!({"state":{"attitude":50,"boredom":10,"stress":5,"extra":true}})
        )]
    ));
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterWorkingStateReplaced,
            json!({"working_state":{"focus":"a".repeat(601)}})
        )]
    ));
    let mut public = effect(
        EventKind::CharacterWorkingStateReplaced,
        json!({"working_state":{}}),
    );
    public.observers.insert(CharacterId::Kind);
    assert!(!push(store, branch, vec![public]));
    assert_eq!(store.branch(branch).unwrap().head_sequence, 0);

    let record = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::High,
        content: "Player likes tea".into(),
        kind: neuromita::memory::MemoryKind::Fact,
        state: neuromita::memory::MemoryState::Active,
        protected: false,
        expires_at_unix_ms: None,
    };
    let memory =
        |change: MemoryChange| effect(EventKind::CharacterMemoryChanged, json!({"change":change}));
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(record.clone()))]
    ));
    let before = store.branch(branch).unwrap().head_sequence;
    let mut duplicate = record.clone();
    duplicate.id = MemoryId::new();
    duplicate.content = " PLAYER  likes TEA ".into();
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(duplicate))]
    ));
    assert!(!push(
        store,
        branch,
        vec![
            memory(MemoryChange::Delete(record.id)),
            memory(MemoryChange::Delete(record.id))
        ]
    ));
    assert_eq!(store.branch(branch).unwrap().head_sequence, before);
    assert_eq!(store.world(branch).unwrap().len(), before as usize);
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));

    let mut batch = Vec::new();
    for index in 0..257 {
        batch.push(memory(MemoryChange::Upsert(MemoryRecord {
            id: MemoryId::new(),
            priority: MemoryPriority::Normal,
            content: format!("memory {index}"),
            kind: neuromita::memory::MemoryKind::Fact,
            state: neuromita::memory::MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        })));
    }
    let before = store.branch(branch).unwrap().head_sequence;
    assert!(!push(store, branch, batch));
    assert_eq!(store.branch(branch).unwrap().head_sequence, before);

    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let id = checkpoint.id;
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(record.clone())), checkpoint]
    ));
    let child = BranchId::new();
    store.fork(branch, id, child).unwrap();
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(push(
        store,
        child,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(!push(
        store,
        child,
        vec![memory(MemoryChange::Delete(record.id))]
    ));

    let target = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::Normal,
        content: "merge target".into(),
        kind: neuromita::memory::MemoryKind::Fact,
        state: neuromita::memory::MemoryState::Active,
        protected: false,
        expires_at_unix_ms: None,
    };
    let source = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::High,
        content: "merge source".into(),
        kind: neuromita::memory::MemoryKind::Fact,
        state: neuromita::memory::MemoryState::Active,
        protected: false,
        expires_at_unix_ms: None,
    };
    assert!(push(
        store,
        branch,
        vec![
            memory(MemoryChange::Upsert(target.clone())),
            memory(MemoryChange::Upsert(source.clone()))
        ]
    ));
    let mut merged = target;
    merged.content = source.content;
    assert!(push(
        store,
        branch,
        vec![
            memory(MemoryChange::Upsert(merged)),
            memory(MemoryChange::Delete(source.id))
        ]
    ));
}

#[test]
fn all_writers_validate_canonical_effects_and_atomic_memory_batches() {
    check(&mut TimelineStore::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_uses_the_same_canonical_validation_inside_the_transaction() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!(
        "neuromita-effects-{}.sqlite3",
        uuid::Uuid::new_v4()
    ));
    let mut repository = SqliteTimelineRepository::open(&path).unwrap();
    check(&mut repository);
    drop(repository);
    std::fs::remove_file(path).unwrap();
}

fn check_lifecycle_effects(store: &mut dyn TimelineRepository) {
    use neuromita::memory::{MemoryIsland, MemoryKind, MemoryState};
    let branch = BranchId::new();
    store.create_session(SessionId::new(), branch).unwrap();
    let mut island =
        MemoryRecord::fact(MemoryId::new(), MemoryPriority::Normal, "Player likes tea");
    island.kind = MemoryKind::Island(MemoryIsland::Preferences);
    let encode = |record: MemoryRecord| {
        effect(
            EventKind::CharacterMemoryChanged,
            json!({"change": MemoryChange::Upsert(record)}),
        )
    };
    assert!(push(store, branch, vec![encode(island.clone())]));
    let mut duplicate = island.clone();
    duplicate.id = MemoryId::new();
    assert!(!push(store, branch, vec![encode(duplicate)]));
    let mut forgotten = island.clone();
    forgotten.state = MemoryState::Forgotten;
    assert!(push(store, branch, vec![encode(forgotten.clone())]));
    let mut restored = forgotten;
    restored.state = MemoryState::Active;
    assert!(push(store, branch, vec![encode(restored)]));
    let mut deleted = island;
    deleted.state = MemoryState::Deleted;
    assert!(push(store, branch, vec![encode(deleted)]));
}

#[test]
fn canonical_memory_lifecycle_is_validated_in_memory() {
    check_lifecycle_effects(&mut TimelineStore::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn canonical_memory_lifecycle_is_validated_in_sqlite() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path =
        std::env::temp_dir().join(format!("neuromita-islands-{}.sqlite", uuid::Uuid::new_v4()));
    let mut repository = SqliteTimelineRepository::open(&path).unwrap();
    check_lifecycle_effects(&mut repository);
    drop(repository);
    std::fs::remove_file(path).unwrap();
}

fn check_protected_effects(store: &mut dyn TimelineRepository) {
    let branch = BranchId::new();
    store.create_session(SessionId::new(), branch).unwrap();
    let mut record = MemoryRecord::fact(MemoryId::new(), MemoryPriority::Critical, "protected");
    let change =
        |value: MemoryChange| effect(EventKind::CharacterMemoryChanged, json!({"change": value}));
    assert!(push(
        store,
        branch,
        vec![change(MemoryChange::Upsert(record.clone()))]
    ));
    assert!(push(
        store,
        branch,
        vec![change(MemoryChange::SetProtection {
            id: record.id,
            protected: true,
        })]
    ));
    let before = store.branch(branch).unwrap().head_sequence;
    record.state = neuromita::memory::MemoryState::Forgotten;
    assert!(!push(
        store,
        branch,
        vec![change(MemoryChange::Upsert(record.clone()))]
    ));
    assert!(!push(
        store,
        branch,
        vec![change(MemoryChange::Delete(record.id))]
    ));
    assert_eq!(store.branch(branch).unwrap().head_sequence, before);
    assert!(push(
        store,
        branch,
        vec![
            change(MemoryChange::SetProtection {
                id: record.id,
                protected: false
            }),
            change(MemoryChange::Upsert(record)),
        ]
    ));
}

#[test]
fn protection_is_canonical_and_atomic_in_memory() {
    check_protected_effects(&mut TimelineStore::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn protection_is_canonical_and_atomic_in_sqlite() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!(
        "neuromita-protection-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut repository = SqliteTimelineRepository::open(&path).unwrap();
    check_protected_effects(&mut repository);
    drop(repository);
    std::fs::remove_file(path).unwrap();
}
