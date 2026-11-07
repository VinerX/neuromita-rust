use tokio::sync::watch;

use crate::{
    character::{
        CharacterDefinition, CharacterError, CharacterHandle, CharacterId, CharacterMemorySnapshot,
        CharacterRegistry, CharacterState, CharacterView, MemoryEditResult,
    },
    memory::{MemoryCapacityPlan, MemoryCommand, MemoryKind, MemoryRecord, MemoryState},
    request::{InputError, UserInput},
    timeline::{ContextAnchor, EventId},
};

/// Read-only selection criteria for a user-facing memory list.
/// The query never affects canonical data or retrieval context.
#[derive(Debug, Clone)]
pub struct MemoryListQuery {
    pub text: Option<String>,
    pub state: Option<MemoryState>,
    pub kind: Option<MemoryKind>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for MemoryListQuery {
    fn default() -> Self {
        Self {
            text: None,
            state: None,
            kind: None,
            offset: 0,
            limit: 50,
        }
    }
}

impl MemoryListQuery {
    fn validate(&self) -> Result<(), CharacterError> {
        if !(1..=100).contains(&self.limit) {
            return Err(CharacterError::InvalidMemory(
                "memory page limit must be 1..100",
            ));
        }
        if self
            .text
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .chars()
            .count()
            > 200
        {
            return Err(CharacterError::InvalidMemory(
                "memory search text exceeds 200 characters",
            ));
        }
        Ok(())
    }
}

/// Counts for the full authoritative character snapshot, independent of list filters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySummary {
    pub active: usize,
    pub forgotten: usize,
    pub deleted: usize,
    pub facts: usize,
    pub islands: usize,
    pub protected: usize,
}

/// A bounded read model with the unfiltered edit token from the same snapshot.
/// Editing with this token retains the Character owner's optimistic concurrency checks.
#[derive(Debug, Clone)]
pub struct MemoryListPage {
    snapshot: CharacterMemorySnapshot,
    records: Vec<MemoryRecord>,
    summary: MemorySummary,
    total_matches: usize,
    next_offset: Option<usize>,
}

impl MemoryListPage {
    pub fn snapshot(&self) -> &CharacterMemorySnapshot {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> CharacterMemorySnapshot {
        self.snapshot
    }

    pub fn records(&self) -> &[MemoryRecord] {
        &self.records
    }

    pub fn summary(&self) -> MemorySummary {
        self.summary
    }

    pub fn total_matches(&self) -> usize {
        self.total_matches
    }

    pub fn next_offset(&self) -> Option<usize> {
        self.next_offset
    }

    /// Page a previously captured snapshot without re-reading mutable Timeline state.
    /// This keeps all page offsets and totals stable until the caller requests a refresh.
    pub fn from_snapshot(
        snapshot: CharacterMemorySnapshot,
        query: MemoryListQuery,
    ) -> Result<Self, CharacterError> {
        query.validate()?;
        let search = query.text.as_deref().map(str::trim).unwrap_or_default();
        let normalized_search = search.to_lowercase();
        let mut summary = MemorySummary::default();
        let mut matching = Vec::new();
        for record in snapshot.records() {
            match record.state {
                MemoryState::Active => summary.active += 1,
                MemoryState::Forgotten => summary.forgotten += 1,
                MemoryState::Deleted => summary.deleted += 1,
            }
            match record.kind {
                MemoryKind::Fact => summary.facts += 1,
                MemoryKind::Island(_) => summary.islands += 1,
            }
            if record.protected {
                summary.protected += 1;
            }
            if query.state.is_some_and(|state| record.state != state)
                || query.kind.is_some_and(|kind| record.kind != kind)
                || (!normalized_search.is_empty()
                    && !record.content.to_lowercase().contains(&normalized_search))
            {
                continue;
            }
            matching.push(record.clone());
        }
        let total_matches = matching.len();
        let start = query.offset.min(total_matches);
        let end = start.saturating_add(query.limit).min(total_matches);
        let records = matching[start..end].to_vec();
        Ok(Self {
            snapshot,
            records,
            summary,
            total_matches,
            next_offset: (end < total_matches).then_some(end),
        })
    }
}

/// A capacity operation tied to exactly one character snapshot.
/// The preview is immutable; applying it uses the existing guarded character edit.
#[derive(Debug, Clone)]
pub struct MemoryCapacityPreview {
    snapshot: CharacterMemorySnapshot,
    plan: MemoryCapacityPlan,
}

impl MemoryCapacityPreview {
    pub fn from_snapshot(
        snapshot: CharacterMemorySnapshot,
        limit: usize,
    ) -> Result<Self, CharacterError> {
        let plan = MemoryCapacityPlan::preview(snapshot.records(), limit)
            .map_err(CharacterError::InvalidMemory)?;
        Ok(Self { snapshot, plan })
    }

