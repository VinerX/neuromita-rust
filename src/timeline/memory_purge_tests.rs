use serde_json::json;

use super::{
    SessionTimeline,
    domain::{EventDraft, EventKind, TimelineError},
};
use crate::{
    identity::CharacterId,
    maintenance::{SafePurgeError, purge_with_backup},
    memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord, MemoryState},
};

fn memory(
    timeline: &mut SessionTimeline,
    id: MemoryId,
    text: &str,
    state: MemoryState,
    protected: bool,
) {
    let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, text);
    record.state = state;
    record.protected = protected;
    let mut event = EventDraft::new(EventKind::CharacterMemoryChanged);
    event.actor = Some(CharacterId::Crazy.to_string());
    event.observers.insert(CharacterId::Crazy);
    event
        .payload
        .insert("change".into(), json!(MemoryChange::Upsert(record)));
    timeline
        .append_test_fact(&format!("purge-test:{}", event.id.0), event)
        .unwrap();
}

fn path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "neuromita-purge-{}-{name}.json",
        uuid::Uuid::new_v4()
    ))
}

#[test]
fn preview_is_read_only_bounded_and_rejects_mixed_or_protected_targets() {
    let mut timeline = SessionTimeline::new();
    let deleted = MemoryId::new();
    let active = MemoryId::new();
    let protected = MemoryId::new();
    memory(
        &mut timeline,
        deleted,
        "removed",
        MemoryState::Deleted,
        false,
    );
    memory(&mut timeline, active, "current", MemoryState::Active, false);
    memory(
        &mut timeline,
        protected,
        "shielded",
        MemoryState::Deleted,
        true,
    );
    let head = timeline.anchor();
    let selection = timeline.selection();
    for ids in [
        vec![],
        vec![deleted, deleted],
        vec![deleted, active],
        vec![deleted, protected],
        vec![MemoryId::new()],
    ] {
        assert!(
            timeline
                .preview_memory_purge(selection, CharacterId::Crazy, &ids)
                .is_err()
        );
        assert_eq!(timeline.anchor(), head);
    }
    assert!(
        timeline
            .preview_memory_purge(selection, CharacterId::Kind, &[deleted])
            .is_err()
    );
    let preview = timeline
        .preview_memory_purge(selection, CharacterId::Crazy, &[deleted])
        .unwrap();
    assert_eq!(preview.records().len(), 1);
    assert_eq!(preview.character(), CharacterId::Crazy);
    assert_eq!(preview.anchor(), head);
}

#[test]
fn confirmed_purge_requires_backup_and_cannot_overwrite_previous_backup() {
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(
        &mut timeline,
        id,
        "private to backup",
        MemoryState::Deleted,
        false,
    );
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let destination = path("confirmed");
    let head = timeline.anchor();
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, false, &destination),
        Err(SafePurgeError::ConfirmationRequired)
    ));
    assert!(!destination.exists());
    assert_eq!(timeline.anchor(), head);
    assert_eq!(
        purge_with_backup(&mut timeline, &preview, true, &destination).unwrap(),
        1
    );
    let backup: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
    assert_eq!(backup["version"], 1);
    assert_eq!(backup["format"], "neuromita-memory-purge-backup");
    assert_eq!(backup["records"][0]["id"], id.0.to_string());
    assert_eq!(backup["records"][0]["content"], "private to backup");
    assert_eq!(backup["character"], "Crazy");
    assert_eq!(backup["through"], head.through);
    assert_eq!(timeline.anchor().through, head.through + 1);
    assert!(
        timeline
            .inspect_context_at(CharacterId::Crazy, timeline.anchor())
            .unwrap()
            .memories
            .get(id)
            .is_none()
    );
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, true, &destination),
        Err(SafePurgeError::Timeline(TimelineError::ConcurrentWrite))
    ));
    let _ = std::fs::remove_file(destination);
}

#[test]
fn failed_backup_and_existing_path_never_remove_memory() {
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(&mut timeline, id, "sensitive", MemoryState::Deleted, false);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let head = timeline.anchor();
    let destination = path("existing");
    std::fs::write(&destination, b"existing backup is immutable").unwrap();
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, true, &destination),
        Err(SafePurgeError::Backup(_))
    ));
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"existing backup is immutable"
    );
    let missing = path("missing").join("backup.json");
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, true, &missing),
        Err(SafePurgeError::Backup(_))
    ));
    assert_eq!(timeline.anchor(), head);
    assert!(
        timeline
            .inspect_context_at(CharacterId::Crazy, head)
            .unwrap()
            .memories
            .get(id)
            .is_some()
    );
    let _ = std::fs::remove_file(destination);
}

