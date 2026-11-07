use serde_json::json;

use super::{
    SessionTimeline,
    domain::{EventDraft, EventKind, TimelineError},
};
use crate::{
    identity::CharacterId,
    memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord, MemoryState},
};

fn append_memory(timeline: &mut SessionTimeline, record: MemoryRecord) {
    let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
    draft.actor = Some(CharacterId::Crazy.to_string());
    draft.observers.insert(CharacterId::Crazy);
    draft
        .payload
        .insert("change".into(), json!(MemoryChange::Upsert(record)));
    timeline
        .append_test_fact(&format!("test-memory:{}", draft.id.0), draft)
        .unwrap();
}

#[test]
fn expiry_commits_only_due_unprotected_memories_once_and_is_character_private() {
    let mut timeline = SessionTimeline::new();
    let first = MemoryId::new();
    let protected_id = MemoryId::new();
    let late = MemoryId::new();
    for (id, protected, expiry) in [
        (first, false, 100),
        (protected_id, true, 100),
        (late, false, 200),
    ] {
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, id.0.to_string());
        record.protected = protected;
        record.expires_at_unix_ms = Some(expiry);
        append_memory(&mut timeline, record);
    }
    let selected = timeline.selection();
    let initial = timeline.anchor();
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Crazy, 99),
        Ok(0)
    );
    assert_eq!(timeline.anchor(), initial);
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Crazy, 100),
        Ok(1)
    );
    assert_eq!(timeline.anchor().through, initial.through + 1);
    let state = timeline
        .inspect_context_at(CharacterId::Crazy, timeline.anchor())
        .unwrap()
        .memories;
    assert_eq!(state.get(first).unwrap().state, MemoryState::Forgotten);
    assert_eq!(state.get(protected_id).unwrap().state, MemoryState::Active);
    assert_eq!(state.get(late).unwrap().state, MemoryState::Active);
    assert!(
        timeline
            .inspect_context_at(CharacterId::Kind, timeline.anchor())
            .unwrap()
            .memories
            .all()
            .is_empty()
    );
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Crazy, 100),
        Ok(0)
    );
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Crazy, 200),
        Ok(1)
    );
}

#[test]
fn wrong_selection_cannot_start_expiration_maintenance() {
    let mut timeline = SessionTimeline::new();
    let old = timeline.selection();
    timeline
        .select_branch(old.branch_id, old)
        .expect("same branch can be reselected");
    assert_eq!(
        timeline.expire_character_memories(old, CharacterId::Crazy, 100),
        Err(TimelineError::ConcurrentWrite)
    );
}

#[tokio::test]
async fn startup_maintenance_replays_expiration_without_running_a_scheduler() {
    use std::sync::{Arc, Mutex};

    let mut timeline = SessionTimeline::new();
    let expired = MemoryId::new();
    let protected = MemoryId::new();
    for (id, protected_flag) in [(expired, false), (protected, true)] {
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, id.0.to_string());
        record.protected = protected_flag;
        record.expires_at_unix_ms = Some(1);
        append_memory(&mut timeline, record);
    }
    let before = timeline.anchor().through;
    let timeline = Arc::new(Mutex::new(timeline));
    let stop = tokio_util::sync::CancellationToken::new();
    let (_, tasks) =
        crate::character::CharacterRegistry::spawn(Arc::clone(&timeline), stop.clone(), 4);
    {
        let timeline = timeline.lock().unwrap();
        assert_eq!(timeline.anchor().through, before + 1);
        let memories = timeline
            .inspect_context_at(CharacterId::Crazy, timeline.anchor())
            .unwrap()
            .memories;
        assert_eq!(memories.get(expired).unwrap().state, MemoryState::Forgotten);
        assert_eq!(memories.get(protected).unwrap().state, MemoryState::Active);
    }
    stop.cancel();
    for task in tasks {
        task.await.unwrap();
    }
}

#[test]
fn recall_is_tied_to_historical_prefix_without_restoring_it() {
    use crate::knowledge::forgotten_recall::{RecallSelector, recall_forgotten};
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    let mut record = MemoryRecord::fact(id, MemoryPriority::High, "player has a blue umbrella");
    record.state = MemoryState::Forgotten;
    append_memory(&mut timeline, record);
    let captured_at = timeline.anchor();
    let snapshot = timeline
        .inspect_context_at(CharacterId::Crazy, captured_at)
        .unwrap();
    let recall = recall_forgotten(
        CharacterId::Crazy,
        snapshot.history.at,
        &snapshot.memories,
        RecallSelector::Text {
            query: "blue umbrella".into(),
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(recall.anchor, captured_at);
    assert_eq!(recall.facts.len(), 1);
    assert_eq!(recall.facts[0].record.id, id);
    assert!(
        recall_forgotten(
            CharacterId::Kind,
            timeline.anchor(),
            &timeline
                .inspect_context_at(CharacterId::Kind, timeline.anchor())
                .unwrap()
                .memories,
            RecallSelector::Text {
                query: "blue umbrella".into(),
                limit: 1
            },
        )
        .unwrap()
        .facts
        .is_empty()
    );
    let selected = timeline.selection();
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Crazy, u64::MAX),
        Ok(0)
    );
    assert_eq!(timeline.anchor(), captured_at);
    assert_eq!(
        timeline
            .inspect_context_at(CharacterId::Crazy, timeline.anchor())
            .unwrap()
            .memories
            .get(id)
            .unwrap()
            .state,
        MemoryState::Forgotten
    );
}

