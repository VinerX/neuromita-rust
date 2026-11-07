use crate::identity::{CharacterId, EventId};

use super::domain::{BranchId, TimelineError, TimelineRepository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineSelection {
    pub branch_id: BranchId,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineAdmission {
    pub selection: TimelineSelection,
    pub context_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTurn {
    pub selection: TimelineSelection,
    pub context_head: u64,
    pub character: CharacterId,
    pub context_epoch: u64,
    pub trigger: Option<EventId>,
}

/// Durable compare-and-set selection independent of the chosen storage engine.
/// Selecting even the same branch must advance the generation.
pub trait TimelineSelectionRepository {
    fn selection(&self) -> Result<Option<TimelineSelection>, TimelineError>;
    fn select_branch(
        &mut self,
        branch: BranchId,
        expected: Option<TimelineSelection>,
    ) -> Result<TimelineSelection, TimelineError>;
}

#[derive(Default)]
pub struct SessionWorkspace {
    selected: Option<TimelineSelection>,
}

impl SessionWorkspace {
    pub fn from_selection(selection: TimelineSelection) -> Self {
        Self {
            selected: Some(selection),
        }
    }

    pub fn install_selection(&mut self, selection: TimelineSelection) {
        self.selected = Some(selection);
    }

    pub fn selected(&self) -> Option<TimelineSelection> {
        self.selected
    }

    pub fn select(
        &mut self,
        store: &dyn TimelineRepository,
        branch: BranchId,
        expected: Option<TimelineSelection>,
    ) -> Result<TimelineSelection, TimelineError> {
        store.branch(branch)?;
        if self.selected != expected {
            return Err(TimelineError::ConcurrentWrite);
        }
        let generation = self.selected.map_or(1, |selection| {
            selection.generation.checked_add(1).unwrap_or(0)
        });
        if generation == 0 {
            return Err(TimelineError::Invalid("selection generation overflow"));
        }
        let selection = TimelineSelection {
            branch_id: branch,
            generation,
        };
        self.selected = Some(selection);
        Ok(selection)
    }

    pub fn capture(
        &self,
        store: &dyn TimelineRepository,
        character: CharacterId,
        trigger: Option<EventId>,
    ) -> Result<TimelineTurn, TimelineError> {
        let selection = self.selected.ok_or(TimelineError::NotFound)?;
        let context_head = store.branch(selection.branch_id)?.head_sequence;
        self.capture_at(store, character, trigger, context_head)
    }

    /// The knowledge epoch must describe the prompt snapshot, not a later
    /// repository head observed while preparing the request.
    pub fn capture_at(
        &self,
        store: &dyn TimelineRepository,
        character: CharacterId,
        trigger: Option<EventId>,
        context_head: u64,
    ) -> Result<TimelineTurn, TimelineError> {
        let selection = self.selected.ok_or(TimelineError::NotFound)?;
        let current_head = store.branch(selection.branch_id)?.head_sequence;
        if context_head > current_head {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        if let Some(trigger) = trigger
            && !store.knows(selection.branch_id, character, trigger, Some(current_head))?
        {
            return Err(TimelineError::Invalid("character does not know trigger"));
        }
        Ok(TimelineTurn {
            selection,
            context_head,
            character,
            context_epoch: store.context_epoch_at(
                selection.branch_id,
                character,
                Some(context_head),
            )?,
            trigger,
        })
    }

    pub fn assert_current(
        &self,
        store: &dyn TimelineRepository,
        turn: TimelineTurn,
    ) -> Result<(), TimelineError> {
        if self.selected != Some(turn.selection)
            || Self::context_epoch(store, turn.selection.branch_id, turn.character)?
                != turn.context_epoch
        {
            return Err(TimelineError::ConcurrentWrite);
        }
        if let Some(trigger) = turn.trigger
            && !store.knows(turn.selection.branch_id, turn.character, trigger, None)?
        {
            return Err(TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    fn context_epoch(
        store: &dyn TimelineRepository,
        branch: BranchId,
        character: CharacterId,
    ) -> Result<u64, TimelineError> {
        store.context_epoch(branch, character)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::timeline::domain::{EventDraft, EventKind, SessionId, TimelineStore};

    #[test]
    fn selecting_same_branch_still_invalidates_old_pins() {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let mut workspace = SessionWorkspace::default();
        let selected = workspace.select(&store, branch, None).unwrap();
        let pinned = workspace.capture(&store, CharacterId::Crazy, None).unwrap();
        let again = workspace.select(&store, branch, Some(selected)).unwrap();
        assert_ne!(again, selected);
        assert_eq!(
            workspace.assert_current(&store, pinned),
            Err(TimelineError::ConcurrentWrite)
        );
    }

    #[test]
    fn reset_rejects_existing_pins_without_erasing_event_history() {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let mut workspace = SessionWorkspace::default();
        workspace.select(&store, branch, None).unwrap();
        let pinned = workspace.capture(&store, CharacterId::Crazy, None).unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        store
            .append(branch, 0, "reset", json!({}), vec![reset])
            .unwrap();
        assert_eq!(
            workspace.assert_current(&store, pinned),
            Err(TimelineError::ConcurrentWrite)
        );
        assert_eq!(store.branch(branch).unwrap().head_sequence, 1);
    }

    #[test]
    fn late_facts_do_not_invalidate_pin_without_context_reset() {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let mut workspace = SessionWorkspace::default();
        workspace.select(&store, branch, None).unwrap();
        let pinned = workspace.capture(&store, CharacterId::Crazy, None).unwrap();
        store
            .append(
                branch,
                0,
                "world",
                json!({}),
                vec![EventDraft::new(EventKind::UnityEvent)],
            )
            .unwrap();
        assert_eq!(workspace.assert_current(&store, pinned), Ok(()));
    }
}
