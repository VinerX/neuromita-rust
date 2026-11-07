use neuromita::{
    character::CharacterId,
    timeline::{
        conversation::{
            ConversationMessage, ConversationSpeaker, Message, append_conversation_message,
        },
        domain::{
            AppendGuard, BranchId, CausalLink, CausalRelation, Event, EventDraft, EventKind,
            SessionId, TimelineError, TimelineRepository, TimelineStore,
        },
        view::TimelineMessageQueries,
    },
};
use serde_json::json;

fn append(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    draft: EventDraft,
) -> Result<(), TimelineError> {
    let head = repo.branch(branch)?.head_sequence;
    repo.append(
        branch,
        head,
        &format!("knowledge:{}", draft.id.0),
        json!({}),
        vec![draft],
    )?;
    Ok(())
}

fn contract(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
) -> (
    BranchId,
    neuromita::timeline::EventId,
    neuromita::timeline::EventId,
) {
    let mut x = EventDraft::new(EventKind::UnityEvent);
    x.observers.insert(CharacterId::Crazy);
    let x_id = x.id;
    append(repo, branch, x).unwrap();

    let mut y = ConversationMessage::new(
        "y",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!("I saw X"),
        [CharacterId::Kind],
    );
    y.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: x_id,
    });
    assert_eq!(Event::kind(&y), EventKind::CharacterMessage);
    assert!(Event::witnesses(&y).contains(&CharacterId::Crazy));
    assert!(Event::witnesses(&y).contains(&CharacterId::Kind));
    assert_eq!(
        Message::speaker(&y),
        ConversationSpeaker::Character(CharacterId::Crazy)
    );
    let y_id = y.id;
    append_conversation_message(
        repo,
        branch,
        1,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        y,
    )
    .unwrap();

    let mut z = ConversationMessage::new(
        "z",
        ConversationSpeaker::Character(CharacterId::Kind),
        json!("A told me about X"),
        [CharacterId::Crazy],
    );
    z.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: y_id,
    });
    let z_id = z.id;
    append_conversation_message(
        repo,
        branch,
        2,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        z,
    )
    .unwrap();

    assert!(repo.knows(branch, CharacterId::Crazy, x_id, None).unwrap());
    assert!(!repo.knows(branch, CharacterId::Kind, x_id, None).unwrap());
    assert!(repo.knows(branch, CharacterId::Kind, y_id, None).unwrap());
    assert!(repo.knows(branch, CharacterId::Kind, z_id, None).unwrap());
    let view = TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(view.count(), 2);

    let mut impossible = EventDraft::new(EventKind::CharacterMessage);
    impossible.actor = Some(CharacterId::Kind.to_string());
    impossible.observers.insert(CharacterId::Kind);
    impossible.links.push(CausalLink {
        relation: CausalRelation::ReactTo,
        event_id: x_id,
    });
    assert!(matches!(
        append(repo, branch, impossible),
        Err(TimelineError::Invalid(_))
    ));
    let mut permitted = EventDraft::new(EventKind::CharacterMessage);
    permitted.actor = Some(CharacterId::Kind.to_string());
    permitted.observers.insert(CharacterId::Kind);
    permitted.links.push(CausalLink {
        relation: CausalRelation::ReactTo,
        event_id: y_id,
    });
    append(repo, branch, permitted).unwrap();

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Kind);
    append(repo, branch, reset).unwrap();
    assert!(!repo.knows(branch, CharacterId::Kind, y_id, None).unwrap());
    assert!(!repo.knows(branch, CharacterId::Kind, z_id, None).unwrap());

    let mut after_reset = ConversationMessage::new(
        "after-reset",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!("Remember X?"),
        [CharacterId::Kind],
    );
    after_reset.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: x_id,
    });
    let new_id = after_reset.id;
    let head = repo.branch(branch).unwrap().head_sequence;
    append_conversation_message(
        repo,
        branch,
        head,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        after_reset,
    )
    .unwrap();
    assert!(!repo.knows(branch, CharacterId::Kind, x_id, None).unwrap());
    assert!(!repo.knows(branch, CharacterId::Kind, y_id, None).unwrap());
    assert!(repo.knows(branch, CharacterId::Kind, new_id, None).unwrap());
    (branch, x_id, new_id)
}

#[test]
fn only_direct_events_are_known_in_memory() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    contract(&mut repo, branch);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn only_direct_events_are_known_in_sqlite_across_reopen() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };
    let path = std::env::temp_dir().join(format!(
        "neuromita-knowledge-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut repo = SqliteTimelineRepository::open(&path).unwrap();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    repo.select_branch(branch, None).unwrap();
    let (branch, old_id, new_id) = contract(&mut repo, branch);
    drop(repo);
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert!(
        !reopened
            .knows(branch, CharacterId::Kind, old_id, None)
            .unwrap()
    );
    assert!(
        reopened
            .knows(branch, CharacterId::Kind, new_id, None)
            .unwrap()
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