#[test]
fn stale_head_or_selection_rejects_backup_and_purge() {
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(&mut timeline, id, "old", MemoryState::Deleted, false);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let path = path("stale");
    let next = MemoryId::new();
    memory(&mut timeline, next, "new", MemoryState::Active, false);
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, true, &path),
        Err(SafePurgeError::Timeline(TimelineError::ConcurrentWrite))
    ));
    assert!(!path.exists());
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let old = timeline.selection();
    timeline.select_branch(old.branch_id, old).unwrap();
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, true, &path),
        Err(SafePurgeError::Timeline(TimelineError::ConcurrentWrite))
    ));
    assert!(!path.exists());
}

#[test]
fn multi_purge_is_single_atomic_append_and_sorted_in_backup() {
    let mut timeline = SessionTimeline::new();
    let a = MemoryId::new();
    let b = MemoryId::new();
    memory(&mut timeline, a, "apple", MemoryState::Deleted, false);
    memory(&mut timeline, b, "banana", MemoryState::Deleted, false);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[b, a])
        .unwrap();
    assert!(
        preview
            .records()
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
    let before = timeline.anchor().through;
    let destination = path("multi");
    assert_eq!(
        purge_with_backup(&mut timeline, &preview, true, &destination).unwrap(),
        2
    );
    assert_eq!(timeline.anchor().through, before + 2);
    let snap = timeline
        .inspect_context_at(CharacterId::Crazy, timeline.anchor())
        .unwrap();
    assert!(snap.memories.get(a).is_none() && snap.memories.get(b).is_none());
    let _ = std::fs::remove_file(destination);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn durable_purge_replays_after_sqlite_reopen_and_backup_remains() {
    use super::{
        TimelineConfig,
        domain::{BranchId, SessionId},
    };
    let db = path("sqlite-db");
    let backup = path("sqlite-backup");
    let session = SessionId::new();
    let branch = BranchId::new();
    let id = MemoryId::new();
    let through = {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root: branch,
        })
        .unwrap();
        memory(
            &mut timeline,
            id,
            "durable archive",
            MemoryState::Deleted,
            false,
        );
        let preview = timeline
            .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
            .unwrap();
        assert_eq!(
            purge_with_backup(&mut timeline, &preview, true, &backup).unwrap(),
            1
        );
        timeline.anchor().through
    };
    {
        let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root: branch,
        })
        .unwrap();
        assert_eq!(timeline.anchor().through, through);
        assert!(
            timeline
                .inspect_context_at(CharacterId::Crazy, timeline.anchor())
                .unwrap()
                .memories
                .get(id)
                .is_none()
        );
        assert!(
            timeline
                .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
                .is_err()
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
        assert_eq!(saved["records"][0]["content"], "durable archive");
    }
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn archived_records_can_be_restored_as_soft_deleted_not_active() {
    use crate::maintenance::restore_from_backup;
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(
        &mut timeline,
        id,
        "recoverable",
        MemoryState::Deleted,
        false,
    );
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let backup = path("restore");
    purge_with_backup(&mut timeline, &preview, true, &backup).unwrap();
    let after_purge = timeline.anchor().through;
    assert!(matches!(
        restore_from_backup(&mut timeline, false, &backup),
        Err(SafePurgeError::ConfirmationRequired)
    ));
    assert_eq!(timeline.anchor().through, after_purge);
    assert_eq!(
        restore_from_backup(&mut timeline, true, &backup).unwrap(),
        1
    );
    assert_eq!(
        timeline
            .inspect_context_at(CharacterId::Crazy, timeline.anchor())
            .unwrap()
            .memories
            .get(id)
            .unwrap()
            .state,
        MemoryState::Deleted
    );
    assert_eq!(timeline.anchor().through, after_purge + 1);
    assert!(restore_from_backup(&mut timeline, true, &backup).is_err());
    let _ = std::fs::remove_file(backup);
}

