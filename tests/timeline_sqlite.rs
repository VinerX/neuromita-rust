#![cfg(feature = "sqlite-system")]

use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{
            BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId, TimelineError,
            TimelineRepository, TimelineStore,
        },
        storage::SqliteTimelineRepository,
    },
};
use serde_json::json;
use uuid::Uuid;

struct DatabaseFile(PathBuf);

impl DatabaseFile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("neuromita-timeline-{}.db", Uuid::new_v4()));
        Self(path)
    }
    fn open(&self) -> SqliteTimelineRepository {
        SqliteTimelineRepository::open(&self.0).unwrap()
    }
}
impl Drop for DatabaseFile {
    fn drop(&mut self) {
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
    }
}

fn mk(kind: EventKind, observers: &[CharacterId]) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.observers.extend(observers.iter().copied());
    draft
}
fn append<T: TimelineRepository + ?Sized>(
    repo: &mut T,
    id: BranchId,
    key: &str,
    draft: EventDraft,
) {
    let head = repo.branch(id).unwrap().head_sequence;
    repo.append(id, head, key, json!({"operation": key}), vec![draft])
        .unwrap();
}

fn contract(repo: &mut dyn TimelineRepository) {
    let session = SessionId::new();
    let root = BranchId::new();
    let info = repo.create_session(session, root).unwrap();
    assert_eq!(repo.create_session(session, root), Ok(info));
    let mut first = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    first
        .payload
        .insert("value".into(), json!({"nested": [2, 4]}));
    let first_id = first.id;
    let original = repo
        .append(root, 0, "first", json!({"a": 1}), vec![first.clone()])
        .unwrap();
    let mut replay = first;
    replay.occurred_at_unix_ms += 1000;
    append(
        repo,
        root,
        "advance",
        mk(EventKind::UnityEvent, &[CharacterId::Crazy]),
    );
    assert_eq!(
        repo.append(root, 0, "first", json!({"a": 1}), vec![replay.clone()]),
        Ok(original)
    );
    assert_eq!(
        repo.append(root, 0, "first", json!({"a": 2}), vec![replay]),
        Err(TimelineError::Conflict)
    );
    assert!(
        repo.knows(root, CharacterId::Crazy, first_id, None)
            .unwrap()
    );
    assert!(!repo.knows(root, CharacterId::Kind, first_id, None).unwrap());
    let mut retelling = mk(EventKind::PlayerMessage, &[CharacterId::Kind]);
    retelling.actor = Some("player".into());
    retelling.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: first_id,
    });
    let retold_id = retelling.id;
    append(repo, root, "retelling", retelling);
    assert!(!repo.knows(root, CharacterId::Kind, first_id, None).unwrap());
    assert!(
        repo.knows(root, CharacterId::Kind, retold_id, None)
            .unwrap()
    );
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint
        .payload
        .insert("snapshot".into(), json!({"player": {"level": 7}}));
    let checkpoint_id = checkpoint.id;
    append(repo, root, "checkpoint", checkpoint);
    let child = BranchId::new();
    let info = repo.fork(root, checkpoint_id, child).unwrap();
    assert_eq!(repo.fork(root, checkpoint_id, child), Ok(info));
    let old_future = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    let old_future_id = old_future.id;
    append(repo, root, "future", old_future);
    assert!(
        !repo
            .world(child)
            .unwrap()
            .iter()
            .any(|fact| fact.draft.id == old_future_id)
    );
    assert!(
        repo.world(root)
            .unwrap()
            .iter()
            .any(|fact| fact.draft.id == old_future_id)
    );
    assert_eq!(repo.world(child).unwrap().len(), 5);
}

#[test]
fn in_memory_and_sqlite_obey_the_same_domain_contract() {
    contract(&mut TimelineStore::new());
    let db = DatabaseFile::new();
    contract(&mut db.open());
}

