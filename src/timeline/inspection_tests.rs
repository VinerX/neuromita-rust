use std::sync::Arc;

use serde_json::json;

use super::*;

fn inspection_contract(timeline: &mut SessionTimeline) -> ContextAnchor {
    let selected = timeline.selection();
    let root = timeline.branch;
    timeline
        .publish_player_message(CharacterId::Crazy, EventId::new(), Arc::from("shared"))
        .unwrap();
    let mut answer = EventDraft::new(EventKind::CharacterMessage);
    answer.actor = Some(CharacterId::Crazy.to_string());
    answer.observers.insert(CharacterId::Crazy);
    answer
        .payload
        .insert("content".into(), json!("accepted reply"));
    answer
        .payload
        .insert("request_id".into(), json!(RequestId::new().0.to_string()));
    timeline
        .store
        .append(root, 1, "inspection:reply", json!({}), vec![answer])
        .unwrap();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    timeline
        .store
        .append(
            root,
            2,
            "inspection:checkpoint",
            json!({}),
            vec![checkpoint],
        )
        .unwrap();
    let alternative = BranchId::new();
    let fork_info = timeline
        .store
        .fork(root, checkpoint_id, alternative)
        .unwrap();
    assert_eq!(fork_info.head_sequence, 4);
    timeline
        .publish_player_message(CharacterId::Crazy, EventId::new(), Arc::from("root-only"))
        .unwrap();
    let mut fork_message = EventDraft::new(EventKind::PlayerMessage);
    fork_message.actor = Some("player".into());
    fork_message.observers.insert(CharacterId::Crazy);
    fork_message
        .payload
        .insert("content".into(), json!("fork-only"));
    timeline
        .store
        .append(
            alternative,
            fork_info.head_sequence,
            "inspection:fork-message",
            json!({}),
            vec![fork_message],
        )
        .unwrap();
    let fork_at = ContextAnchor {
        branch_id: alternative.0,
        through: 5,
    };
    let fork = timeline
        .inspect_context_at(CharacterId::Crazy, fork_at)
        .unwrap();
    assert_eq!(fork.history.at, fork_at);
    assert_eq!(fork.view.branch, alternative);
    assert_eq!(fork.view.through, 5);
    assert_eq!(fork.view.count(), 3);
    assert_eq!(fork.view.messages()[0].content, "shared");
    assert_eq!(fork.view.messages()[1].content, "accepted reply");
    assert_eq!(fork.view.messages()[2].content, "fork-only");
    assert_eq!(fork.history.events.len(), 2);
    let other_character = timeline
        .inspect_context_at(CharacterId::Kind, fork_at)
        .unwrap();
    assert_eq!(other_character.view.count(), 0);
    assert_eq!(
        timeline.accepted_replies_at(CharacterId::Kind, fork_at),
        Ok(0)
    );
    assert_eq!(
        timeline.accepted_replies_at(CharacterId::Crazy, fork_at),
        Ok(1)
    );
    let root_at = timeline.anchor();
    let active = timeline
        .capture_context(CharacterId::Crazy, ContextPosition::Latest)
        .unwrap();
    assert_eq!(active.view.messages()[2].content, "root-only");
    assert!(
        timeline
            .capture_context(CharacterId::Crazy, ContextPosition::At(fork_at))
            .is_err()
    );

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Crazy);
    timeline
        .store
        .append(alternative, 5, "inspection:reset", json!({}), vec![reset])
        .unwrap();
    let forgotten = timeline
        .inspect_context_at(
            CharacterId::Crazy,
            ContextAnchor {
                branch_id: alternative.0,
                through: 6,
            },
        )
        .unwrap();
    assert_eq!(forgotten.view.count(), 0);
    assert_eq!(fork.view.count(), 3);
    assert_eq!(
        timeline.accepted_replies_at(
            CharacterId::Crazy,
            ContextAnchor {
                branch_id: alternative.0,
                through: 6,
            },
        ),
        Ok(1)
    );
    assert_eq!(
        timeline
            .inspect_context_at(CharacterId::Crazy, fork_at)
            .unwrap()
            .view
            .count(),
        3
    );
    assert_eq!(
        timeline
            .inspect_context_at(CharacterId::Crazy, root_at)
            .unwrap()
            .view
            .messages()[2]
            .content,
        "root-only"
    );
    assert!(
        timeline
            .inspect_context_at(
                CharacterId::Crazy,
                ContextAnchor {
                    branch_id: alternative.0,
                    through: 100,
                },
            )
            .is_err()
    );
    let unrelated = BranchId::new();
    timeline
        .store
        .create_session(SessionId::new(), unrelated)
        .unwrap();
    assert!(
        timeline
            .inspect_context_at(
                CharacterId::Crazy,
                ContextAnchor {
                    branch_id: unrelated.0,
                    through: 0,
                },
            )
            .is_err()
    );
    assert_eq!(timeline.selection(), selected);
    fork_at
}

