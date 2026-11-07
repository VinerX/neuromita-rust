use crate::{
    identity::CharacterId,
    memory::{MAX_MEMORY_EDIT_CHANGES, MemoryChange, MemoryCommand, MemoryRecord},
    timeline::{
        ContextAnchor, SessionTimeline, domain::TimelineError, selection::TimelineSelection,
    },
};

/// A presenter-facing, immutable view of one character's retained knowledge.
/// Its private selection token prevents edits after switching Timeline branches.
#[derive(Debug, Clone)]
pub struct CharacterMemorySnapshot {
    pub(crate) character: CharacterId,
    pub(crate) anchor: ContextAnchor,
    pub(crate) records: Vec<MemoryRecord>,
    pub(crate) selection: TimelineSelection,
}

impl CharacterMemorySnapshot {
    pub fn character(&self) -> CharacterId {
        self.character
    }

    pub fn anchor(&self) -> ContextAnchor {
        self.anchor
    }

    pub fn records(&self) -> &[MemoryRecord] {
        &self.records
    }

    /// Opaque branch selection plus character-memory identity, without
    /// treating unrelated Timeline appends as memory-edit conflicts.
    pub(crate) fn matches_version(&self, other: &Self) -> bool {
        self.character == other.character
            && self.selection == other.selection
            && self.anchor.branch_id == other.anchor.branch_id
            && self.records == other.records
    }
}

#[derive(Debug, Clone)]
pub struct MemoryEditResult {
    pub applied: Vec<MemoryChange>,
    pub snapshot: CharacterMemorySnapshot,
}

/// Construct the character-owned projection from the authoritative Timeline snapshot.
/// The Timeline module itself must not know about presenter-facing character DTOs.
pub(crate) fn read_memory_snapshot(
    timeline: &SessionTimeline,
    character: CharacterId,
) -> Result<CharacterMemorySnapshot, TimelineError> {
    let (selection, anchor, world) = timeline.selected_world_snapshot()?;
    let records = super::projection::character_memories_at(&world, character)?.all();
    Ok(CharacterMemorySnapshot {
        character,
        anchor,
        records,
        selection,
    })
}

/// Resolve the command inside the character boundary, then atomically persist
/// the derived changes with Timeline's existing guarded append operation.
pub(crate) fn edit_memory_snapshot(
    timeline: &mut SessionTimeline,
    character: CharacterId,
    expected: &CharacterMemorySnapshot,
    commands: &[MemoryCommand],
) -> Result<MemoryEditResult, TimelineError> {
    if expected.character != character {
        return Err(TimelineError::ConcurrentWrite);
    }
    if commands.is_empty() || commands.len() > MAX_MEMORY_EDIT_CHANGES {
        return Err(TimelineError::Invalid(
            "memory edit requires 1..256 operations",
        ));
    }
    if commands
        .iter()
        .any(|command| matches!(command, MemoryCommand::Delete { .. }))
    {
        return Err(TimelineError::Invalid("hard deletion is not a user edit"));
    }
    let (selection, anchor, world) = timeline.selected_world_snapshot()?;
    if selection != expected.selection || anchor.branch_id != expected.anchor.branch_id {
        return Err(TimelineError::ConcurrentWrite);
    }
    let memories = super::projection::character_memories_at(&world, character)?;
    if memories.all() != expected.records {
        return Err(TimelineError::ConcurrentWrite);
    }
    let changes = memories.resolve(commands).map_err(TimelineError::Invalid)?;
    if !changes.is_empty() {
        timeline.append_character_memory_changes(character, selection, anchor, &changes)?;
    }
    Ok(MemoryEditResult {
        applied: changes,
        snapshot: read_memory_snapshot(timeline, character)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio_util::sync::CancellationToken;

    use crate::{
        character::{CharacterError, CharacterId, CharacterRegistry},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryState},
        timeline::SessionTimeline,
    };

    #[tokio::test]
    async fn memory_edits_are_character_owned_versioned_and_soft_delete_only() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
        let before = mita.memories().await.unwrap();
        assert!(before.records.is_empty());
        let added = mita
            .edit_memories(
                before.clone(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "A remembered detail".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(added.applied.len(), 1);
        let record = &added.snapshot.records[0];
        let id = record.id;
        assert_eq!(record.content, "A remembered detail");
        assert!(kind.memories().await.unwrap().records.is_empty());
        assert!(matches!(
            mita.edit_memories(before, vec![MemoryCommand::Forget { id }])
                .await,
            Err(CharacterError::StaleState)
        ));
        assert!(matches!(
            kind.edit_memories(added.snapshot.clone(), vec![MemoryCommand::Forget { id }])
                .await,
            Err(CharacterError::StaleState)
        ));
        assert!(
            mita.edit_memories(added.snapshot.clone(), vec![MemoryCommand::Delete { id }])
                .await
                .is_err()
        );
        assert_eq!(mita.memories().await.unwrap().records.len(), 1);
        let secured = mita
            .edit_memories(
                added.snapshot,
                vec![MemoryCommand::SetProtection {
                    id,
                    protected: true,
                }],
            )
            .await
            .unwrap();
        assert!(
            mita.edit_memories(
                secured.snapshot.clone(),
                vec![MemoryCommand::SoftDelete { id }]
            )
            .await
            .is_err()
        );
        let unlocked = mita
            .edit_memories(
                secured.snapshot,
                vec![MemoryCommand::SetProtection {
                    id,
                    protected: false,
                }],
            )
            .await
            .unwrap();
        let deleted = mita
            .edit_memories(unlocked.snapshot, vec![MemoryCommand::SoftDelete { id }])
            .await
            .unwrap();
        assert_eq!(deleted.snapshot.records[0].state, MemoryState::Deleted);
        assert_eq!(deleted.snapshot.records[0].id, id);
        assert_eq!(kind.memories().await.unwrap().records.len(), 0);
        let invalid = mita
            .edit_memories(
                deleted.snapshot,
                vec![MemoryCommand::Restore {
                    id: MemoryId::new(),
                }],
            )
            .await;
        assert!(invalid.is_err());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn unrelated_timeline_and_other_character_writes_do_not_invalidate_memory_editor() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
        let old_mita = mita.memories().await.unwrap();
        kind.edit_memories(
            kind.memories().await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "Other character's fact".into(),
            }],
        )
        .await
        .unwrap();
        let result = mita
            .edit_memories(
                old_mita,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Own character's fact".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(result.snapshot.records().len(), 1);
        assert_eq!(kind.memories().await.unwrap().records().len(), 1);
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }
}