#[test]
fn corrupted_or_forged_backup_never_mutates_timeline() {
    use crate::maintenance::restore_from_backup;
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(&mut timeline, id, "original", MemoryState::Deleted, false);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let backup = path("corrupt");
    purge_with_backup(&mut timeline, &preview, true, &backup).unwrap();
    let head = timeline.anchor();
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
    for changed in [
        ("content", json!("forged content")),
        ("state", json!("active")),
        ("id", json!(MemoryId::new().0.to_string())),
    ] {
        let original = data["records"][0].clone();
        data["records"][0][changed.0] = changed.1;
        std::fs::write(&backup, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(restore_from_backup(&mut timeline, true, &backup).is_err());
        assert_eq!(timeline.anchor(), head);
        data["records"][0] = original;
    }
    let _ = std::fs::remove_file(backup);
}

#[cfg(unix)]
#[test]
fn backup_refuses_symlink_destination_and_is_private_to_owner() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(&mut timeline, id, "secret", MemoryState::Deleted, false);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let target = path("symlink-target");
    let link = path("symlink-link");
    std::fs::write(&target, b"untouched").unwrap();
    symlink(&target, &link).unwrap();
    assert!(purge_with_backup(&mut timeline, &preview, true, &link).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    let _ = std::fs::remove_file(link);
    let _ = std::fs::remove_file(target);
    let dest = path("mode");
    purge_with_backup(&mut timeline, &preview, true, &dest).unwrap();
    assert_eq!(
        std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let _ = std::fs::remove_file(dest);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn restore_backup_after_reopening_sqlite_keeps_it_deleted() {
    use super::{
        TimelineConfig,
        domain::{BranchId, SessionId},
    };
    use crate::maintenance::restore_from_backup;
    let db = path("sqlite-restore-db");
    let backup = path("sqlite-restore-backup");
    let session = SessionId::new();
    let branch = BranchId::new();
    let id = MemoryId::new();
    let after_purge = {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root: branch,
        })
        .unwrap();
        memory(
            &mut timeline,
            id,
            "historic data",
            MemoryState::Deleted,
            false,
        );
        let preview = timeline
            .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
            .unwrap();
        purge_with_backup(&mut timeline, &preview, true, &backup).unwrap();
        timeline.anchor().through
    };
    {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root: branch,
        })
        .unwrap();
        assert_eq!(timeline.anchor().through, after_purge);
        assert_eq!(
            restore_from_backup(&mut timeline, true, &backup).unwrap(),
            1
        );
        assert_eq!(timeline.anchor().through, after_purge + 1);
        assert_eq!(
            timeline
                .inspect_context_at(CharacterId::Crazy, timeline.anchor())
                .unwrap()
                .memories
                .get(id)
                .unwrap()
                .state,
            MemoryState::Deleted
        );
    }
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn foreign_branch_backup_and_duplicate_archive_entries_fail_closed() {
    use crate::maintenance::restore_from_backup;
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(
        &mut timeline,
        id,
        "only this branch",
        MemoryState::Deleted,
        false,
    );
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    let archive = path("foreign");
    purge_with_backup(&mut timeline, &preview, true, &archive).unwrap();
    let head = timeline.anchor();
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive).unwrap()).unwrap();
    data["branch_id"] = json!(uuid::Uuid::new_v4());
    std::fs::write(&archive, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(restore_from_backup(&mut timeline, true, &archive).is_err());
    data["branch_id"] = json!(head.branch_id);
    let duplicated = data["records"][0].clone();
    data["records"].as_array_mut().unwrap().push(duplicated);
    std::fs::write(&archive, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(restore_from_backup(&mut timeline, true, &archive).is_err());
    assert_eq!(timeline.anchor(), head);
    let _ = std::fs::remove_file(archive);
}

#[test]
fn memory_purge_and_restore_notify_only_after_authorized_commits() {
    use crate::maintenance::restore_from_backup;
    let mut timeline = SessionTimeline::new();
    let id = MemoryId::new();
    memory(&mut timeline, id, "archived", MemoryState::Deleted, false);
    let mut mita = timeline.observe_memory_changes(CharacterId::Crazy);
    let mut other = timeline.observe_memory_changes(CharacterId::Kind);
    let preview = timeline
        .preview_memory_purge(timeline.selection(), CharacterId::Crazy, &[id])
        .unwrap();
    assert!(!mita.has_changed().unwrap());
    let destination = path("watch-commits");
    assert!(matches!(
        purge_with_backup(&mut timeline, &preview, false, &destination),
        Err(SafePurgeError::ConfirmationRequired)
    ));
    assert!(!mita.has_changed().unwrap());
    purge_with_backup(&mut timeline, &preview, true, &destination).unwrap();
    assert!(mita.has_changed().unwrap());
    assert!(!other.has_changed().unwrap());
    mita.borrow_and_update();
    assert!(restore_from_backup(&mut timeline, false, &destination).is_err());
    assert!(!mita.has_changed().unwrap());
    restore_from_backup(&mut timeline, true, &destination).unwrap();
    assert!(mita.has_changed().unwrap());
    assert!(!other.has_changed().unwrap());
    let _ = std::fs::remove_file(destination);
    other.borrow_and_update();
}
