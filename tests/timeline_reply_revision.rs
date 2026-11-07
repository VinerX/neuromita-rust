use neuromita::{
    character::CharacterId,
    timeline::domain::{
        AppendGuard, BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId,
        TimelineError, TimelineRepository, TimelineStore,
    },
};
use serde_json::json;
use uuid::Uuid;

fn player_message() -> EventDraft {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.extend(CharacterId::ALL);
    draft.payload.insert("content".into(), json!("hello"));
    draft
}

fn accepted_message(character: CharacterId, target: neuromita::timeline::EventId) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::CharacterMessage);
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft
        .payload
        .insert("request_id".into(), json!(Uuid::new_v4().to_string()));
    draft.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: target,
    });
    draft
}

fn guarded_reply(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    selection_generation: u64,
    character: CharacterId,
    target: neuromita::timeline::EventId,
    expected_revision: u64,
) -> Result<(), TimelineError> {
    let head = repo.branch(branch)?.head_sequence;
    repo.append_guarded(
        branch,
        head,
        &format!("accepted:{}", Uuid::new_v4()),
        json!({}),
        vec![accepted_message(character, target)],
        AppendGuard {
            selection_generation,
            context: Some((character, 0, Some(target))),
            accepted_reply_revision: Some((character, expected_revision)),
        },
    )?;
    Ok(())
}

fn acceptance_revision_contract(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    generation: u64,
) {
    let source = player_message();
    let target = source.id;
    repo.append(branch, 0, "source", json!({}), vec![source])
        .unwrap();

    guarded_reply(repo, branch, generation, CharacterId::Crazy, target, 0).unwrap();
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 2);

    assert_eq!(
        guarded_reply(repo, branch, generation, CharacterId::Crazy, target, 0),
        Err(TimelineError::ConcurrentWrite),
    );
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 2);

    guarded_reply(repo, branch, generation, CharacterId::Kind, target, 0).unwrap();
    guarded_reply(repo, branch, generation, CharacterId::Crazy, target, 1).unwrap();
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 4);

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Crazy);
    repo.append(branch, 4, "reset", json!({}), vec![reset])
        .unwrap();
    let stale = guarded_reply(repo, branch, generation, CharacterId::Crazy, target, 0);
    assert_eq!(stale, Err(TimelineError::ConcurrentWrite));
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 5);
}

#[test]
fn in_memory_reply_revision_is_character_scoped_and_canonical() {
    let mut store = TimelineStore::new();
    let branch = BranchId::new();
    store.create_session(SessionId::new(), branch).unwrap();
    acceptance_revision_contract(&mut store, branch, 1);
}

#[test]
fn reply_revision_follows_fork_cutoff_not_parent_future() {
    let mut store = TimelineStore::new();
    let parent = BranchId::new();
    let child = BranchId::new();
    store.create_session(SessionId::new(), parent).unwrap();
    let source = player_message();
    let target = source.id;
    store
        .append(parent, 0, "source", json!({}), vec![source])
        .unwrap();
    guarded_reply(&mut store, parent, 1, CharacterId::Crazy, target, 0).unwrap();

    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    store
        .append(parent, 2, "checkpoint", json!({}), vec![checkpoint])
        .unwrap();
    store.fork(parent, checkpoint_id, child).unwrap();

    guarded_reply(&mut store, parent, 1, CharacterId::Crazy, target, 1).unwrap();
    assert_eq!(
        guarded_reply(&mut store, child, 1, CharacterId::Crazy, target, 0),
        Err(TimelineError::ConcurrentWrite)
    );
    guarded_reply(&mut store, child, 1, CharacterId::Crazy, target, 1).unwrap();
    assert_eq!(store.branch(parent).unwrap().head_sequence, 4);
    assert_eq!(store.branch(child).unwrap().head_sequence, 5);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn independent_sqlite_writers_cannot_accept_the_same_character_revision() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };

    let path = std::env::temp_dir().join(format!("timeline-reply-cas-{}.db", Uuid::new_v4()));
    let branch = BranchId::new();
    let session = SessionId::new();
    let mut first = SqliteTimelineRepository::open(&path).unwrap();
    first.create_session(session, branch).unwrap();
    let selection = first.select_branch(branch, None).unwrap();
    let mut second = SqliteTimelineRepository::open(&path).unwrap();

    let source = player_message();
    let target = source.id;
    first
        .append(branch, 0, "source", json!({}), vec![source])
        .unwrap();
    guarded_reply(
        &mut first,
        branch,
        selection.generation,
        CharacterId::Crazy,
        target,
        0,
    )
    .unwrap();

    assert_eq!(
        guarded_reply(
            &mut second,
            branch,
            selection.generation,
            CharacterId::Crazy,
            target,
            0
        ),
        Err(TimelineError::ConcurrentWrite),
    );
    assert_eq!(second.branch(branch).unwrap().head_sequence, 2);
    guarded_reply(
        &mut second,
        branch,
        selection.generation,
        CharacterId::Kind,
        target,
        0,
    )
    .unwrap();
    drop(first);
    drop(second);

    let mut reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 3);
    guarded_reply(
        &mut reopened,
        branch,
        selection.generation,
        CharacterId::Crazy,
        target,
        1,
    )
    .unwrap();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 4);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
