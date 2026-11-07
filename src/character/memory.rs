use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_ACTIVE_FACTS: usize = 256;
pub const MAX_RETAINED_MEMORIES: usize = 1024;
pub const MAX_MEMORY_EDIT_CHANGES: usize = MAX_ACTIVE_FACTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryId(pub Uuid);

impl MemoryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

/// Whether a retained memory participates in ordinary prompt selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    #[default]
    Active,
    Forgotten,
    Deleted,
}

/// One canonical island per character and category, independently of the fact budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryIsland {
    Relationship,
    Opinion,
    Preferences,
    CommitmentsConflicts,
    Language,
}

impl MemoryIsland {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relationship => "relationship",
            Self::Opinion => "opinion",
            Self::Preferences => "preferences",
            Self::CommitmentsConflicts => "commitments_conflicts",
            Self::Language => "language",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    #[default]
    Fact,
    Island(MemoryIsland),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub priority: MemoryPriority,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_fact")]
    pub kind: MemoryKind,
    #[serde(default, skip_serializing_if = "is_active")]
    pub state: MemoryState,
    /// A protected record cannot be forgotten, purged or merged until explicitly unprotected.
    #[serde(default, skip_serializing_if = "is_unprotected")]
    pub protected: bool,
    /// Explicit expiration cutoff, applied only by a committed maintenance operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<u64>,
}

fn is_unprotected(protected: &bool) -> bool {
    !*protected
}

fn is_fact(kind: &MemoryKind) -> bool {
    *kind == MemoryKind::Fact
}

fn is_active(state: &MemoryState) -> bool {
    *state == MemoryState::Active
}

fn normalized_content(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl MemoryRecord {
    pub fn fact(id: MemoryId, priority: MemoryPriority, content: impl Into<String>) -> Self {
        Self {
            id,
            priority,
            content: content.into(),
            kind: MemoryKind::Fact,
            state: MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        }
    }

    pub fn available_at(&self, unix_ms: u64) -> bool {
        self.state == MemoryState::Active
            && self
                .expires_at_unix_ms
                .is_none_or(|expires| expires > unix_ms)
    }

    pub fn is_valid(&self) -> bool {
        let content = self.content.trim();
        !content.is_empty() && content.chars().count() <= 2000
    }
}

/// The model's uncommitted intent, addressed by immutable memory identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCommand {
    Add {
        priority: MemoryPriority,
        content: String,
    },
    /// Idempotent exact-text insertion. A retained forgotten/deleted fact is
    /// reactivated under its original ID instead of allocating another record.
    /// Ordinary Add, explicit Restore and island operations are unchanged.
    EnsureFact {
        priority: MemoryPriority,
        content: String,
    },
    /// Explicit lexical Jaccard dedup on active facts only. This does not run inference.
    /// A threshold of 800 represents Python MEMORY_DEDUP_THRESHOLD=0.8.
    AddLexicallyDeduplicated {
        priority: MemoryPriority,
        content: String,
        threshold_per_mille: u16,
    },
    Update {
        id: MemoryId,
        priority: Option<MemoryPriority>,
        content: String,
    },
    Delete {
        id: MemoryId,
    },
    Merge {
        target: MemoryId,
        sources: Vec<MemoryId>,
        content: Option<String>,
    },
    /// Explicitly replace a typed island, preserving its identity on subsequent updates.
    SetIsland {
        island: MemoryIsland,
        content: String,
    },
    /// Initialize one island only if its category has never been retained.
    /// Unlike SetIsland, repeated seeding never overwrites evolving memory.
    SeedIsland {
        island: MemoryIsland,
        content: String,
    },
    Forget {
        id: MemoryId,
    },
    Restore {
        id: MemoryId,
    },
    SoftDelete {
        id: MemoryId,
    },
    SetExpiry {
        id: MemoryId,
        expires_at_unix_ms: Option<u64>,
    },
    SetProtection {
        id: MemoryId,
        protected: bool,
    },
}

/// A pure, opt-in capacity reduction proposal. No memory is forgotten until
/// the character owner commits the corresponding explicit Forget commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCapacityPlan {
    active_facts: usize,
    limit: usize,
    victims: Vec<MemoryId>,
}

impl MemoryCapacityPlan {
    /// Stable priority-first ordering, with MemoryId as a tie-breaker. MemoryId
    /// is NOT a proxy for creation time or retrieval frequency.
    pub fn preview(records: &[MemoryRecord], limit: usize) -> Result<Self, &'static str> {
        if !(1..=MAX_ACTIVE_FACTS).contains(&limit) {
            return Err("active memory target must be 1..256");
        }
        let active_facts = records
            .iter()
            .filter(|record| record.kind == MemoryKind::Fact && record.state == MemoryState::Active)
            .count();
        let needed = active_facts.saturating_sub(limit);
        let mut candidates = records
            .iter()
            .filter(|record| {
                record.kind == MemoryKind::Fact
                    && record.state == MemoryState::Active
                    && !record.protected
                    && record.priority != MemoryPriority::Critical
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|record| (record.priority, record.id));
        if candidates.len() < needed {
            return Err("insufficient forgettable memories to reach capacity target");
        }
        Ok(Self {
            active_facts,
            limit,
            victims: candidates[..needed]
                .iter()
                .map(|record| record.id)
                .collect(),
        })
    }

