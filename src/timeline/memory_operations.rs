use serde_json::json;

use super::{
    ContextAnchor, SessionTimeline,
    domain::{AppendGuard, EventDraft, EventKind, TimelineError, TimelineFact},
    selection::TimelineSelection,
};
use crate::{
    identity::CharacterId,
    memory::{MAX_MEMORY_EDIT_CHANGES, MemoryChange},
};

impl SessionTimeline {
    /// Return the pinned raw world and its storage revision. Character projections
    /// are constructed by their owners, never by the Timeline persistence layer.
    pub(crate) fn selected_world_snapshot(
        &self,
    ) -> Result<(TimelineSelection, ContextAnchor, Vec<TimelineFact>), TimelineError> {
        let selection = self.selection();
        self.assert_selection(selection)?;
        let anchor = self.anchor();
        let world = self.store.world(self.branch)?;
        Ok((selection, anchor, world))
    }

    /// Persist a previously validated character-domain change batch at the exact
    /// world revision it was resolved against. No domain state is cached here.
    pub(crate) fn append_character_memory_changes(
        &mut self,
        character: CharacterId,
        expected_selection: TimelineSelection,
        expected_anchor: ContextAnchor,
        changes: &[MemoryChange],
    ) -> Result<(), TimelineError> {
        self.assert_selection(expected_selection)?;
        if self.anchor() != expected_anchor {
            return Err(TimelineError::ConcurrentWrite);
        }
        if changes.is_empty() || changes.len() > MAX_MEMORY_EDIT_CHANGES {
            return Err(TimelineError::Invalid(
                "memory edit requires 1..256 changes",
            ));
        }
        if changes
            .iter()
            .any(|change| matches!(change, MemoryChange::Delete(_)))
        {
            return Err(TimelineError::Invalid("hard deletion is not a user edit"));
        }
        let mut drafts = Vec::with_capacity(changes.len());
        for change in changes {
            let mut event = EventDraft::new(EventKind::CharacterMemoryChanged);
            event.actor = Some(character.to_string());
            event.observers.insert(character);
            event.payload.insert(
                "change".into(),
                serde_json::to_value(change)
                    .map_err(|_| TimelineError::Invalid("memory change serialization failed"))?,
            );
            drafts.push(event);
        }
        self.store.append_guarded(
            self.branch,
            expected_anchor.through,
            &format!(
                "character-memory-edit:{}:{}",
                character,
                uuid::Uuid::new_v4()
            ),
            json!({"character": character.to_string(), "expected_head": expected_anchor.through}),
            drafts,
            AppendGuard {
                selection_generation: expected_selection.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        self.notify_memory_changed(character);
        Ok(())
    }
}