#[test]
fn committed_facts_and_receipts_survive_reopen() {
    let db = DatabaseFile::new();
    let session = SessionId::new();
    let branch = BranchId::new();
    let mut original = db.open();
    original.create_session(session, branch).unwrap();
    let draft = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    let committed = original
        .append(branch, 0, "once", json!({"id": 1}), vec![draft.clone()])
        .unwrap();
    drop(original);
    let mut reopened = db.open();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
    assert_eq!(reopened.world(branch).unwrap(), committed);
    assert_eq!(
        reopened.append(branch, 0, "once", json!({"id": 1}), vec![draft]),
        Ok(committed)
    );
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
    assert_eq!(
        reopened.create_session(session, BranchId::new()),
        Err(TimelineError::Conflict)
    );
}

#[test]
fn invalid_batch_cannot_leave_partial_events_or_receipts() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), branch).unwrap();
    let invalid = EventDraft::new(EventKind::CharacterMessage);
    assert!(
        repo.append(
            branch,
            0,
            "failed",
            json!({}),
            vec![mk(EventKind::UnityEvent, &[]), invalid]
        )
        .is_err()
    );
    drop(repo);
    let mut reopened = db.open();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 0);
    assert!(reopened.world(branch).unwrap().is_empty());
    assert_eq!(
        reopened.append(branch, 0, "failed", json!({}), vec![]),
        Ok(Vec::new())
    );
}

#[test]
fn two_independent_connections_cannot_commit_the_same_expected_head() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    db.open().create_session(SessionId::new(), branch).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for name in ["a", "b"] {
        let barrier = Arc::clone(&barrier);
        let path = db.0.clone();
        threads.push(thread::spawn(move || {
            let mut repo = SqliteTimelineRepository::open(path).unwrap();
            barrier.wait();
            repo.append(
                branch,
                0,
                name,
                json!({}),
                vec![mk(EventKind::UnityEvent, &[])],
            )
        }));
    }
    let results = threads
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(TimelineError::ConcurrentWrite))
            .count(),
        1
    );
    assert_eq!(db.open().branch(branch).unwrap().head_sequence, 1);
}

#[test]
fn timeline_tables_do_not_conflict_with_other_application_tables() {
    let db = DatabaseFile::new();
    let mut repo = db.open();
    let id = BranchId::new();
    repo.create_session(SessionId::new(), id).unwrap();
    assert_eq!(db.open().branch(id).unwrap().head_sequence, 0);
}

#[test]
fn selection_is_durable_cas_and_reselection_invalidates_previous_pins() {
    use neuromita::timeline::selection::TimelineSelectionRepository;
    let db = DatabaseFile::new();
    let root = BranchId::new();
    let mut first = db.open();
    let mut second = db.open();
    first.create_session(SessionId::new(), root).unwrap();
    assert_eq!(first.selection().unwrap(), None);
    let initial = first.select_branch(root, None).unwrap();
    assert_eq!(second.selection().unwrap(), Some(initial));
    assert_eq!(
        second.select_branch(root, None),
        Err(TimelineError::ConcurrentWrite)
    );
    let next = second.select_branch(root, Some(initial)).unwrap();
    assert_eq!(next.generation, initial.generation + 1);
    assert_eq!(first.selection().unwrap(), Some(next));
    drop(first);
    drop(second);
    assert_eq!(db.open().selection().unwrap(), Some(next));
}

fn page_contract(
    repo: &mut dyn TimelineRepository,
    root: BranchId,
    session: SessionId,
) -> (
    Vec<Vec<neuromita::timeline::domain::TimelineFact>>,
    Vec<Vec<neuromita::timeline::domain::TimelineFact>>,
) {
    repo.create_session(session, root).unwrap();
    for i in 0..9 {
        let observers = if i % 2 == 0 {
            &[CharacterId::Crazy][..]
        } else {
            &[CharacterId::Kind][..]
        };
        append(
            repo,
            root,
            &format!("event:{i}"),
            mk(EventKind::UnityEvent, observers),
        );
    }
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(repo, root, "checkpoint", checkpoint);
    let child = BranchId(Uuid::from_u128(0x11111111111111111111111111111111));
    repo.fork(root, checkpoint_id, child).unwrap();
    for i in 0..4 {
        append(
            repo,
            child,
            &format!("child:{i}"),
            mk(EventKind::UnityEvent, &[CharacterId::Crazy]),
        );
    }
    append(
        repo,
        root,
        "unrelated parent future",
        mk(EventKind::UnityEvent, &[CharacterId::Kind]),
    );
    let mut all = Vec::new();
    let mut observed = Vec::new();
    for after in [0, 1, 4, 9, 10, 11, 12, 13] {
        for through in [None, Some(2), Some(9), Some(10), Some(13), Some(15)] {
            for limit in [1, 2, 4, 100] {
                let result = repo.read(child, after, through, limit, None);
                let witness = repo.read(child, after, through, limit, Some(CharacterId::Crazy));
                if through.is_some_and(|upper| upper > 15) {
                    assert!(result.is_err());
                    assert!(witness.is_err());
                    continue;
                }
                all.push(result.unwrap());
                observed.push(witness.unwrap());
            }
        }
    }
    (all, observed)
}

