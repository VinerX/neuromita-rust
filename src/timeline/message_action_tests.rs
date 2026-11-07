use serde_json::json;

use super::*;
use crate::timeline::domain::TimelineError;
use crate::timeline::message_actions::MessageAction;

fn add_message(
    timeline: &mut SessionTimeline,
    observers: &[CharacterId],
    content: &str,
) -> EventId {
    let mut message = EventDraft::new(EventKind::PlayerMessage);
    message.actor = Some("player".to_string());
    message.observers.extend(observers.iter().copied());
    message.payload.insert("content".into(), json!(content));
    let id = message.id;
    let branch = timeline.selection().branch_id;
    let head = timeline.store.branch(branch).unwrap().head_sequence;
    timeline
        .store
        .append(
            branch,
            head,
            &format!("seed:{}", id.0),
            json!({}),
            vec![message],
        )
        .unwrap();
    id
}

fn view(timeline: &SessionTimeline, character: CharacterId) -> Vec<view::ContextEntry> {
    let branch = timeline.selection().branch_id;
    TimelineMessageQueries::capture(timeline.store.as_ref(), branch, character, None)
        .unwrap()
        .context_entries()
}

fn documents(
    timeline: &SessionTimeline,
    character: CharacterId,
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let branch = timeline.selection().branch_id;
    TimelineMessageQueries::capture(timeline.store.as_ref(), branch, character, None)
        .unwrap()
        .provider_messages()
}

fn presentation_contract(timeline: &mut SessionTimeline) -> EventId {
    let shared = add_message(timeline, &CharacterId::ALL, "original");
    let private = add_message(timeline, &[CharacterId::Crazy], "secret");
    let selection = timeline.selection();
    let branch = selection.branch_id;
    let old =
        TimelineMessageQueries::capture(timeline.store.as_ref(), branch, CharacterId::Crazy, None)
            .unwrap();
    assert_eq!(old.count(), 2);

    let hidden = EventId::new();
    let action = MessageAction::Hide {
        targets: vec![shared],
    };
    let first = timeline
        .change_message(selection, CharacterId::Crazy, hidden, action.clone())
        .unwrap();
    assert_eq!(first.draft.kind, EventKind::MessageHidden);
    assert_eq!(documents(timeline, CharacterId::Crazy).len(), 1);
    assert_eq!(documents(timeline, CharacterId::Kind).len(), 1);
    assert_eq!(
        documents(timeline, CharacterId::Kind)[0]["content"],
        "original"
    );
    assert_eq!(old.count(), 2);

    let after_hide = timeline.store.branch(branch).unwrap().head_sequence;
    assert_eq!(
        timeline
            .change_message(selection, CharacterId::Crazy, hidden, action.clone())
            .unwrap(),
        first,
    );
    assert_eq!(
        timeline.store.branch(branch).unwrap().head_sequence,
        after_hide
    );
    assert_eq!(
        timeline.change_message(
            selection,
            CharacterId::Crazy,
            hidden,
            MessageAction::Restore { target: shared }
        ),
        Err(TimelineError::Conflict),
    );
    assert_eq!(
        timeline.store.branch(branch).unwrap().head_sequence,
        after_hide
    );

    assert_eq!(
        timeline.change_message(
            selection,
            CharacterId::Kind,
            EventId::new(),
            MessageAction::Hide {
                targets: vec![private]
            }
        ),
        Err(TimelineError::Invalid("cannot modify unknown message")),
    );

    let mut changes = serde_json::Map::new();
    changes.insert("content".into(), json!("modified"));
    changes.insert("annotation".into(), json!("from editor"));
    timeline
        .change_message(
            selection,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Revise {
                target: shared,
                changes,
            },
        )
        .unwrap();
    assert_eq!(documents(timeline, CharacterId::Crazy).len(), 2);
    timeline
        .change_message(
            selection,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Hide {
                targets: vec![shared],
            },
        )
        .unwrap();
    assert_eq!(documents(timeline, CharacterId::Crazy).len(), 1);
    timeline
        .change_message(
            selection,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Restore { target: shared },
        )
        .unwrap();
    let now = documents(timeline, CharacterId::Crazy);
    assert_eq!(now.len(), 2);
    assert_eq!(now[0]["content"], "modified");
    assert_eq!(now[0]["annotation"], "from editor");
    assert_eq!(now[0]["timeline_event_id"], shared.0.to_string());
    assert_eq!(
        documents(timeline, CharacterId::Kind)[0]["content"],
        "original"
    );

    let mut multimodal = serde_json::Map::new();
    let parts = json!([{"type": "text", "text": "edited in parts"}]);
    multimodal.insert("content".into(), parts.clone());
    timeline
        .change_message(
            selection,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Revise {
                target: shared,
                changes: multimodal,
            },
        )
        .unwrap();
    assert_eq!(documents(timeline, CharacterId::Crazy)[0]["content"], parts);
    assert_eq!(
        documents(timeline, CharacterId::Kind)[0]["content"],
        "original"
    );

    let mut invalid = serde_json::Map::new();
    invalid.insert("content".into(), json!("invalid"));
    invalid.insert("message_id".into(), json!("replaced"));
    assert!(matches!(
        timeline.change_message(
            selection,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Revise {
                target: shared,
                changes: invalid
            }
        ),
        Err(TimelineError::Invalid(_)),
    ));
    assert_eq!(old.messages()[0].content, "original");
    assert_eq!(view(timeline, CharacterId::Crazy).len(), 2);
    shared
}

#[test]
fn message_actions_are_visible_only_to_the_target_character_in_memory() {
    presentation_contract(&mut SessionTimeline::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn message_actions_survive_sqlite_reopen_and_do_not_modify_other_character() {
    let path = std::env::temp_dir().join(format!(
        "neuromita-message-action-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let session = SessionId::new();
    let root = BranchId::new();
    let shared;
    {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        shared = presentation_contract(&mut timeline);
    }
    {
        let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        assert_eq!(
            documents(&timeline, CharacterId::Crazy)[0]["content"],
            json!([{"type": "text", "text": "edited in parts"}]),
        );
        assert_eq!(
            documents(&timeline, CharacterId::Kind)[0]["content"],
            "original"
        );
        assert_eq!(
            documents(&timeline, CharacterId::Crazy)[0]["timeline_event_id"],
            shared.0.to_string(),
        );
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn message_actions_reject_stale_selection_without_writing() {
    let mut timeline = SessionTimeline::new();
    let original = add_message(&mut timeline, &[CharacterId::Crazy], "original");
    let stale = timeline.selection();
    timeline.select_branch(stale.branch_id, stale).unwrap();
    let head = timeline.anchor().through;
    assert_eq!(
        timeline.change_message(
            stale,
            CharacterId::Crazy,
            EventId::new(),
            MessageAction::Hide {
                targets: vec![original]
            }
        ),
        Err(TimelineError::ConcurrentWrite),
    );
    assert_eq!(timeline.anchor().through, head);
}