    pub fn active_facts(&self) -> usize {
        self.active_facts
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn victims(&self) -> &[MemoryId] {
        &self.victims
    }

    pub fn is_noop(&self) -> bool {
        self.victims.is_empty()
    }

    pub fn forget_commands(&self) -> Vec<MemoryCommand> {
        self.victims
            .iter()
            .map(|&id| MemoryCommand::Forget { id })
            .collect()
    }
}

/// Canonical committed operation; upserts include complete values, not partial deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MemoryChange {
    Upsert(MemoryRecord),
    Delete(MemoryId),
    /// An explicit, auditable protection transition separate from model-authored upserts.
    SetProtection {
        id: MemoryId,
        protected: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharacterMemories {
    records: BTreeMap<MemoryId, MemoryRecord>,
}

impl CharacterMemories {
    pub(crate) fn ensure_unique(&self) -> Result<(), &'static str> {
        let mut facts = BTreeSet::new();
        let mut islands = BTreeSet::new();
        for record in self.records.values() {
            if record.state == MemoryState::Deleted {
                continue;
            }
            match record.kind {
                MemoryKind::Fact => {
                    if !facts.insert(normalized_content(&record.content)) {
                        return Err("duplicate memory content");
                    }
                }
                MemoryKind::Island(island) => {
                    if !islands.insert(island) {
                        return Err("duplicate memory island");
                    }
                }
            }
        }
        Ok(())
    }

    /// Current active, non-expired facts are projected by explicit maintenance;
    /// reading cannot mutate the authoritative Timeline.
    pub fn list(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| record.kind == MemoryKind::Fact && record.state == MemoryState::Active)
            .cloned()
            .collect()
    }