#[test]
fn indexed_sqlite_read_matches_in_memory_across_nested_cutoffs_and_direct_witnesses() {
    let root = BranchId::new();
    let session = SessionId::new();
    let (expected, observed) = page_contract(&mut TimelineStore::new(), root, session);
    let db = DatabaseFile::new();
    let (actual, sqlite_observed) = page_contract(&mut db.open(), root, session);
    let signature = |pages: Vec<Vec<neuromita::timeline::domain::TimelineFact>>| {
        pages
            .into_iter()
            .map(|page| {
                page.into_iter()
                    .map(|fact| (fact.sequence, fact.draft.kind, fact.draft.observers))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(signature(actual), signature(expected));
    assert_eq!(signature(sqlite_observed), signature(observed));
}

#[test]
fn workspace_pins_validate_against_sqlite_without_a_timeline_store_dependency() {
    use neuromita::timeline::selection::SessionWorkspace;
    let db = DatabaseFile::new();
    let root = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), root).unwrap();
    let mut workspace = SessionWorkspace::default();
    workspace.select(&repo, root, None).unwrap();
    let pin = workspace.capture(&repo, CharacterId::Crazy, None).unwrap();
    append(&mut repo, root, "unrelated", mk(EventKind::UnityEvent, &[]));
    assert_eq!(workspace.assert_current(&repo, pin), Ok(()));
    append(
        &mut repo,
        root,
        "reset",
        mk(EventKind::CharacterContextReset, &[CharacterId::Crazy]),
    );
    assert_eq!(
        workspace.assert_current(&repo, pin),
        Err(TimelineError::ConcurrentWrite)
    );
}

#[test]
fn cached_append_detects_external_writers_and_rebuilds_authoritative_state() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    let mut first = db.open();
    first.create_session(SessionId::new(), branch).unwrap();
    append(&mut first, branch, "first", mk(EventKind::UnityEvent, &[]));
    append(&mut first, branch, "second", mk(EventKind::UnityEvent, &[]));
    let mut second = db.open();
    append(
        &mut second,
        branch,
        "external",
        mk(EventKind::UnityEvent, &[]),
    );
    assert_eq!(
        first.append(
            branch,
            2,
            "stale",
            json!({}),
            vec![mk(EventKind::UnityEvent, &[])]
        ),
        Err(TimelineError::ConcurrentWrite)
    );
    append(
        &mut first,
        branch,
        "current",
        mk(EventKind::UnityEvent, &[]),
    );
    assert_eq!(db.open().branch(branch).unwrap().head_sequence, 4);
    assert_eq!(first.world(branch).unwrap(), second.world(branch).unwrap());
}

#[test]
fn cached_receipts_survive_reopen_and_stale_retry_returns_original_result() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), branch).unwrap();
    let original = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    let first = repo
        .append(
            branch,
            0,
            "idempotent",
            json!({"id":1}),
            vec![original.clone()],
        )
        .unwrap();
    append(
        &mut repo,
        branch,
        "advanced",
        mk(EventKind::UnityEvent, &[]),
    );
    assert_eq!(
        repo.append(
            branch,
            0,
            "idempotent",
            json!({"id":1}),
            vec![original.clone()]
        ),
        Ok(first.clone())
    );
    drop(repo);
    let mut reopened = db.open();
    assert_eq!(
        reopened.append(branch, 0, "idempotent", json!({"id":1}), vec![original]),
        Ok(first)
    );
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 2);
}

