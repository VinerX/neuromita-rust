use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    ContextAnchor, SessionTimeline,
    domain::{AppendGuard, EventDraft, EventKind, MemoryChange, TimelineError},
    selection::TimelineSelection,
};
use crate::{
    identity::CharacterId,
    memory::{MemoryId, MemoryRecord, MemoryState},
};

pub(crate) const MAX_PURGE_RECORDS: usize = 128;

/// A reviewable selection of exact retained records at one immutable Timeline prefix.
/// No purge happens while creating or inspecting this object.
#[derive(Debug, Clone)]
pub(crate) struct MemoryPurgePreview {
    character: CharacterId,
    selection: TimelineSelection,
    anchor: ContextAnchor,
    records: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryPurgeBackup {
    format: String,
    version: u32,
    character: CharacterId,
    branch_id: uuid::Uuid,
    through: u64,
    selection_generation: u64,
    records: Vec<MemoryRecord>,
}

impl MemoryPurgePreview {
    pub(crate) fn character(&self) -> CharacterId {
        self.character
    }

    pub(crate) fn anchor(&self) -> ContextAnchor {
        self.anchor
    }

    pub(crate) fn records(&self) -> &[MemoryRecord] {
        &self.records
    }

    pub(crate) fn backup(&self) -> MemoryPurgeBackup {
        MemoryPurgeBackup {
            format: "neuromita-memory-purge-backup".to_owned(),
            version: 1,
            character: self.character,
            branch_id: self.anchor.branch_id,
            through: self.anchor.through,
            selection_generation: self.selection.generation,
            records: self.records.clone(),
        }
    }
}

impl SessionTimeline {
    pub(crate) fn preview_memory_purge(
        &self,
        selection: TimelineSelection,
        character: CharacterId,
        ids: &[MemoryId],
    ) -> Result<MemoryPurgePreview, TimelineError> {
        self.assert_selection(selection)?;
        if ids.is_empty() || ids.len() > MAX_PURGE_RECORDS {
            return Err(TimelineError::Invalid("purge needs 1..128 memory ids"));
        }
        let mut selected = BTreeSet::new();
        if ids.iter().any(|id| !selected.insert(*id)) {
            return Err(TimelineError::Invalid("purge contains duplicate ids"));
        }
        let head = self.anchor();
        let world = self.store.world(self.branch)?;
        let memories = crate::character::projection::character_memories_at(&world, character)?;
        let mut records = Vec::with_capacity(ids.len());
        for id in selected {
            let record = memories
                .get(id)
                .ok_or(TimelineError::Invalid("purge memory is absent"))?;
            if record.state != MemoryState::Deleted || record.protected {
                return Err(TimelineError::Invalid(
                    "purge requires soft-deleted, unprotected memories",
                ));
            }
            records.push(record.clone());
        }
        Ok(MemoryPurgePreview {
            character,
            selection,
            anchor: head,
            records,
        })
    }

    /// Revalidate the approved exact records before writing anything.
    /// A concurrent branch change fails closed instead of purging newer content.
    pub(crate) fn validate_memory_purge(
        &self,
        preview: &MemoryPurgePreview,
    ) -> Result<(), TimelineError> {
        self.assert_selection(preview.selection)?;
        if self.anchor() != preview.anchor {
            return Err(TimelineError::ConcurrentWrite);
        }
        let world = self.store.world(self.branch)?;
        let memories =
            crate::character::projection::character_memories_at(&world, preview.character)?;
        for record in &preview.records {
            if memories.get(record.id) != Some(record)
                || record.state != MemoryState::Deleted
                || record.protected
            {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        Ok(())
    }

    /// Internal write primitive. Only the explicit backup-first maintenance workflow calls it.
    pub(crate) fn commit_memory_purge(
        &mut self,
        preview: &MemoryPurgePreview,
    ) -> Result<usize, TimelineError> {
        self.validate_memory_purge(preview)?;
        let mut drafts = Vec::with_capacity(preview.records.len());
        for record in &preview.records {
            let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
            draft.actor = Some(preview.character.to_string());
            draft.observers.insert(preview.character);
            draft
                .payload
                .insert("change".into(), json!(MemoryChange::Delete(record.id)));
            drafts.push(draft);
        }
        let ids = preview
            .records
            .iter()
            .map(|record| record.id.0.to_string())
            .collect::<Vec<_>>();
        self.store.append_guarded(
            self.branch,
            preview.anchor.through,
            &format!(
                "memory-purge:{}:{}:{}",
                preview.character,
                preview.anchor.through,
                ids.join(",")
            ),
            json!({"character": preview.character.to_string(), "memory_ids": ids}),
            drafts,
            AppendGuard {
                selection_generation: preview.selection.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        self.notify_memory_changed(preview.character);
        Ok(preview.records.len())
    }

    /// Reinsert the archived soft-deleted records, without activating them or
    /// overwriting any current memory. The backup must agree with its original
    /// canonical historical prefix; the file alone is never trusted.
    pub(crate) fn restore_memory_backup(
        &mut self,
        backup: &MemoryPurgeBackup,
    ) -> Result<usize, TimelineError> {
        let selection = self.selection();
        self.assert_selection(selection)?;
        if backup.format != "neuromita-memory-purge-backup"
            || backup.version != 1
            || backup.branch_id != self.branch.0
            || backup.through > self.anchor().through
            || backup.records.is_empty()
            || backup.records.len() > MAX_PURGE_RECORDS
        {
            return Err(TimelineError::Invalid("invalid or unrelated memory backup"));
        }
        let mut unique = BTreeSet::new();
        let historical = self.read_prefix(ContextAnchor {
            branch_id: backup.branch_id,
            through: backup.through,
        })?;
        let past =
            crate::character::projection::character_memories_at(&historical, backup.character)?;
        let head = self.anchor().through;
        let current = self.store.world(self.branch)?;
        let mut next =
            crate::character::projection::character_memories_at(&current, backup.character)?;
        let mut drafts = Vec::with_capacity(backup.records.len());
        for record in &backup.records {
            if !unique.insert(record.id)
                || !record.is_valid()
                || record.state != MemoryState::Deleted
                || record.protected
                || past.get(record.id) != Some(record)
                || next.get(record.id).is_some()
            {
                return Err(TimelineError::Invalid(
                    "backup identities or original records do not match",
                ));
            }
            let change = MemoryChange::Upsert(record.clone());
            next.apply_committed(&change)
                .map_err(TimelineError::Invalid)?;
            let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
            draft.actor = Some(backup.character.to_string());
            draft.observers.insert(backup.character);
            draft.payload.insert("change".into(), json!(change));
            drafts.push(draft);
        }
        next.ensure_unique().map_err(TimelineError::Invalid)?;
        self.store.append_guarded(
            self.branch,
            head,
            &format!("memory-restore:{}:{}:{}", backup.character, head, unique.iter().map(|id| id.0.to_string()).collect::<Vec<_>>().join(",")),
            json!({"character": backup.character.to_string(), "source_backup_through": backup.through}),
            drafts,
            AppendGuard { selection_generation: selection.generation, context: None, accepted_reply_revision: None },
        )?;
        self.notify_memory_changed(backup.character);
        Ok(backup.records.len())
    }
}