#[test]
fn saved_branch_is_readable_without_switching_active_session() {
    let mut timeline = SessionTimeline::new();
    inspection_contract(&mut timeline);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn saved_branch_remains_readable_after_sqlite_reopen() {
    let path = std::env::temp_dir().join(format!(
        "neuromita-branch-inspection-{}.db",
        uuid::Uuid::new_v4()
    ));
    let session = SessionId::new();
    let root = BranchId::new();
    let fork_at = {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        inspection_contract(&mut timeline)
    };
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection().branch_id, root);
    let snapshot = reopened
        .inspect_context_at(CharacterId::Crazy, fork_at)
        .unwrap();
    assert_eq!(snapshot.view.messages()[2].content, "fork-only");
    assert_eq!(
        reopened.accepted_replies_at(CharacterId::Crazy, fork_at),
        Ok(1)
    );
    assert_eq!(reopened.selection().branch_id, root);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn historical_reset_boundary_contract(
    timeline: &mut SessionTimeline,
) -> (ContextAnchor, ContextAnchor) {
    timeline
        .publish_player_message(
            CharacterId::Crazy,
            EventId::new(),
            Arc::from("forgotten secret"),
        )
        .unwrap();
    let before_reset = timeline.anchor();
    let selection = timeline.selection();
    timeline
        .reset_character_context(selection, CharacterId::Crazy, EventId::new())
        .unwrap();
    let after_mita_reset = timeline.anchor();

    assert_eq!(
        timeline
            .inspect_context_at(CharacterId::Crazy, before_reset)
            .unwrap()
            .view
            .messages()[0]
            .content,
        "forgotten secret"
    );
    assert!(
        timeline
            .capture_context(CharacterId::Crazy, ContextPosition::At(before_reset))
            .is_err()
    );
    assert!(
        timeline
            .capture_context(CharacterId::Kind, ContextPosition::At(before_reset))
            .is_ok()
    );
    assert!(
        timeline
            .capture_context(CharacterId::Crazy, ContextPosition::At(after_mita_reset))
            .is_ok()
    );
    assert!(
        timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap()
            .view
            .messages()
            .is_empty()
    );

    timeline
        .reset_character_context(selection, CharacterId::Kind, EventId::new())
        .unwrap();
    assert!(
        timeline
            .capture_context(CharacterId::Kind, ContextPosition::At(before_reset))
            .is_err()
    );
    assert!(
        timeline
            .capture_context(CharacterId::Crazy, ContextPosition::At(after_mita_reset))
            .is_ok()
    );

    (before_reset, after_mita_reset)
}

#[test]
fn old_knowledge_is_inspectable_but_cannot_generate_after_reset() {
    historical_reset_boundary_contract(&mut SessionTimeline::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn historical_knowledge_epoch_remains_protected_after_sqlite_reopen() {
    let path = std::env::temp_dir().join(format!(
        "neuromita-knowledge-epoch-{}.db",
        uuid::Uuid::new_v4()
    ));
    let session = SessionId::new();
    let root = BranchId::new();
    let (before, after) = {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        historical_reset_boundary_contract(&mut timeline)
    };
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    assert!(
        reopened
            .capture_context(CharacterId::Crazy, ContextPosition::At(before))
            .is_err()
    );
    assert!(
        reopened
            .capture_context(CharacterId::Kind, ContextPosition::At(before))
            .is_err()
    );
    assert!(
        reopened
            .capture_context(CharacterId::Crazy, ContextPosition::At(after))
            .is_ok()
    );
    assert_eq!(
        reopened
            .inspect_context_at(CharacterId::Crazy, before)
            .unwrap()
            .view
            .messages()[0]
            .content,
        "forgotten secret"
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