#[test]
fn branch_scoped_append_cache_keeps_sibling_worlds_and_receipts_independent() {
    let db = DatabaseFile::new();
    let root = BranchId::new();
    let child = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), root).unwrap();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    repo.append(root, 0, "checkpoint", json!({}), vec![checkpoint])
        .unwrap();
    repo.fork(root, checkpoint_id, child).unwrap();
    let mut root_event = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    root_event.external_message_id = Some("same-external".into());
    append(&mut repo, root, "parent-future", root_event.clone());
    let mut child_event = mk(EventKind::UnityEvent, &[CharacterId::Kind]);
    child_event.external_message_id = Some("same-external".into());
    let child_events = repo
        .append(
            child,
            2,
            "child-future",
            json!({}),
            vec![child_event.clone()],
        )
        .unwrap();
    assert_eq!(child_events[0].sequence, 3);
    assert!(
        repo.world(child)
            .unwrap()
            .iter()
            .all(|fact| fact.draft.id != root_event.id)
    );
    assert_eq!(
        repo.append(child, 2, "child-future", json!({}), vec![child_event]),
        Ok(child_events)
    );
    let fresh = mk(EventKind::UnityEvent, &[CharacterId::Crazy]);
    let root_result = repo
        .append(root, 2, "root-new", json!({}), vec![fresh.clone()])
        .unwrap();
    assert_eq!(root_result[0].sequence, 3);
    assert_eq!(db.open().world(root).unwrap().len(), 3);
    assert_eq!(db.open().world(child).unwrap().len(), 3);
    assert_eq!(
        repo.append(root, 2, "root-new", json!({}), vec![fresh]),
        Ok(root_result)
    );
}

#[test]
fn cold_index_matches_domain_knowledge_after_reset_and_cross_branch_disclosure() {
    let db = DatabaseFile::new();
    let session = SessionId::new();
    let root = BranchId::new();
    let mut sql = db.open();
    let mut memory = TimelineStore::new();
    for repo in [&mut sql as &mut dyn TimelineRepository, &mut memory] {
        repo.create_session(session, root).unwrap();
    }
    let mut secret = mk(EventKind::PlayerMessage, &[CharacterId::Crazy]);
    secret.actor = Some("Player".into());
    secret.external_message_id = Some("secret-from-unity".into());
    let secret_id = secret.id;
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    let child = BranchId::new();
    let sibling = BranchId::new();
    for repo in [&mut sql as &mut dyn TimelineRepository, &mut memory] {
        repo.append(root, 0, "secret", json!({}), vec![secret.clone()])
            .unwrap();
        repo.append(root, 1, "checkpoint", json!({}), vec![checkpoint.clone()])
            .unwrap();
        repo.fork(root, checkpoint_id, child).unwrap();
        repo.fork(root, checkpoint_id, sibling).unwrap();
    }
    let mut retelling = mk(EventKind::PlayerMessage, &[CharacterId::Kind]);
    retelling.actor = Some("player".into());
    retelling.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: secret_id,
    });
    let retold_id = retelling.id;
    let mut reset = mk(EventKind::CharacterContextReset, &[CharacterId::Kind]);
    reset.external_message_id = Some("context-reset".into());
    for repo in [&mut sql as &mut dyn TimelineRepository, &mut memory] {
        let head = repo.branch(child).unwrap().head_sequence;
        repo.append(child, head, "retell", json!({}), vec![retelling.clone()])
            .unwrap();
        let head = repo.branch(child).unwrap().head_sequence;
        repo.append(child, head, "reset", json!({}), vec![reset.clone()])
            .unwrap();
    }
    drop(sql);
    let mut sql = db.open();
    assert_eq!(
        sql.knows(child, CharacterId::Kind, secret_id, None),
        Ok(false)
    );
    assert_eq!(
        sql.knows(sibling, CharacterId::Kind, secret_id, None),
        Ok(false)
    );
    let head = sql.branch(child).unwrap().head_sequence;
    let mut reply = mk(EventKind::CharacterMessage, &[CharacterId::Kind]);
    reply.actor = Some(CharacterId::Kind.to_string());
    reply.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: secret_id,
    });
    let mut reply_other = reply.clone();
    reply_other.id = neuromita::timeline::EventId::new();
    assert!(matches!(
        sql.append(child, head, "unknown-reply", json!({}), vec![reply.clone()]),
        Err(TimelineError::Invalid(_))
    ));
    assert!(matches!(
        sql.append(
            sibling,
            sql.branch(sibling).unwrap().head_sequence,
            "unknown-reply",
            json!({}),
            vec![reply_other]
        ),
        Err(TimelineError::Invalid(_))
    ));
    let mut new_retelling = retelling;
    new_retelling.id = neuromita::timeline::EventId::new();
    let new_retelling_id = new_retelling.id;
    let commands = vec![new_retelling];
    let expected = memory.append(child, head, "new-retelling", json!({}), commands.clone());
    let actual = sql.append(child, head, "new-retelling", json!({}), commands);
    assert_eq!(actual, expected);
    assert_eq!(
        sql.knows(child, CharacterId::Kind, secret_id, None),
        Ok(false)
    );
    assert_eq!(
        sql.knows(child, CharacterId::Kind, new_retelling_id, None),
        Ok(true)
    );
    assert_eq!(
        sql.knows(child, CharacterId::Kind, retold_id, None),
        Ok(false)
    );
    assert_eq!(
        sql.knows(sibling, CharacterId::Kind, secret_id, None),
        Ok(false)
    );
    let duplicated = mk(EventKind::UnityEvent, &[]);
    let mut duplicated = duplicated;
    duplicated.external_message_id = Some("secret-from-unity".into());
    assert_eq!(
        sql.append(
            child,
            sql.branch(child).unwrap().head_sequence,
            "duplicate-external",
            json!({}),
            vec![duplicated]
        ),
        Err(TimelineError::Conflict)
    );
}