    /// Live read-side boundary: an expired record is never offered to generation,
    /// even if the periodic durable TTL pass has not committed its transition.
    pub fn list_at(&self, as_of_unix_ms: u64) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| record.kind == MemoryKind::Fact && record.available_at(as_of_unix_ms))
            .cloned()
            .collect()
    }

    pub fn islands_at(&self, as_of_unix_ms: u64) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Island(_)) && record.available_at(as_of_unix_ms)
            })
            .cloned()
            .collect()
    }

    /// Complete retained history for inspection, restore, and maintenance.
    pub fn all(&self) -> Vec<MemoryRecord> {
        self.records.values().cloned().collect()
    }

    pub fn islands(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Island(_)) && record.state == MemoryState::Active
            })
            .cloned()
            .collect()
    }

    pub fn forgotten(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| {
                record.kind == MemoryKind::Fact && record.state == MemoryState::Forgotten
            })
            .cloned()
            .collect()
    }

    pub fn get(&self, id: MemoryId) -> Option<&MemoryRecord> {
        self.records.get(&id)
    }

    pub fn apply_committed(&mut self, change: &MemoryChange) -> Result<(), &'static str> {
        match change {
            MemoryChange::Upsert(record) => {
                if !record.is_valid() {
                    return Err("invalid memory record");
                }
                if let Some(existing) = self.records.get(&record.id) {
                    if existing.kind != record.kind {
                        return Err("memory kind cannot change for an existing id");
                    }
                    if existing.protected != record.protected {
                        return Err("use SetProtection to change memory protection");
                    }
                    if existing.protected && existing != record {
                        return Err("protected memory cannot change before unprotecting");
                    }
                }
                if !self.records.contains_key(&record.id)
                    && self.records.len() >= MAX_RETAINED_MEMORIES
                {
                    return Err("retained memory capacity exceeded");
                }
                let next_active_facts = self
                    .records
                    .values()
                    .filter(|other| {
                        other.id != record.id
                            && other.kind == MemoryKind::Fact
                            && other.state == MemoryState::Active
                    })
                    .count()
                    + usize::from(
                        record.kind == MemoryKind::Fact && record.state == MemoryState::Active,
                    );
                if next_active_facts > MAX_ACTIVE_FACTS {
                    return Err("active memory capacity exceeded");
                }
                self.records.insert(record.id, record.clone());
            }
            // Legacy physical delete is kept for exact replay compatibility.
            MemoryChange::Delete(id) => {
                let existing = self.records.get(id).ok_or("unknown memory id")?;
                if existing.protected {
                    return Err("protected memory must be unprotected before purge");
                }
                self.records.remove(id);
            }
            MemoryChange::SetProtection { id, protected } => {
                let record = self.records.get_mut(id).ok_or("unknown memory id")?;
                if record.state == MemoryState::Deleted {
                    return Err("deleted memory cannot change protection");
                }
                record.protected = *protected;
            }
        }
        Ok(())
    }

    /// Resolve an explicit TTL maintenance pass without reading the wall clock.
    /// Callers commit the returned changes through the ordinary Timeline boundary.
    pub fn resolve_expirations(&self, as_of_unix_ms: u64) -> Vec<MemoryChange> {
        self.records
            .values()
            .filter(|record| {
                record.state == MemoryState::Active
                    && !record.protected
                    && record
                        .expires_at_unix_ms
                        .is_some_and(|expiry| expiry <= as_of_unix_ms)
            })
            .map(|record| {
                let mut expired = record.clone();
                expired.state = MemoryState::Forgotten;
                MemoryChange::Upsert(expired)
            })
            .collect()
    }

    /// Resolves a proposed batch completely before any Timeline append.
    pub fn resolve(&self, commands: &[MemoryCommand]) -> Result<Vec<MemoryChange>, &'static str> {
        let mut next = self.clone();
        let mut changes = Vec::new();
        for command in commands {
            match command {
                MemoryCommand::Add { priority, content }
                | MemoryCommand::AddLexicallyDeduplicated {
                    priority, content, ..
                } => {
                    if let MemoryCommand::AddLexicallyDeduplicated {
                        threshold_per_mille,
                        ..
                    } = command
                    {
                        if content.trim().is_empty() || content.chars().count() > 2000 {
                            return Err("invalid memory record");
                        }
                        let duplicate = super::lexical_dedup::best_active_fact(
                            next.records.values(),
                            content,
                            *threshold_per_mille,
                        )?;
                        if let Some(id) = duplicate {
                            let previous = next.get(id).expect("dedup candidate from records");
                            if previous.protected {
                                return Err(
                                    "protected memory cannot be edited before unprotecting",
                                );
                            }
                            let mut changed = previous.clone();
                            changed.content = content.clone();
                            changed.priority = changed.priority.max(*priority);
                            if changed != *previous {
                                let change = MemoryChange::Upsert(changed);
                                next.apply_committed(&change)?;
                                changes.push(change);
                            }
                            continue;
                        }
                    }
                    if let Some(previous) = next.records.values().find(|record| {
                        record.kind == MemoryKind::Fact
                            && record.state != MemoryState::Deleted
                            && normalized_content(&record.content) == normalized_content(content)
                    }) {
                        if previous.state != MemoryState::Active {
                            return Err("memory is forgotten: restore explicitly");
                        }
                        if *priority > previous.priority {
                            let mut upgraded = previous.clone();
                            upgraded.priority = *priority;
                            let change = MemoryChange::Upsert(upgraded);
                            next.apply_committed(&change)?;
                            changes.push(change);
                        }
                        continue;
                    }
                    let change = MemoryChange::Upsert(MemoryRecord::fact(
                        MemoryId::new(),
                        *priority,
                        content.clone(),
                    ));
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::EnsureFact { priority, content } => {
                    if content.trim().is_empty() || content.chars().count() > 2000 {
                        return Err("invalid memory record");
                    }
                    // Python skip_if_exists compares exact text, not normalized
                    // lexical similarity. Ambiguous retained identities must not
                    // be chosen based on map iteration order.
                    let mut matches = next.records.values().filter(|record| {
                        record.kind == MemoryKind::Fact && record.content == *content
                    });
                    let existing = matches.next().cloned();
                    if matches.next().is_some() {
                        return Err("ambiguous retained exact memory match");
                    }
                    if let Some(mut record) = existing {
                        if record.state == MemoryState::Active {
                            continue;
                        }
                        if record.protected {
                            return Err("protected memory cannot be restored");
                        }
                        record.state = MemoryState::Active;
                        record.priority = *priority;
                        let change = MemoryChange::Upsert(record);
                        next.apply_committed(&change)?;
                        changes.push(change);
                    } else {
                        let change = MemoryChange::Upsert(MemoryRecord::fact(
                            MemoryId::new(),
                            *priority,
                            content.clone(),
                        ));
                        next.apply_committed(&change)?;
                        changes.push(change);
                    }
                }
                MemoryCommand::Update {
                    id,
                    priority,
                    content,
                } => {
                    let mut record = next.get(*id).ok_or("unknown memory id")?.clone();
                    if record.state != MemoryState::Active || record.kind != MemoryKind::Fact {
                        return Err("memory is not an active fact");
                    }
                    if record.protected {
                        return Err("protected memory cannot be edited before unprotecting");
                    }
                    record.content = content.clone();
                    if let Some(priority) = priority {
                        record.priority = *priority;
                    }
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Delete { id } => {
                    let change = MemoryChange::Delete(*id);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Merge {
                    target,
                    sources,
                    content,
                } => {
                    let mut target_record =
                        next.get(*target).ok_or("unknown merge target")?.clone();
                    if target_record.kind != MemoryKind::Fact
                        || target_record.state != MemoryState::Active
                    {
                        return Err("merge requires active facts");
                    }
                    if target_record.protected {
                        return Err("protected memory cannot be merged");
                    }
                    if sources.is_empty() {
                        return Err("merge requires sources");
                    }
                    let mut unique = BTreeSet::new();
                    unique.insert(*target);
                    let mut combined = vec![target_record.content.clone()];
                    for source in sources {
                        if !unique.insert(*source) {
                            return Err("duplicate merge memory id");
                        }
                        let source_record = next.get(*source).ok_or("unknown merge source")?;
                        if source_record.kind != MemoryKind::Fact
                            || source_record.state != MemoryState::Active
                        {
                            return Err("merge requires active facts");
                        }
                        if source_record.protected {
                            return Err("protected memory cannot be merged");
                        }
                        combined.push(source_record.content.clone());
                    }
                    target_record.content = content.clone().unwrap_or_else(|| combined.join(" | "));
                    for source in sources {
                        let mut retained =
                            next.get(*source).expect("validated merge source").clone();
                        retained.state = MemoryState::Deleted;
                        let change = MemoryChange::Upsert(retained);
                        next.apply_committed(&change)?;
                        changes.push(change);
                    }
                    let change = MemoryChange::Upsert(target_record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SetIsland { island, content } => {
                    let record = next
                        .records
                        .values()
                        .find(|record| {
                            record.kind == MemoryKind::Island(*island)
                                && record.state != MemoryState::Deleted
                        })
                        .cloned();
                    if record
                        .as_ref()
                        .is_some_and(|item| item.state != MemoryState::Active)
                    {
                        return Err("forgotten island must be restored explicitly");
                    }
                    if record.as_ref().is_some_and(|item| item.protected) {
                        return Err("protected memory cannot be edited before unprotecting");
                    }
                    let mut record = record.unwrap_or_else(|| MemoryRecord {
                        kind: MemoryKind::Island(*island),
                        ..MemoryRecord::fact(MemoryId::new(), MemoryPriority::Normal, "")
                    });
                    record.content = content.clone();
                    record.state = MemoryState::Active;
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SeedIsland { island, content } => {
                    if content.trim().is_empty() || content.chars().count() > 2000 {
                        return Err("invalid memory record");
                    }
                    if next
                        .records
                        .values()
                        .any(|record| record.kind == MemoryKind::Island(*island))
                    {
                        continue;
                    }
                    let mut record =
                        MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, content.clone());
                    record.kind = MemoryKind::Island(*island);
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Forget { id }
                | MemoryCommand::Restore { id }
                | MemoryCommand::SoftDelete { id } => {
                    let record = next.get(*id).ok_or("unknown memory id")?;
                    let desired = match command {
                        MemoryCommand::Forget { .. } => MemoryState::Forgotten,
                        MemoryCommand::Restore { .. } => MemoryState::Active,
                        MemoryCommand::SoftDelete { .. } => MemoryState::Deleted,
                        _ => unreachable!(),
                    };
                    if record.state == desired {
                        continue;
                    }
                    if record.protected && desired != MemoryState::Active {
                        return Err("protected memory cannot be forgotten or deleted");
                    }
                    if desired == MemoryState::Forgotten && record.state != MemoryState::Active {
                        return Err("only active memory may be forgotten");
                    }
                    let mut changed = record.clone();
                    changed.state = desired;
                    let change = MemoryChange::Upsert(changed);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SetExpiry {
                    id,
                    expires_at_unix_ms,
                } => {
                    let record = next.get(*id).ok_or("unknown memory id")?;
                    if record.state == MemoryState::Deleted {
                        return Err("deleted memory cannot be rescheduled");
                    }
                    if record.protected {
                        return Err("protected memory cannot be rescheduled");
                    }
                    let mut changed = record.clone();
                    changed.expires_at_unix_ms = *expires_at_unix_ms;
                    let change = MemoryChange::Upsert(changed);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SetProtection { id, protected } => {
                    let record = next.get(*id).ok_or("unknown memory id")?;
                    if record.state == MemoryState::Deleted {
                        return Err("deleted memory cannot change protection");
                    }
                    if record.protected == *protected {
                        continue;
                    }
                    let change = MemoryChange::SetProtection {
                        id: *id,
                        protected: *protected,
                    };
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
            }
            next.ensure_unique()?;
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_add_coalesces_identity_and_can_upgrade_priority() {
        let id = MemoryId::new();
        let mut book = CharacterMemories::default();
        book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
            id,
            priority: MemoryPriority::Low,
            content: "Player likes TEA".into(),
            kind: crate::memory::MemoryKind::Fact,
            state: crate::memory::MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        }))
        .unwrap();
        let changes = book
            .resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: " player   likes tea ".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Low,
                    content: "PLAYER likes tea".into(),
                },
            ])
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0],
            MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: "Player likes TEA".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            })
        );
        book.apply_committed(&changes[0]).unwrap();
        assert_eq!(book.list().len(), 1);
        assert_eq!(book.get(id).unwrap().priority, MemoryPriority::Normal);
    }

    #[test]
    fn exact_duplicates_inside_batch_do_not_allocate_multiple_ids() {
        let changes = CharacterMemories::default()
            .resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Remember me".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "  remember   ME ".into(),
                },
            ])
            .unwrap();
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn update_to_duplicate_fails_without_mutating_existing_snapshot() {
        let left = MemoryId::new();
        let right = MemoryId::new();
        let mut book = CharacterMemories::default();
        for (id, content) in [(left, "tea"), (right, "coffee")] {
            book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: content.into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            }))
            .unwrap();
        }
        let old = book.clone();
        assert!(
            book.resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Critical,
                    content: "new memory".into(),
                },
                MemoryCommand::Update {
                    id: right,
                    priority: None,
                    content: " TEA ".into(),
                },
            ])
            .is_err()
        );
        assert_eq!(book, old);
    }

    #[test]
    fn merge_is_atomic_and_preserves_target_identity() {
        let a = MemoryId::new();
        let b = MemoryId::new();
        let mut book = CharacterMemories::default();
        for (id, content) in [(a, "first"), (b, "second")] {
            book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: content.into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            }))
            .unwrap();
        }
        let edits = book
            .resolve(&[MemoryCommand::Merge {
                target: a,
                sources: vec![b],
                content: None,
            }])
            .unwrap();
        for edit in edits {
            book.apply_committed(&edit).unwrap();
        }
        assert_eq!(book.list().len(), 1);
        assert_eq!(book.get(a).unwrap().content, "first | second");
        assert_eq!(book.get(b).unwrap().state, MemoryState::Deleted);
        assert!(
            book.resolve(&[MemoryCommand::Merge {
                target: a,
                sources: vec![b],
                content: None,
            }])
            .is_err()
        );
    }
    fn apply(book: &mut CharacterMemories, commands: &[MemoryCommand]) {
        let changes = book.resolve(commands).expect("valid memory batch");
        for change in changes {
            book.apply_committed(&change).expect("committed change");
        }
    }

    #[test]
    fn legacy_record_json_is_stable_and_extensions_are_strict() {
        let id = MemoryId(Uuid::from_u128(17));
        let legacy = serde_json::json!({"id": id, "priority": "normal", "content": "Tea"});
        let record: MemoryRecord = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(record.kind, MemoryKind::Fact);
        assert_eq!(record.state, MemoryState::Active);
        assert_eq!(record.expires_at_unix_ms, None);
        assert_eq!(serde_json::to_value(&record).unwrap(), legacy);
        assert!(
            serde_json::from_value::<MemoryRecord>(serde_json::json!({
                "id": id, "priority": "normal", "content": "Tea", "unknown": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn forgotten_deleted_and_restored_are_distinct_explicit_states() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Player likes tea".into(),
            }],
        );
        let id = book.list()[0].id;
        apply(&mut book, &[MemoryCommand::Forget { id }]);
        assert!(book.list().is_empty());
        assert_eq!(book.forgotten()[0].id, id);
        assert!(
            book.resolve(&[MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "PLAYER LIKES TEA".into()
            }])
            .is_err()
        );
        apply(&mut book, &[MemoryCommand::Restore { id }]);
        assert_eq!(book.list()[0].id, id);
        apply(&mut book, &[MemoryCommand::SoftDelete { id }]);
        assert!(book.list().is_empty());
        assert!(book.forgotten().is_empty());
        assert_eq!(book.all().len(), 1);
        assert_eq!(book.get(id).unwrap().state, MemoryState::Deleted);
        apply(&mut book, &[MemoryCommand::Restore { id }]);
        assert_eq!(book.list()[0].id, id);
        apply(&mut book, &[MemoryCommand::SoftDelete { id }]);
        apply(&mut book, &[MemoryCommand::Delete { id }]);
        assert!(book.all().is_empty());
    }

    #[test]
    fn islands_are_typed_singletons_separate_from_fact_capacity() {
        let mut book = CharacterMemories::default();
        let islands = [
            MemoryIsland::Relationship,
            MemoryIsland::Opinion,
            MemoryIsland::Preferences,
            MemoryIsland::CommitmentsConflicts,
            MemoryIsland::Language,
        ];
        for island in islands {
            apply(
                &mut book,
                &[MemoryCommand::SetIsland {
                    island,
                    content: format!("first {island:?}"),
                }],
            );
        }
        let original = book
            .islands()
            .iter()
            .map(|item| (item.kind, item.id))
            .collect::<BTreeMap<_, _>>();
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "updated preferences".into(),
            }],
        );
        assert_eq!(book.islands().len(), 5);
        assert_eq!(
            book.islands()
                .iter()
                .map(|item| (item.kind, item.id))
                .collect::<BTreeMap<_, _>>(),
            original
        );
        assert!(book.list().is_empty());
        assert_eq!(
            book.islands()
                .iter()
                .find(|r| r.kind == MemoryKind::Island(MemoryIsland::Preferences))
                .unwrap()
                .content,
            "updated preferences"
        );
        let id = original[&MemoryKind::Island(MemoryIsland::Preferences)];
        assert!(
            book.resolve(&[MemoryCommand::Merge {
                target: id,
                sources: vec![original[&MemoryKind::Island(MemoryIsland::Language)]],
                content: None
            }])
            .is_err()
        );
    }

    #[test]
    fn expiry_is_explicit_idempotent_and_never_mutates_read() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "temporary fact".into(),
            }],
        );
        let id = book.list()[0].id;
        apply(
            &mut book,
            &[MemoryCommand::SetExpiry {
                id,
                expires_at_unix_ms: Some(1000),
            }],
        );
        assert!(book.get(id).unwrap().available_at(999));
        assert!(!book.get(id).unwrap().available_at(1000));
        assert!(book.resolve_expirations(999).is_empty());
        assert_eq!(book.list().len(), 1);
        let changes = book.resolve_expirations(1000);
        assert_eq!(changes.len(), 1);
        assert_eq!(book.list().len(), 1);
        for change in changes {
            book.apply_committed(&change).unwrap();
        }
        assert!(book.resolve_expirations(2000).is_empty());
        assert!(book.list().is_empty());
        assert_eq!(book.forgotten()[0].id, id);
    }

    #[test]
    fn live_selection_excludes_expired_facts_and_islands_without_mutating_history() {
        let mut book = CharacterMemories::default();
        let fact = MemoryId::new();
        let island = MemoryId::new();
        let mut fact_record = MemoryRecord::fact(fact, MemoryPriority::High, "short-lived fact");
        fact_record.expires_at_unix_ms = Some(100);
        book.apply_committed(&MemoryChange::Upsert(fact_record))
            .unwrap();
        let mut island_record =
            MemoryRecord::fact(island, MemoryPriority::Normal, "temporary preference");
        island_record.kind = MemoryKind::Island(MemoryIsland::Preferences);
        island_record.expires_at_unix_ms = Some(100);
        book.apply_committed(&MemoryChange::Upsert(island_record))
            .unwrap();
        assert_eq!(book.list_at(99).len(), 1);
        assert_eq!(book.islands_at(99).len(), 1);
        assert!(book.list_at(100).is_empty());
        assert!(book.islands_at(100).is_empty());
        assert_eq!(book.all().len(), 2);
        assert_eq!(book.resolve_expirations(100).len(), 2);
    }

    #[test]
    fn protection_is_explicit_auditable_and_rejects_implicit_mutations() {
        let mut book = CharacterMemories::default();
        let id = MemoryId::new();
        let mut initial = MemoryRecord::fact(id, MemoryPriority::Critical, "permanent fact");
        initial.expires_at_unix_ms = Some(100);
        book.apply_committed(&MemoryChange::Upsert(initial.clone()))
            .unwrap();
        let protect = book
            .resolve(&[MemoryCommand::SetProtection {
                id,
                protected: true,
            }])
            .unwrap();
        assert_eq!(
            protect,
            vec![MemoryChange::SetProtection {
                id,
                protected: true
            }]
        );
        book.apply_committed(&protect[0]).unwrap();
        assert!(book.get(id).unwrap().protected);
        assert!(book.resolve_expirations(1000).is_empty());
        for forbidden in [
            MemoryCommand::Forget { id },
            MemoryCommand::SoftDelete { id },
            MemoryCommand::Delete { id },
            MemoryCommand::Update {
                id,
                content: "changed".into(),
                priority: None,
            },
            MemoryCommand::SetExpiry {
                id,
                expires_at_unix_ms: None,
            },
        ] {
            assert!(book.resolve(&[forbidden]).is_err());
        }
        assert_eq!(
            book.apply_committed(&MemoryChange::Delete(id)),
            Err("protected memory must be unprotected before purge")
        );
        let mut changed = book.get(id).unwrap().clone();
        changed.protected = false;
        assert_eq!(
            book.apply_committed(&MemoryChange::Upsert(changed)),
            Err("use SetProtection to change memory protection")
        );
        let unprotect = book
            .resolve(&[MemoryCommand::SetProtection {
                id,
                protected: false,
            }])
            .unwrap();
        book.apply_committed(&unprotect[0]).unwrap();
        assert_eq!(book.resolve_expirations(1000).len(), 1);
        assert_eq!(book.get(id).unwrap().state, MemoryState::Active);
    }

    #[test]
    fn protected_island_requires_unprotect_to_replace_or_delete() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "Keep this".into(),
            }],
        );
        let id = book.islands()[0].id;
        apply(
            &mut book,
            &[MemoryCommand::SetProtection {
                id,
                protected: true,
            }],
        );
        assert!(
            book.resolve(&[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "Overwrite".into(),
            }])
            .is_err()
        );
        apply(
            &mut book,
            &[MemoryCommand::SetProtection {
                id,
                protected: false,
            }],
        );
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "Overwrite".into(),
            }],
        );
        assert_eq!(book.islands()[0].id, id);
    }

    #[test]
    fn invalid_batch_has_no_partial_effects_and_unique_island_validation() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Language,
                content: "Russian".into(),
            }],
        );
        let initial = book.clone();
        assert!(
            book.resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "tea".into()
                },
                MemoryCommand::SetIsland {
                    island: MemoryIsland::Opinion,
                    content: "".into()
                },
            ])
            .is_err()
        );
        assert_eq!(book, initial);
        let mut duplicate =
            MemoryRecord::fact(MemoryId::new(), MemoryPriority::Normal, "duplicate");
        duplicate.kind = MemoryKind::Island(MemoryIsland::Language);
        book.apply_committed(&MemoryChange::Upsert(duplicate))
            .unwrap();
        assert_eq!(book.ensure_unique(), Err("duplicate memory island"));
    }

    #[test]
    fn ensure_fact_exact_match_restores_retained_identity_and_priority() {
        let id = MemoryId(Uuid::from_u128(501));
        let mut book = CharacterMemories::default();
        let mut record = MemoryRecord::fact(id, MemoryPriority::Low, "Original Text");
        record.expires_at_unix_ms = Some(1234);
        book.apply_committed(&MemoryChange::Upsert(record)).unwrap();

        apply(&mut book, &[MemoryCommand::Forget { id }]);
        assert!(
            book.resolve(&[MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Original Text".into(),
            }])
            .is_err()
        );
        let expected = MemoryCommand::EnsureFact {
            priority: MemoryPriority::High,
            content: "Original Text".into(),
        };
        let changes = book.resolve(std::slice::from_ref(&expected)).unwrap();
        assert_eq!(changes.len(), 1);
        let MemoryChange::Upsert(reactivated) = &changes[0] else {
            panic!("expected upsert")
        };
        assert_eq!(reactivated.id, id);
        assert_eq!(reactivated.state, MemoryState::Active);
        assert_eq!(reactivated.priority, MemoryPriority::High);
        assert_eq!(reactivated.expires_at_unix_ms, Some(1234));
        book.apply_committed(&changes[0]).unwrap();
        assert!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Critical,
                content: "Original Text".into(),
            }])
            .unwrap()
            .is_empty()
        );
        assert_eq!(book.get(id).unwrap().priority, MemoryPriority::High);

        apply(&mut book, &[MemoryCommand::SoftDelete { id }]);
        apply(
            &mut book,
            &[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Normal,
                content: "Original Text".into(),
            }],
        );
        assert_eq!(book.all().len(), 1);
        assert_eq!(book.get(id).unwrap().state, MemoryState::Active);
        assert_eq!(book.get(id).unwrap().priority, MemoryPriority::Normal);
    }

    #[test]
    fn ensure_fact_uses_exact_text_and_refuses_ambiguous_retained_matches() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Normal,
                content: "Tea time".into(),
            }],
        );
        let before = book.clone();
        assert!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Low,
                content: "tea time".into(),
            }])
            .is_err()
        );
        assert_eq!(book, before);

        let first = MemoryId(Uuid::from_u128(502));
        let second = MemoryId(Uuid::from_u128(503));
        for id in [first, second] {
            let mut record = MemoryRecord::fact(id, MemoryPriority::Low, "duplicated deleted row");
            record.state = MemoryState::Deleted;
            book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        }
        assert_eq!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::High,
                content: "duplicated deleted row".into(),
            }]),
            Err("ambiguous retained exact memory match")
        );
        assert!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Normal,
                content: "  ".into(),
            }])
            .is_err()
        );
        assert_eq!(book.get(first).unwrap().state, MemoryState::Deleted);
        assert_eq!(book.get(second).unwrap().state, MemoryState::Deleted);
        assert_eq!(book.all().len(), 3);
    }

    #[test]
    fn ensure_fact_rejects_protected_reactivation_and_batch_rolls_back() {
        let mut book = CharacterMemories::default();
        let id = MemoryId(Uuid::from_u128(504));
        let mut record = MemoryRecord::fact(id, MemoryPriority::High, "protected forgotten");
        record.state = MemoryState::Forgotten;
        record.protected = true;
        book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        let before = book.clone();
        assert!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Critical,
                content: "protected forgotten".into(),
            }])
            .is_err()
        );
        assert!(
            book.resolve(&[
                MemoryCommand::EnsureFact {
                    priority: MemoryPriority::Low,
                    content: "new fact".into(),
                },
                MemoryCommand::EnsureFact {
                    priority: MemoryPriority::Critical,
                    content: "protected forgotten".into(),
                },
            ])
            .is_err()
        );
        assert_eq!(book, before);
    }

    #[test]
    fn ensure_fact_batches_are_idempotent_and_capacity_guarded() {
        let mut book = CharacterMemories::default();
        let changes = book
            .resolve(&[
                MemoryCommand::EnsureFact {
                    priority: MemoryPriority::Low,
                    content: "one consistent fact".into(),
                },
                MemoryCommand::EnsureFact {
                    priority: MemoryPriority::High,
                    content: "one consistent fact".into(),
                },
            ])
            .unwrap();
        assert_eq!(changes.len(), 1);
        let MemoryChange::Upsert(record) = &changes[0] else {
            panic!("expected upsert");
        };
        assert_eq!(record.priority, MemoryPriority::Low);
        book.apply_committed(&changes[0]).unwrap();
        assert_eq!(book.all().len(), 1);

        for n in 1..MAX_ACTIVE_FACTS {
            let record = MemoryRecord::fact(
                MemoryId(Uuid::from_u128(1000 + n as u128)),
                MemoryPriority::Normal,
                format!("capacity item {n}"),
            );
            book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        }
        let mut hidden = MemoryRecord::fact(
            MemoryId(Uuid::from_u128(2000)),
            MemoryPriority::Low,
            "unavailable target",
        );
        hidden.state = MemoryState::Forgotten;
        book.apply_committed(&MemoryChange::Upsert(hidden)).unwrap();
        let original = book.clone();
        assert_eq!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::High,
                content: "unavailable target".into(),
            }]),
            Err("active memory capacity exceeded")
        );
        assert!(
            book.resolve(&[MemoryCommand::EnsureFact {
                priority: MemoryPriority::Normal,
                content: "new overflow".into(),
            }])
            .is_err()
        );
        assert_eq!(book, original);
    }

    #[test]
    fn seed_island_is_one_time_and_does_not_overwrite_any_retained_state() {
        let mut book = CharacterMemories::default();
        let seed = MemoryCommand::SeedIsland {
            island: MemoryIsland::Relationship,
            content: "initial relationship".into(),
        };
        let changes = book.resolve(&[seed.clone(), seed.clone()]).unwrap();
        assert_eq!(changes.len(), 1);
        book.apply_committed(&changes[0]).unwrap();
        let original = book.all()[0].clone();
        assert_eq!(original.priority, MemoryPriority::High);
        assert_eq!(
            original.kind,
            MemoryKind::Island(MemoryIsland::Relationship)
        );
        assert!(
            book.resolve(&[MemoryCommand::SeedIsland {
                island: MemoryIsland::Relationship,
                content: "attempted overwrite".into(),
            }])
            .unwrap()
            .is_empty()
        );
        assert_eq!(book.all(), vec![original.clone()]);

        apply(&mut book, &[MemoryCommand::Forget { id: original.id }]);
        assert!(
            book.resolve(std::slice::from_ref(&seed))
                .unwrap()
                .is_empty()
        );
        assert_eq!(book.all()[0].state, MemoryState::Forgotten);
        apply(&mut book, &[MemoryCommand::SoftDelete { id: original.id }]);
        assert!(book.resolve(&[seed]).unwrap().is_empty());
        assert_eq!(book.all()[0].state, MemoryState::Deleted);
        assert_eq!(book.all()[0].id, original.id);
    }

    #[test]
    fn seed_island_preserves_manual_updates_and_protection() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::SeedIsland {
                island: MemoryIsland::Preferences,
                content: "original preferences".into(),
            }],
        );
        let id = book.islands()[0].id;
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "updated preferences".into(),
            }],
        );
        apply(
            &mut book,
            &[MemoryCommand::SetProtection {
                id,
                protected: true,
            }],
        );
        assert!(
            book.resolve(&[MemoryCommand::SeedIsland {
                island: MemoryIsland::Preferences,
                content: "should not change".into(),
            }])
            .unwrap()
            .is_empty()
        );
        assert_eq!(book.islands()[0].id, id);
        assert_eq!(book.islands()[0].content, "updated preferences");
        assert!(book.islands()[0].protected);
        assert!(
            book.resolve(&[MemoryCommand::SeedIsland {
                island: MemoryIsland::Language,
                content: "   ".into(),
            }])
            .is_err()
        );
        assert!(
            book.resolve(&[
                MemoryCommand::SeedIsland {
                    island: MemoryIsland::Language,
                    content: "language seed".into(),
                },
                MemoryCommand::SeedIsland {
                    island: MemoryIsland::Opinion,
                    content: " ".into(),
                },
            ])
            .is_err()
        );
        assert_eq!(book.all().len(), 1);
    }

    #[test]
    fn capacity_preview_is_pure_priority_ordered_and_excludes_protected_and_islands() {
        let mut book = CharacterMemories::default();
        let mut fixture = [
            (10, MemoryPriority::Low, false, MemoryKind::Fact),
            (2, MemoryPriority::Low, false, MemoryKind::Fact),
            (6, MemoryPriority::Normal, false, MemoryKind::Fact),
            (7, MemoryPriority::Low, true, MemoryKind::Fact),
            (8, MemoryPriority::Critical, false, MemoryKind::Fact),
            (
                9,
                MemoryPriority::Low,
                false,
                MemoryKind::Island(MemoryIsland::Language),
            ),
        ];
        fixture.reverse();
        for (number, priority, protected, kind) in fixture {
            let mut record = MemoryRecord::fact(
                MemoryId(Uuid::from_u128(number)),
                priority,
                format!("unique {number}"),
            );
            record.protected = protected;
            record.kind = kind;
            book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        }
        let before = book.clone();
        let plan = MemoryCapacityPlan::preview(&book.all(), 3).unwrap();
        assert_eq!(plan.active_facts(), 5);
        assert_eq!(plan.limit(), 3);
        assert_eq!(
            plan.victims(),
            &[MemoryId(Uuid::from_u128(2)), MemoryId(Uuid::from_u128(10))]
        );
        assert_eq!(book, before);
        let changes = book.resolve(&plan.forget_commands()).unwrap();
        assert_eq!(changes.len(), 2);
        for change in &changes {
            book.apply_committed(change).unwrap();
        }
        assert_eq!(book.list().len(), 3);
        assert_eq!(book.islands().len(), 1);
        assert_eq!(book.forgotten().len(), 2);
        assert!(
            MemoryCapacityPlan::preview(&book.all(), 3)
                .unwrap()
                .is_noop()
        );
    }

    #[test]
    fn capacity_plan_fails_atomically_if_critical_or_protected_block_target() {
        let mut book = CharacterMemories::default();
        let protected_id = MemoryId(Uuid::from_u128(1));
        let critical_id = MemoryId(Uuid::from_u128(2));
        let normal_id = MemoryId(Uuid::from_u128(3));
        for (id, priority, protected) in [
            (protected_id, MemoryPriority::Low, true),
            (critical_id, MemoryPriority::Critical, false),
            (normal_id, MemoryPriority::Normal, false),
        ] {
            let mut record = MemoryRecord::fact(id, priority, format!("fact {id:?}"));
            record.protected = protected;
            book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        }
        let original = book.clone();
        assert_eq!(
            MemoryCapacityPlan::preview(&book.all(), 1),
            Err("insufficient forgettable memories to reach capacity target")
        );
        assert!(MemoryCapacityPlan::preview(&book.all(), 0).is_err());
        assert!(MemoryCapacityPlan::preview(&book.all(), 257).is_err());
        assert_eq!(book, original);
        assert_eq!(
            MemoryCapacityPlan::preview(&book.all(), 2)
                .unwrap()
                .victims(),
            &[normal_id]
        );
    }
}