#[tokio::test]
async fn periodic_maintenance_expires_after_startup_and_exits_with_owners() {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    let mut timeline = SessionTimeline::new();
    let due = MemoryId::new();
    let protected = MemoryId::new();
    let now = crate::character::CharacterRegistry::now_unix_ms();
    for (id, protected_flag) in [(due, false), (protected, true)] {
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, id.0.to_string());
        record.protected = protected_flag;
        record.expires_at_unix_ms = Some(now + 150);
        append_memory(&mut timeline, record);
    }
    let before = timeline.anchor().through;
    let timeline = Arc::new(Mutex::new(timeline));
    let stop = tokio_util::sync::CancellationToken::new();
    let (_, tasks) = crate::character::CharacterRegistry::spawn_with_maintenance_interval(
        Arc::clone(&timeline),
        stop.clone(),
        4,
        Duration::from_millis(10),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let (state, anchor) = {
            let locked = timeline.lock().unwrap();
            (
                locked
                    .inspect_context_at(CharacterId::Crazy, locked.anchor())
                    .unwrap()
                    .memories,
                locked.anchor(),
            )
        };
        if state.get(due).unwrap().state == MemoryState::Forgotten {
            assert_eq!(state.get(protected).unwrap().state, MemoryState::Active);
            assert_eq!(anchor.through, before + 1);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "periodic maintenance never expired the record"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.cancel();
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn forgotten_recall_and_ttl_survive_sqlite_reopen_without_duplicate_expiration() {
    use super::{
        TimelineConfig,
        domain::{BranchId, SessionId},
    };
    use crate::knowledge::forgotten_recall::{RecallSelector, recall_forgotten};
    let path = std::env::temp_dir().join(format!(
        "neuromita-memory-recall-{}.db",
        uuid::Uuid::new_v4()
    ));
    let session = SessionId::new();
    let branch = BranchId::new();
    let id = MemoryId::new();
    let through = {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: branch,
        })
        .unwrap();
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, "old green umbrella");
        record.expires_at_unix_ms = Some(500);
        append_memory(&mut timeline, record);
        assert_eq!(
            timeline.expire_character_memories(timeline.selection(), CharacterId::Crazy, 500),
            Ok(1)
        );
        timeline.anchor().through
    };
    {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: branch,
        })
        .unwrap();
        assert_eq!(timeline.anchor().through, through);
        assert_eq!(
            timeline.expire_character_memories(timeline.selection(), CharacterId::Crazy, 500),
            Ok(0)
        );
        let snap = timeline
            .inspect_context_at(CharacterId::Crazy, timeline.anchor())
            .unwrap();
        let recalled = recall_forgotten(
            CharacterId::Crazy,
            snap.history.at,
            &snap.memories,
            RecallSelector::Text {
                query: "green umbrella".into(),
                limit: 3,
            },
        )
        .unwrap();
        assert_eq!(recalled.facts.len(), 1);
        assert_eq!(recalled.facts[0].record.id, id);
        assert_eq!(recalled.facts[0].record.state, MemoryState::Forgotten);
        assert_eq!(timeline.anchor().through, through);
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn committed_expiry_and_selection_invalidate_memory_observers() {
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, "expires soon");
    record.expires_at_unix_ms = Some(100);
    append_memory(&mut timeline, record);
    let mut mita = timeline.observe_memory_changes(CharacterId::Crazy);
    let mut kind = timeline.observe_memory_changes(CharacterId::Kind);
    assert!(!mita.has_changed().unwrap());
    assert_eq!(
        timeline.expire_character_memories(timeline.selection(), CharacterId::Crazy, 99),
        Ok(0)
    );
    assert!(!mita.has_changed().unwrap());
    assert_eq!(
        timeline.expire_character_memories(timeline.selection(), CharacterId::Crazy, 100),
        Ok(1)
    );
    assert!(mita.has_changed().unwrap());
    assert!(!kind.has_changed().unwrap());
    mita.borrow_and_update();
    assert_eq!(
        timeline.expire_character_memories(timeline.selection(), CharacterId::Crazy, 100),
        Ok(0)
    );
    assert!(!mita.has_changed().unwrap());
    let selected = timeline.selection();
    timeline
        .select_branch(selected.branch_id, selected)
        .unwrap();
    assert!(mita.has_changed().unwrap());
    assert!(kind.has_changed().unwrap());
    kind.borrow_and_update();
}

#[test]
fn accepted_memory_effect_notifies_after_commit_only() {
    use super::domain::AppliedEffect;
    use super::{GeneratedReply, conversation::ConversationAudience};
    use crate::{identity::RequestId, interaction::TurnIntent};
    let mut timeline = SessionTimeline::new();
    let mut mita = timeline.observe_memory_changes(CharacterId::Crazy);
    let mut other = timeline.observe_memory_changes(CharacterId::Kind);
    let turn = timeline
        .capture_turn(CharacterId::Crazy, None, timeline.anchor())
        .unwrap();
    timeline
        .append_reply(GeneratedReply {
            request: RequestId::new(),
            intent: TurnIntent::Initiate,
            turn,
            expected_revision: 0,
            content: json!([{"text":"noted"}]),
            audience: ConversationAudience::private_to(CharacterId::Crazy),
            effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
                MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, "a fact"),
            ))],
        })
        .unwrap();
    assert!(mita.has_changed().unwrap());
    assert!(!other.has_changed().unwrap());
    mita.borrow_and_update();
    let failed = timeline.append_reply(GeneratedReply {
        request: RequestId::new(),
        intent: TurnIntent::Initiate,
        turn,
        expected_revision: 0,
        content: json!([{"text":"duplicate"}]),
        audience: ConversationAudience::private_to(CharacterId::Crazy),
        effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
            MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, "not accepted"),
        ))],
    });
    assert!(failed.is_err());
    assert!(!mita.has_changed().unwrap());
    other.borrow_and_update();
}