fn historic_epoch_contract(repo: &mut dyn TimelineRepository) {
    let root = BranchId::new();
    repo.create_session(SessionId::new(), root).unwrap();
    append(
        repo,
        root,
        "root-mita-reset",
        mk(EventKind::CharacterContextReset, &[CharacterId::Crazy]),
    );
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(repo, root, "checkpoint", checkpoint);
    let fork = BranchId::new();
    let fork_start = repo.fork(root, checkpoint_id, fork).unwrap().head_sequence;
    append(
        repo,
        root,
        "root-kind-reset",
        mk(EventKind::CharacterContextReset, &[CharacterId::Kind]),
    );
    append(
        repo,
        fork,
        "fork-mita-reset",
        mk(EventKind::CharacterContextReset, &[CharacterId::Crazy]),
    );

    assert_eq!(
        repo.context_epoch_at(root, CharacterId::Crazy, Some(0)),
        Ok(0)
    );
    assert_eq!(
        repo.context_epoch_at(root, CharacterId::Crazy, Some(1)),
        Ok(1)
    );
    assert_eq!(repo.context_epoch(root, CharacterId::Kind), Ok(3));
    assert_eq!(
        repo.context_epoch_at(fork, CharacterId::Crazy, Some(1)),
        Ok(1)
    );
    assert_eq!(
        repo.context_epoch_at(fork, CharacterId::Crazy, Some(fork_start)),
        Ok(1)
    );
    assert_eq!(
        repo.context_epoch(fork, CharacterId::Crazy),
        Ok(fork_start + 1)
    );
    assert_eq!(repo.context_epoch(fork, CharacterId::Kind), Ok(0));
    assert!(
        repo.context_epoch_at(fork, CharacterId::Crazy, Some(fork_start + 2))
            .is_err()
    );
}

#[test]
fn historical_reset_epochs_match_for_in_memory_and_sqlite_forks() {
    historic_epoch_contract(&mut TimelineStore::new());
    let file = DatabaseFile::new();
    historic_epoch_contract(&mut file.open());
}