    pub fn plan(&self) -> &MemoryCapacityPlan {
        &self.plan
    }

    pub fn snapshot(&self) -> &CharacterMemorySnapshot {
        &self.snapshot
    }
}

/// Coalescing observer of character memory changes committed by this Core.
/// The signal contains no memory data: every observation rebuilds its page
/// from the same authoritative CharacterHandle used for ordinary reads.
pub struct MemoryObserver {
    character: CharacterHandle,
    updates: watch::Receiver<u64>,
    query: MemoryListQuery,
    first: bool,
}

impl MemoryObserver {
    pub async fn next(&mut self) -> Result<MemoryListPage, CharacterError> {
        if !self.first {
            self.updates
                .changed()
                .await
                .map_err(|_| CharacterError::Unavailable)?;
        }
        self.updates.borrow_and_update();
        let result =
            MemoryListPage::from_snapshot(self.character.memories().await?, self.query.clone());
        self.first = result.is_err();
        result
    }
}

#[derive(Clone)]
pub struct CharactersPresenter {
    registry: CharacterRegistry,
}
impl CharactersPresenter {
    pub(crate) fn new(registry: CharacterRegistry) -> Self {
        Self { registry }
    }
    fn get(&self, id: CharacterId) -> Result<CharacterHandle, CharacterError> {
        self.registry.get(id).ok_or(CharacterError::Unavailable)
    }
    pub fn observe_memories(
        &self,
        id: CharacterId,
        query: MemoryListQuery,
    ) -> Result<MemoryObserver, CharacterError> {
        query.validate()?;
        let character = self.get(id)?;
        Ok(MemoryObserver {
            updates: character.memory_updates(),
            character,
            query,
            first: true,
        })
    }

    pub async fn memories(
        &self,
        id: CharacterId,
    ) -> Result<CharacterMemorySnapshot, CharacterError> {
        self.get(id)?.memories().await
    }
    pub async fn list_memories(
        &self,
        id: CharacterId,
        query: MemoryListQuery,
    ) -> Result<MemoryListPage, CharacterError> {
        MemoryListPage::from_snapshot(self.memories(id).await?, query)
    }
    /// Preview a reduction without mutating Timeline or invoking inference.
    pub async fn preview_memory_capacity(
        &self,
        id: CharacterId,
        limit: usize,
    ) -> Result<MemoryCapacityPreview, CharacterError> {
        MemoryCapacityPreview::from_snapshot(self.memories(id).await?, limit)
    }

    /// The plan's embedded snapshot enforces branch and character memory CAS.
    /// No-op plans deliberately produce no event or observer notification.
    pub async fn apply_memory_capacity(
        &self,
        id: CharacterId,
        preview: MemoryCapacityPreview,
    ) -> Result<Option<MemoryEditResult>, CharacterError> {
        if preview.snapshot.character() != id {
            return Err(CharacterError::StaleState);
        }
        if preview.plan.is_noop() {
            let current = self.memories(id).await?;
            if !current.matches_version(&preview.snapshot) {
                return Err(CharacterError::StaleState);
            }
            return Ok(None);
        }
        self.edit_memories(id, preview.snapshot, preview.plan.forget_commands())
            .await
            .map(Some)
    }

    pub async fn edit_memories(
        &self,
        id: CharacterId,
        expected: CharacterMemorySnapshot,
        commands: Vec<MemoryCommand>,
    ) -> Result<MemoryEditResult, CharacterError> {
        self.get(id)?.edit_memories(expected, commands).await
    }
    /// UI receives the same definitions used to instantiate owned characters.
    pub fn catalog(&self) -> Vec<CharacterDefinition> {
        crate::character::character_catalog().collect()
    }

    pub fn available(&self) -> &'static [CharacterId] {
        &CharacterId::ALL
    }
    pub async fn state(&self, id: CharacterId) -> Result<CharacterState, CharacterError> {
        self.get(id)?.state().await
    }
    pub async fn context(
        &self,
        id: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterView, CharacterError> {
        self.get(id)?.get_context(at).await
    }
    pub async fn observe(
        &self,
        id: CharacterId,
        input: UserInput,
    ) -> Result<EventId, CharacterError> {
        self.get(id)?.observe(input).await
    }
    pub fn input(&self, text: String) -> Result<UserInput, InputError> {
        UserInput::new(text)
    }
}
