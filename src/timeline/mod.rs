use std::{collections::HashMap, sync::Arc};

use tokio::sync::watch;

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    identity::{CharacterId, RequestId},
    interaction::TurnIntent,
};

pub use crate::identity::EventId;
#[cfg(test)]
mod context_read_tests;
pub mod conversation;
pub mod domain;
pub mod ingress;
mod legacy;
#[cfg(test)]
mod memory_maintenance_tests;
mod memory_operations;
// Administrative purge is not reachable from generation or runtime until an authorized UI exists.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod memory_purge;
#[cfg(test)]
mod memory_purge_tests;
pub mod message_actions;
pub mod selection;
pub mod storage;
pub mod view;

use conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker};
#[cfg(all(test, feature = "sqlite-system"))]
use domain::{CharacterBehavior, MemoryChange};
use legacy::{legacy_event, make_draft};
use message_actions::MessageAction;
use selection::{SessionWorkspace, TimelineAdmission, TimelineSelection, TimelineTurn};
use view::MitaView;
#[cfg(test)]
use view::TimelineMessageQueries;

#[cfg(feature = "sqlite-system")]
use selection::TimelineSelectionRepository;

use domain::{
    AppendGuard, AppliedEffect, BranchId, CausalLink, CausalRelation, CharacterMemories,
    CharacterWorkingState, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore,
};

#[derive(Debug, Clone)]
pub(crate) struct AcceptedReplyEvents {
    pub message: EventId,
    pub game: Vec<EventId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAnchor {
    pub branch_id: Uuid,
    pub through: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPosition {
    Latest,
    At(ContextAnchor),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContextCaptureError {
    #[error("generation context belongs to a prior character knowledge epoch")]
    StaleKnowledge,
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventContent {
    PlayerMessage {
        text: Arc<str>,
    },
    Observation {
        text: Arc<str>,
    },
    Reaction {
        text: Arc<str>,
        target: EventId,
    },
    Initiation {
        text: Arc<str>,
    },
    CharacterReply {
        text: Arc<str>,
        reply_to: EventId,
        context: ContextAnchor,
    },
}

#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub id: EventId,
    pub sequence: u64,
    pub observer: CharacterId,
    pub request_id: Option<RequestId>,
    pub content: EventContent,
}

#[derive(Debug, Clone)]
pub struct HistoryView {
    pub at: ContextAnchor,
    pub events: Vec<TimelineEvent>,
}

/// Two read models bound to the same immutable timeline prefix.
/// The legacy view is retained for clients that have not migrated yet.
pub(crate) struct GameCommit {
    pub state: crate::games::GameSlot,
    pub expected_head: u64,
}

pub(crate) struct GeneratedReply {
    pub request: RequestId,
    pub intent: TurnIntent,
    pub turn: TimelineTurn,
    pub expected_revision: u64,
    pub content: serde_json::Value,
    pub audience: ConversationAudience,
    pub effects: Vec<AppliedEffect>,
}

pub(crate) struct TimelineContextSnapshot {
    pub(crate) state: crate::character::CharacterState,
    pub(crate) working_state: CharacterWorkingState,
    pub(crate) memories: CharacterMemories,
    pub(crate) known: Vec<domain::KnownEvent>,
    pub(crate) game: crate::games::GameSlot,
    pub(crate) game_token: Option<EventId>,
    pub(crate) history: HistoryView,
    pub(crate) view: MitaView,
}

pub enum TimelineConfig {
    Transient,
    Persistent {
        database: std::path::PathBuf,
    },
    #[cfg(feature = "sqlite-system")]
    Sqlite {
        database: std::path::PathBuf,
        session: SessionId,
        root: BranchId,
    },
}

pub(crate) struct SessionTimeline {
    branch: BranchId,
    store: Box<dyn TimelineRepository + Send>,
    workspace: SessionWorkspace,
    selection_updates: watch::Sender<TimelineSelection>,
    context_updates: HashMap<CharacterId, watch::Sender<u64>>,
    memory_updates: HashMap<CharacterId, watch::Sender<u64>>,
    #[cfg(feature = "sqlite-system")]
    selection_repository: Option<storage::SqliteTimelineRepository>,
}

impl SessionTimeline {
    pub(crate) fn new() -> Self {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store
            .create_session(SessionId::new(), branch)
            .expect("fresh session must create its branch");
        let mut workspace = SessionWorkspace::default();
        workspace
            .select(&store, branch, None)
            .expect("fresh branch exists");
        let context_updates =
            Self::context_channels(&store, branch).expect("fresh branch context epochs exist");
        Self {
            branch,
            store: Box::new(store),
            selection_updates: watch::channel(workspace.selected().expect("selection set")).0,
            context_updates,
            memory_updates: Self::memory_channels(),
            workspace,
            #[cfg(feature = "sqlite-system")]
            selection_repository: None,
        }
    }

    pub(crate) fn from_config(config: TimelineConfig) -> Result<Self, domain::TimelineError> {
        match config {
            TimelineConfig::Transient => Ok(Self::new()),
            TimelineConfig::Persistent { database } => {
                #[cfg(feature = "sqlite-system")]
                {
                    let mut repository = storage::SqliteTimelineRepository::open(&database)?;
                    let (session, root) = repository.open_or_create_session()?;
                    Self::from_config(TimelineConfig::Sqlite {
                        database,
                        session,
                        root,
                    })
                }
                #[cfg(not(feature = "sqlite-system"))]
                {
                    let _ = database;
                    Err(domain::TimelineError::Storage(
                        "persistent Timeline requires the sqlite-system feature".into(),
                    ))
                }
            }
            #[cfg(feature = "sqlite-system")]
            TimelineConfig::Sqlite {
                database,
                session,
                root,
            } => {
                use storage::SqliteTimelineRepository;
                let mut store = SqliteTimelineRepository::open(&database)?;
                store.create_session(session, root)?;
                let mut selection_repository = SqliteTimelineRepository::open(database)?;
                let selection = match selection_repository.selection()? {
                    Some(selection) => selection,
                    None => selection_repository.select_branch(root, None)?,
                };
                if store.branch(selection.branch_id)?.session_id != session {
                    return Err(domain::TimelineError::Conflict);
                }
                let context_updates = Self::context_channels(&store, selection.branch_id)?;
                Ok(Self {
                    branch: selection.branch_id,
                    store: Box::new(store),
                    workspace: SessionWorkspace::from_selection(selection),
                    selection_updates: watch::channel(selection).0,
                    context_updates,
                    memory_updates: Self::memory_channels(),
                    selection_repository: Some(selection_repository),
                })
            }
        }
    }

    pub(crate) fn capture_turn(
        &self,
        character: CharacterId,
        trigger: Option<EventId>,
        context: ContextAnchor,
    ) -> Result<TimelineTurn, domain::TimelineError> {
        if context.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "generation context belongs to another timeline branch",
            ));
        }
        self.workspace
            .capture_at(self.store.as_ref(), character, trigger, context.through)
    }

    pub(crate) fn assert_selection(
        &self,
        selection: TimelineSelection,
    ) -> Result<(), domain::TimelineError> {
        if self.selection() != selection {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository
            && repository.selection()? != Some(selection)
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    pub(crate) fn admission(
        &self,
        character: CharacterId,
        selection: TimelineSelection,
    ) -> Result<TimelineAdmission, domain::TimelineError> {
        self.assert_selection(selection)?;
        Ok(TimelineAdmission {
            selection,
            context_epoch: self.store.context_epoch(self.branch, character)?,
        })
    }

    pub(crate) fn assert_admission(
        &self,
        character: CharacterId,
        admission: TimelineAdmission,
    ) -> Result<(), domain::TimelineError> {
        self.assert_selection(admission.selection)?;
        if self.store.context_epoch(self.branch, character)? != admission.context_epoch {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    pub(crate) fn assert_turn(&self, turn: TimelineTurn) -> Result<(), domain::TimelineError> {
        self.workspace.assert_current(self.store.as_ref(), turn)?;
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository
            && repository.selection()? != Some(turn.selection)
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    fn context_channels(
        store: &dyn TimelineRepository,
        branch: BranchId,
    ) -> Result<HashMap<CharacterId, watch::Sender<u64>>, domain::TimelineError> {
        CharacterId::ALL
            .into_iter()
            .map(|character| {
                let epoch = store.context_epoch(branch, character)?;
                Ok((character, watch::channel(epoch).0))
            })
            .collect()
    }

    fn memory_channels() -> HashMap<CharacterId, watch::Sender<u64>> {
        CharacterId::ALL
            .into_iter()
            .map(|character| (character, watch::channel(0).0))
            .collect()
    }

    /// In-process invalidation hint only. The Timeline remains the sole source of
    /// memory data, and receivers must read a new character snapshot after waking.
    pub(crate) fn observe_memory_changes(&self, character: CharacterId) -> watch::Receiver<u64> {
        self.memory_updates[&character].subscribe()
    }

    pub(crate) fn notify_memory_changed(&self, character: CharacterId) {
        self.memory_updates[&character].send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
    }

    pub(crate) fn observe_context_epoch(&self, character: CharacterId) -> watch::Receiver<u64> {
        self.context_updates[&character].subscribe()
    }

    /// Reset is a semantic fact, not a request or a mutation of past events.
    /// The local character watcher is notified only after the guarded append commits.
    pub(crate) fn reset_character_context(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        id: EventId,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.id = id;
        reset.observers.insert(character);
        self.store.append_guarded(
            self.branch,
            self.anchor().through,
            &format!("context-reset:{}", id.0),
            json!({"character": character.to_string()}),
            vec![reset],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        let epoch = self.store.context_epoch(self.branch, character)?;
        let sender = &self.context_updates[&character];
        if *sender.borrow() != epoch {
            sender.send_replace(epoch);
        }
        Ok(id)
    }

    pub(crate) fn observe_selection(&self) -> watch::Receiver<TimelineSelection> {
        self.selection_updates.subscribe()
    }

    pub(crate) fn selection(&self) -> TimelineSelection {
        self.workspace
            .selected()
            .expect("active timeline selection exists")
    }

    pub(crate) fn select_branch(
        &mut self,
        branch: BranchId,
        expected: TimelineSelection,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Conflict);
        }
        if self.workspace.selected() != Some(expected) {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let epochs = CharacterId::ALL
            .into_iter()
            .map(|character| {
                self.store
                    .context_epoch(branch, character)
                    .map(|epoch| (character, epoch))
            })
            .collect::<Result<Vec<_>, _>>()?;
        #[cfg(feature = "sqlite-system")]
        let new_selection = if let Some(repository) = &mut self.selection_repository {
            repository.select_branch(branch, Some(expected))?
        } else {
            self.workspace
                .select(self.store.as_ref(), branch, Some(expected))?
        };
        #[cfg(not(feature = "sqlite-system"))]
        let new_selection = self
            .workspace
            .select(self.store.as_ref(), branch, Some(expected))?;
        self.workspace.install_selection(new_selection);
        self.branch = branch;
        for (character, epoch) in epochs {
            self.context_updates[&character].send_replace(epoch);
        }
        self.selection_updates.send_replace(new_selection);
        for character in CharacterId::ALL {
            self.notify_memory_changed(character);
        }
        Ok(new_selection)
    }

    #[cfg(test)]
    pub(crate) fn append_test_fact(
        &mut self,
        key: &str,
        draft: EventDraft,
    ) -> Result<(), domain::TimelineError> {
        let head = self.anchor().through;
        self.store
            .append(self.branch, head, key, json!({}), vec![draft])?;
        Ok(())
    }

    /// Apply expired, unprotected memories in one guarded Timeline transaction.
    /// The scheduler/caller supplies time and selection; no read-side mutation occurs.
    pub(crate) fn expire_character_memories(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        as_of_unix_ms: u64,
    ) -> Result<usize, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        let facts = self.store.world(self.branch)?;
        let memories = crate::character::projection::character_memories_at(&facts, character)?;
        let changes = memories.resolve_expirations(as_of_unix_ms);
        if changes.is_empty() {
            return Ok(0);
        }
        let count = changes.len();
        let mut drafts = Vec::with_capacity(count);
        for change in changes {
            let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            draft.payload.insert(
                "change".into(),
                serde_json::to_value(change)
                    .map_err(|_| domain::TimelineError::Invalid("invalid expiry change"))?,
            );
            drafts.push(draft);
        }
        self.store.append_guarded(
            self.branch,
            head,
            &format!("memory-expiry:{character}:{as_of_unix_ms}:{head}"),
            json!({"character": character.to_string(), "as_of_unix_ms": as_of_unix_ms}),
            drafts,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        self.notify_memory_changed(character);
        Ok(count)
    }

    pub(crate) fn anchor(&self) -> ContextAnchor {
        ContextAnchor {
            branch_id: self.branch.0,
            through: self
                .store
                .branch(self.branch)
                .expect("owned branch exists")
                .head_sequence,
        }
    }

    pub(crate) fn capture_context(
        &self,
        character: CharacterId,
        position: ContextPosition,
    ) -> Result<TimelineContextSnapshot, ContextCaptureError> {
        let anchor = match position {
            ContextPosition::Latest => self.anchor(),
            ContextPosition::At(at) => at,
        };
        if anchor.branch_id != self.branch.0 {
            return Err(ContextCaptureError::Invalid(
                "generation context belongs to another timeline branch".into(),
            ));
        }
        if matches!(position, ContextPosition::At(_)) {
            let captured_epoch = self
                .store
                .context_epoch_at(self.branch, character, Some(anchor.through))
                .map_err(|error| ContextCaptureError::Invalid(error.to_string()))?;
            let current_epoch = self
                .store
                .context_epoch(self.branch, character)
                .map_err(|error| ContextCaptureError::Invalid(error.to_string()))?;
            if captured_epoch != current_epoch {
                return Err(ContextCaptureError::StaleKnowledge);
            }
        }
        self.inspect_context_at(character, anchor)
            .map_err(ContextCaptureError::Invalid)
    }

    /// Read a saved branch without selecting it or changing pending generations.
    pub(crate) fn inspect_context_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<TimelineContextSnapshot, String> {
        let facts = self.read_prefix(at).map_err(|error| error.to_string())?;
        let known = domain::character_view(&facts, character);
        let history = HistoryView {
            at,
            events: known
                .iter()
                .filter_map(|item| legacy_event(&item.fact, character))
                .collect(),
        };
        let view = MitaView::from_facts(BranchId(at.branch_id), at.through, character, &facts);
        let state = crate::character::projection::character_state(&facts, character)
            .map_err(|error| error.to_string())?;
        let working_state =
            crate::character::projection::character_working_state_at(&facts, character)
                .map_err(|error| error.to_string())?;
        let memories = crate::character::projection::character_memories_at(&facts, character)
            .map_err(|error| error.to_string())?;
        let (game, game_token) = Self::game_from_facts(character, at, &facts)?;
        Ok(TimelineContextSnapshot {
            game,
            game_token,
            history,
            view,
            state,
            working_state,
            memories,
            known,
        })
    }

    pub(crate) fn game_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<(crate::games::GameSlot, Option<EventId>), String> {
        let facts = self.read_prefix(at).map_err(|error| error.to_string())?;
        Self::game_from_facts(character, at, &facts)
    }

    fn game_from_facts(
        character: CharacterId,
        at: ContextAnchor,
        facts: &[domain::TimelineFact],
    ) -> Result<(crate::games::GameSlot, Option<EventId>), String> {
        let mut game = crate::games::GameSlot::default();
        let mut token = None;
        for fact in facts.iter().rev() {
            if fact.draft.kind == EventKind::GameStateCaptured
                && fact.draft.actor.as_deref() == Some(character.to_string().as_str())
            {
                game = serde_json::from_value(fact.draft.payload["game_state"].clone())
                    .map_err(|error| format!("invalid canonical game: {error}"))?;
                game.validate().map_err(str::to_owned)?;
                token = Some(fact.draft.id);
                break;
            }
        }
        game.bind_branch(BranchId(at.branch_id));
        Ok((game, token))
    }

    fn game_state_draft(
        character: CharacterId,
        game: crate::games::GameSlot,
    ) -> Result<EventDraft, domain::TimelineError> {
        game.validate().map_err(domain::TimelineError::Invalid)?;
        let mut draft = EventDraft::new(EventKind::GameStateCaptured);
        draft.actor = Some(character.to_string());
        draft.observers.insert(character);
        draft.payload.insert(
            "game_state".into(),
            serde_json::to_value(game)
                .map_err(|_| domain::TimelineError::Invalid("invalid game state"))?,
        );
        Ok(draft)
    }

    pub(crate) fn character_state_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<crate::character::CharacterState, domain::TimelineError> {
        crate::character::projection::character_state(&self.read_prefix(at)?, character)
    }

    fn read_prefix(
        &self,
        at: ContextAnchor,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .read(branch, 0, Some(at.through), i64::MAX as usize, None)
    }

    pub(crate) fn reminder_facts_at(
        &self,
        at: ContextAnchor,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.read_prefix(at)
    }

    /// Facts of the selected branch that can carry reminder state. Unselected
    /// branches stay untouched, so their pending deliveries are never lost.
    pub(crate) fn reminder_facts(
        &self,
        selection: crate::timeline::selection::TimelineSelection,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.assert_selection(selection)?;
        self.read_prefix(self.anchor())
    }

    /// Head and facts of one branch in the active session, read as a single
    /// consistent prefix so a delivery guard and its append can use one CAS.
    pub(crate) fn reminder_snapshot_on(
        &self,
        branch: BranchId,
    ) -> Result<(u64, Vec<domain::TimelineFact>), domain::TimelineError> {
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Invalid(
                "branch belongs to another session",
            ));
        }
        let head = self.store.branch(branch)?.head_sequence;
        let facts = self.store.read(branch, 0, None, i64::MAX as usize, None)?;
        Ok((head, facts))
    }

    /// Append a delivery transition to the branch that owns the fired
    /// occurrence, even when another branch is selected. Only the three
    /// delivery transition kinds are accepted; this is not a general write
    /// bypass. The caller supplies the head it validated against, so a stale
    /// settlement is rejected instead of being re-read and applied blindly.
    pub(crate) fn append_delivery_transition(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        draft: domain::EventDraft,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        if !matches!(
            draft.kind,
            domain::EventKind::ReminderDeliverySettled
                | domain::EventKind::ReminderDeliveryFailed
                | domain::EventKind::ReminderDeliveryRetried
        ) {
            return Err(domain::TimelineError::Invalid(
                "only delivery transitions may target another branch",
            ));
        }
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Invalid(
                "branch belongs to another session",
            ));
        }
        self.store
            .append(branch, expected_head, key, json!({}), vec![draft])
    }

    pub(crate) fn append_reminder_facts(
        &mut self,
        expected: TimelineSelection,
        key: &str,
        drafts: Vec<domain::EventDraft>,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        if self.selection() != expected {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let anchor = self.anchor();
        self.store.append_guarded(
            self.branch,
            anchor.through,
            key,
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )
    }

    fn inspection_branch(&self, at: ContextAnchor) -> Result<BranchId, domain::TimelineError> {
        let branch = BranchId(at.branch_id);
        let active_session = self.store.branch(self.branch)?.session_id;
        let info = self.store.branch(branch)?;
        if info.session_id != active_session {
            return Err(domain::TimelineError::Invalid(
                "context belongs to another session",
            ));
        }
        if at.through > info.head_sequence {
            return Err(domain::TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        Ok(branch)
    }

    /// Count accepted replies in canonical history, independent of what the
    /// character currently remembers or what the UI projection hides.
    /// The value is branch-prefix scoped and survives context resets and reopen.
    #[cfg(test)]
    pub(crate) fn accepted_replies_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<u64, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        let facts = self.store.world(branch)?;
        domain::accepted_reply_count(
            facts.iter().take_while(|fact| fact.sequence <= at.through),
            character,
        )
    }

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_behavior_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterBehavior, domain::TimelineError> {
        crate::character::projection::character_behavior_at(&self.read_prefix(at)?, character)
    }

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_working_state_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterWorkingState, domain::TimelineError> {
        crate::character::projection::character_working_state_at(&self.read_prefix(at)?, character)
    }

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_memories_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterMemories, domain::TimelineError> {
        crate::character::projection::character_memories_at(&self.read_prefix(at)?, character)
    }

    /// Find only a committed character reply in an immutable branch prefix.
    /// Unanswered/cancelled/unfinished requests are intentionally indistinguishable.
    pub(crate) fn accepted_reply_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: ContextAnchor,
    ) -> Result<Option<domain::TimelineFact>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .accepted_reply_at(branch, character, request.0, Some(at.through))
    }

    /// Read-only evidence of ingress, proactive dispatch or committed reply.
    /// It does not recover the process-local request state or replay work.
    pub(crate) fn request_evidence_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: ContextAnchor,
    ) -> Result<domain::RequestEvidence, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .request_evidence_at(branch, character, request.0, Some(at.through))
    }

    /// Recover the request associated with an input identity retained before
    /// submitting it, without resurrecting process-local execution.
    pub(crate) fn request_evidence_for_input_at(
        &self,
        character: CharacterId,
        input: EventId,
        at: ContextAnchor,
    ) -> Result<Option<(RequestId, domain::RequestEvidence)>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .request_evidence_for_input_at(branch, character, input, Some(at.through))
            .map(|value| value.map(|(request, evidence)| (RequestId(request), evidence)))
    }

    pub(crate) fn reaction_cause_at(
        &self,
        character: CharacterId,
        intent: TurnIntent,
        at: ContextAnchor,
    ) -> Result<Option<domain::TimelineFact>, domain::TimelineError> {
        let target = match intent {
            TurnIntent::React(target) | TurnIntent::Reply(target) => target,
            TurnIntent::Initiate => return Ok(None),
        };
        if at.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "reaction context branch mismatch",
            ));
        }
        let fact = self.store.event(self.branch, target)?;
        if matches!(intent, TurnIntent::Reply(_)) && !fact.draft.kind.is_message() {
            return Err(domain::TimelineError::Invalid(
                "reply target must be a conversation message",
            ));
        }
        if fact.sequence > at.through
            || !self
                .store
                .knows(self.branch, character, target, Some(at.through))?
        {
            return Err(domain::TimelineError::Invalid(
                "reaction cause was not known at captured context",
            ));
        }
        Ok(Some(fact))
    }

    #[cfg(test)]
    pub(crate) fn history(&self, character: CharacterId) -> HistoryView {
        self.history_at(character, self.anchor())
            .expect("current branch anchor is valid")
    }

    #[cfg(test)]
    pub(crate) fn history_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<HistoryView, String> {
        if at.branch_id != self.branch.0 {
            return Err("invalid timeline position".into());
        }
        self.history_on_branch(character, self.branch, at)
    }

    #[cfg(test)]
    fn history_on_branch(
        &self,
        character: CharacterId,
        branch: BranchId,
        at: ContextAnchor,
    ) -> Result<HistoryView, String> {
        let known = self
            .store
            .view(branch, character, Some(at.through))
            .map_err(|error| error.to_string())?;
        Ok(HistoryView {
            at,
            events: known
                .iter()
                .filter_map(|item| legacy_event(&item.fact, character))
                .collect(),
        })
    }

    pub(crate) fn observe(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<EventId, String> {
        let mut draft = make_draft(id, EventKind::CharacterObservation, character, text, None);
        draft.actor = Some(character.to_string());
        self.commit(format!("observe:{}", id.0), draft, None, None)
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    pub(crate) fn at_event(&self, id: EventId) -> Option<ContextAnchor> {
        self.store
            .event(self.branch, id)
            .ok()
            .map(|event| ContextAnchor {
                branch_id: self.branch.0,
                through: event.sequence,
            })
    }

    pub(crate) fn ingest_semantic_events(
        &mut self,
        expected: TimelineSelection,
        batch: ingress::SemanticBatch,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        ingress::append_semantic_batch(
            self.store.as_mut(),
            self.branch,
            head,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
            batch,
        )
    }

    /// Commit a shared conversation message once for all its witnesses.
    /// The message is independent of LLM admission and never starts generation.
    pub(crate) fn publish_conversation_message(
        &mut self,
        expected: TimelineSelection,
        message: conversation::ConversationMessage,
    ) -> Result<domain::TimelineFact, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        conversation::append_conversation_message(
            self.store.as_mut(),
            self.branch,
            head,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
            message,
        )
    }

    /// Edit, hide, or restore the current character's visible conversation view.
    /// Changes are immutable facts and never modify another character's history.
    pub(crate) fn change_message(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        operation: EventId,
        action: MessageAction,
    ) -> Result<domain::TimelineFact, domain::TimelineError> {
        self.assert_selection(expected)?;
        let draft = action.into_draft(operation, character);
        let facts = self.store.append_guarded(
            self.branch,
            self.anchor().through,
            &format!("message-action:{}", operation.0),
            json!({"character": character.to_string()}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        facts
            .into_iter()
            .next()
            .ok_or(domain::TimelineError::Invalid(
                "message action must commit one fact",
            ))
    }

    pub(crate) fn publish_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<(), String> {
        self.write_player_message(
            character,
            id,
            text,
            None,
            ConversationAudience::player_to(character),
        )
    }

    fn write_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
        request: Option<RequestId>,
        audience: ConversationAudience,
    ) -> Result<(), String> {
        let mut message = ConversationMessage::new(
            id.0.to_string(),
            ConversationSpeaker::Player,
            json!(text.as_ref()),
            [character],
        );
        message.id = id;
        message.request_id = request;
        message.audience = audience;
        let draft = message.into_draft().map_err(|error| error.to_string())?;
        let operation_key = request.map_or_else(
            || format!("message:{}", id.0),
            |request| format!("input:{}", request.0),
        );
        self.commit(operation_key, draft, None, None)
            .map_err(|error| error.to_string())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn append_reply(
        &mut self,
        reply: GeneratedReply,
    ) -> Result<EventId, domain::TimelineError> {
        self.append_reply_tracked(reply, None)
            .map(|accepted| accepted.message)
    }

    pub(crate) fn append_reply_tracked(
        &mut self,
        reply: GeneratedReply,
        game_state: Option<GameCommit>,
    ) -> Result<AcceptedReplyEvents, domain::TimelineError> {
        let GeneratedReply {
            request,
            intent,
            turn,
            expected_revision,
            content,
            audience,
            effects,
        } = reply;
        let character = turn.character;
        self.assert_turn(turn)?;
        let context = ContextAnchor {
            branch_id: turn.selection.branch_id.0,
            through: turn.context_head,
        };
        if context.branch_id != self.branch.0 || context.through > self.anchor().through {
            return Err(domain::TimelineError::Invalid(
                "context does not belong to the active timeline",
            ));
        }
        let cause = match intent {
            TurnIntent::Reply(id) | TurnIntent::React(id) => {
                if turn.trigger != Some(id) {
                    return Err(domain::TimelineError::Invalid(
                        "generation cause does not match the captured turn",
                    ));
                }
                let fact = self.store.event(self.branch, id)?;
                if matches!(intent, TurnIntent::Reply(_)) && !fact.draft.kind.is_message() {
                    return Err(domain::TimelineError::Invalid(
                        "reply target must be a message",
                    ));
                }
                if fact.sequence > context.through
                    || !self.store.knows(self.branch, character, id, None)?
                {
                    return Err(domain::TimelineError::ConcurrentWrite);
                }
                Some(id)
            }
            TurnIntent::Initiate => {
                if turn.trigger.is_some() {
                    return Err(domain::TimelineError::Invalid(
                        "initiate must not have an event trigger",
                    ));
                }
                None
            }
        };
        let id = EventId::new();
        let mut message = ConversationMessage::new(
            id.0.to_string(),
            ConversationSpeaker::Character(character),
            content,
            [],
        );
        message.id = id;
        message.audience = audience;
        message.request_id = Some(request);
        message.context = Some(context);
        if let Some(cause) = cause {
            let relation = match intent {
                TurnIntent::Reply(_) => {
                    message.reply_to = Some(cause);
                    CausalRelation::ReplyTo
                }
                TurnIntent::React(_) => CausalRelation::ReactTo,
                TurnIntent::Initiate => unreachable!(),
            };
            message.links.push(CausalLink {
                relation,
                event_id: cause,
            });
        }
        let draft = message.into_draft()?;
        let mut drafts = vec![draft];
        let mut changes_memory = false;
        let occurred_at = crate::reminders::UtcInstant::from_unix_micros(
            i64::try_from(drafts[0].occurred_at_unix_ms)
                .ok()
                .and_then(|time| time.checked_mul(1000))
                .ok_or(domain::TimelineError::Invalid(
                    "reply timestamp out of reminder range",
                ))?,
        );
        for effect in effects {
            match effect {
                AppliedEffect::CharacterStateChanged(behavior) => {
                    if ![behavior.attitude, behavior.boredom, behavior.stress]
                        .iter()
                        .all(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    {
                        return Err(domain::TimelineError::Invalid("invalid character state"));
                    }
                    let mut change = EventDraft::new(EventKind::CharacterStateChanged);
                    change.actor = Some(character.to_string());
                    change.observers.insert(character);
                    change.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    change.payload.insert(
                        "state".into(),
                        serde_json::to_value(behavior).map_err(|_| {
                            domain::TimelineError::Invalid("invalid character state")
                        })?,
                    );
                    drafts.push(change);
                }
                AppliedEffect::CharacterMemoryChanged(memory_change) => {
                    changes_memory = true;
                    let mut memory = EventDraft::new(EventKind::CharacterMemoryChanged);
                    memory.actor = Some(character.to_string());
                    memory.observers.insert(character);
                    memory.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    memory.payload.insert(
                        "change".into(),
                        serde_json::to_value(memory_change)
                            .map_err(|_| domain::TimelineError::Invalid("invalid memory change"))?,
                    );
                    drafts.push(memory);
                }
                AppliedEffect::CharacterWorkingStateReplaced(state) => {
                    if !state.is_valid() {
                        return Err(domain::TimelineError::Invalid("invalid working state"));
                    }
                    let mut handoff = EventDraft::new(EventKind::CharacterWorkingStateReplaced);
                    handoff.actor = Some(character.to_string());
                    handoff.observers.insert(character);
                    handoff.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    handoff.payload.insert(
                        "working_state".into(),
                        serde_json::to_value(state)
                            .map_err(|_| domain::TimelineError::Invalid("invalid working state"))?,
                    );
                    drafts.push(handoff);
                }
                AppliedEffect::GameAdvanced(fact) => {
                    let kind = match &fact {
                        crate::games::GameFact::Started { .. } => EventKind::GameStarted,
                        crate::games::GameFact::PhaseChanged { .. }
                        | crate::games::GameFact::Shot { .. } => EventKind::GameAction,
                        crate::games::GameFact::Ended { .. } => EventKind::GameEnded,
                    };
                    let mut game = EventDraft::new(kind);
                    game.actor = Some(character.to_string());
                    game.observers.insert(character);
                    game.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    for (key, value) in fact.payload() {
                        game.payload.insert(key, value);
                    }
                    drafts.push(game);
                }
                AppliedEffect::ReminderChanged(change) => {
                    let reminder = crate::reminders::change_draft(&change, Some(id), occurred_at)
                        .map_err(|_| {
                        domain::TimelineError::Invalid("invalid reminder change")
                    })?;
                    drafts.push(reminder);
                }
                AppliedEffect::ReminderToolResult(content) => {
                    let result = crate::reminders::tool_result_draft(
                        character,
                        &content,
                        Some(id),
                        occurred_at,
                    )
                    .map_err(|_| domain::TimelineError::Invalid("invalid reminder tool result"))?;
                    drafts.push(result);
                }
            }
        }
        let expected_head = game_state
            .as_ref()
            .map_or_else(|| self.anchor().through, |commit| commit.expected_head);
        if let Some(GameCommit {
            state: mut game, ..
        }) = game_state
        {
            for draft in &drafts {
                if matches!(
                    draft.kind,
                    EventKind::GameStarted | EventKind::GameAction | EventKind::GameEnded
                ) {
                    game.record_event(draft.id);
                }
            }
            drafts.push(Self::game_state_draft(character, game)?);
        }
        let game_events = drafts
            .iter()
            .filter(|draft| {
                matches!(
                    draft.kind,
                    EventKind::GameStarted | EventKind::GameAction | EventKind::GameEnded
                )
            })
            .map(|draft| draft.id)
            .collect();
        let selected = self.selection();
        self.store.append_guarded(
            self.branch,
            expected_head,
            &format!("reply:{}", request.0),
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: selected.generation,
                context: Some((turn.character, turn.context_epoch, turn.trigger)),
                accepted_reply_revision: Some((character, expected_revision)),
            },
        )?;
        if changes_memory {
            self.notify_memory_changed(character);
        }
        Ok(AcceptedReplyEvents {
            message: id,
            game: game_events,
        })
    }

    /// Commit an owner command and its private state in one guarded batch.
    pub(crate) fn append_game_facts(
        &mut self,
        character: CharacterId,
        facts: &[crate::games::GameFact],
        state: &mut crate::games::GameSlot,
        expected_head: u64,
    ) -> Result<Vec<EventId>, domain::TimelineError> {
        let selected = self.selection();
        let mut drafts = Vec::with_capacity(facts.len());
        for fact in facts {
            let kind = match fact {
                crate::games::GameFact::Started { .. } => EventKind::GameStarted,
                crate::games::GameFact::PhaseChanged { .. }
                | crate::games::GameFact::Shot { .. } => EventKind::GameAction,
                crate::games::GameFact::Ended { .. } => EventKind::GameEnded,
            };
            let mut draft = EventDraft::new(kind);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            for (key, value) in fact.payload() {
                draft.payload.insert(key, value);
            }
            drafts.push(draft);
        }
        let ids: Vec<_> = drafts.iter().map(|draft| draft.id).collect();
        for id in &ids {
            state.record_event(*id);
        }
        drafts.push(Self::game_state_draft(character, state.clone())?);
        let key = format!("game:{character}:{}", uuid::Uuid::new_v4());
        self.store.append_guarded(
            self.branch,
            expected_head,
            &key,
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: selected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(ids)
    }

    pub(crate) fn events(&self) -> Vec<TimelineEvent> {
        self.store
            .world(self.branch)
            .expect("owned branch exists")
            .iter()
            .filter_map(|fact| {
                fact.draft
                    .observers
                    .iter()
                    .next()
                    .and_then(|&character| legacy_event(fact, character))
            })
            .collect()
    }

    fn commit(
        &mut self,
        operation_key: String,
        draft: EventDraft,
        turn: Option<TimelineTurn>,
        accepted_reply_revision: Option<(CharacterId, u64)>,
    ) -> Result<(), domain::TimelineError> {
        let current = self.selection();
        if turn.is_some_and(|turn| turn.selection != current) {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let guard = AppendGuard {
            selection_generation: current.generation,
            context: turn.map(|turn| (turn.character, turn.context_epoch, turn.trigger)),
            accepted_reply_revision,
        };
        let head = self.anchor().through;
        self.store.append_guarded(
            self.branch,
            head,
            &operation_key,
            json!({}),
            vec![draft],
            guard,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod inspection_tests;
#[cfg(test)]
mod message_action_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_accept_does_not_answer_a_forgotten_trigger() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let trigger = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, trigger, Arc::from("hello"))
            .unwrap();
        let turn = timeline
            .capture_turn(CharacterId::Crazy, Some(trigger), timeline.anchor())
            .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        timeline
            .store
            .append(timeline.branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        assert!(
            timeline
                .append_reply(GeneratedReply {
                    request,
                    intent: TurnIntent::Reply(trigger),
                    turn,
                    expected_revision: 0,
                    content: json!("answer"),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: Vec::new(),
                })
                .is_err()
        );
        assert_eq!(timeline.anchor().through, 2);
    }

    fn reset_and_retell(
        store: &mut dyn TimelineRepository,
        branch: BranchId,
        original: EventId,
        key: &str,
    ) {
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        let mut retelling = EventDraft::new(EventKind::PlayerMessage);
        retelling.actor = Some("player".into());
        retelling.observers.insert(CharacterId::Crazy);
        retelling.links.push(CausalLink {
            relation: CausalRelation::CausedBy,
            event_id: original,
        });
        let retelling_id = retelling.id;
        let head = store.branch(branch).unwrap().head_sequence;
        store
            .append(branch, head, key, json!({}), vec![reset, retelling])
            .unwrap();
        assert!(
            !store
                .knows(branch, CharacterId::Crazy, original, None)
                .unwrap()
        );
        assert!(
            store
                .knows(branch, CharacterId::Crazy, retelling_id, None)
                .unwrap()
        );
    }

    fn assert_stale_pin(
        timeline: &SessionTimeline,
        pin: TimelineTurn,
        original: EventId,
        at: ContextAnchor,
    ) {
        assert!(
            timeline
                .capture_turn(CharacterId::Crazy, Some(original), at)
                .is_err()
        );
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        assert!(
            timeline
                .store
                .context_epoch(timeline.branch, CharacterId::Crazy)
                .unwrap()
                > 0
        );
        assert_eq!(
            timeline.assert_turn(pin),
            Err(domain::TimelineError::ConcurrentWrite)
        );
    }

    #[test]
    fn captured_epoch_cannot_be_replaced_by_later_reset_and_retelling() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let captured = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap();
        let at = captured.history.at;
        assert_eq!(captured.view.messages()[0].content, "original");

        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        let branch = timeline.branch;
        reset_and_retell(
            timeline.store.as_mut(),
            branch,
            original,
            "reset-and-retell",
        );
        assert_stale_pin(&timeline, pin, original, at);
        assert_eq!(captured.view.messages()[0].content, "original");

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_reply(GeneratedReply {
                request: RequestId::new(),
                intent: TurnIntent::React(original),
                turn: pin,
                expected_revision: 0,
                content: json!("reaction"),
                audience: ConversationAudience::private_to(CharacterId::Crazy),
                effects: Vec::new(),
            }),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(timeline.anchor().through, before);
    }

    #[test]
    fn newer_unrelated_facts_do_not_invalidate_a_captured_epoch() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        timeline
            .publish_player_message(CharacterId::Crazy, EventId::new(), Arc::from("later"))
            .unwrap();
        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        timeline.assert_turn(pin).unwrap();
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn external_sqlite_reset_and_retelling_cannot_repin_old_prompt() {
        let path =
            std::env::temp_dir().join(format!("neuromita-snapshot-epoch-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let branch = BranchId::new();
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: branch,
        })
        .unwrap();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        let mut external = storage::SqliteTimelineRepository::open(&path).unwrap();
        reset_and_retell(&mut external, branch, original, "remote-reset-and-retell");
        assert_stale_pin(&timeline, pin, original, at);

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_reply(GeneratedReply {
                request: RequestId::new(),
                intent: TurnIntent::React(original),
                turn: pin,
                expected_revision: 0,
                content: json!("reaction"),
                audience: ConversationAudience::private_to(CharacterId::Crazy),
                effects: Vec::new(),
            }),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(timeline.anchor().through, before);
        drop(external);
        drop(timeline);
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn standalone_message_remains_durable_without_request_receipt() {
        let path = std::env::temp_dir().join(format!(
            "neuromita-standalone-message-{}.db",
            uuid::Uuid::new_v4()
        ));
        let session = SessionId::new();
        let root = BranchId::new();
        let id = EventId::new();
        for reopen in [false, true] {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            if !reopen {
                timeline
                    .publish_player_message(CharacterId::Crazy, id, Arc::from("independent input"))
                    .unwrap();
            }
            let events = timeline.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].id, id);
            assert_eq!(events[0].request_id, None);
            let captured = timeline
                .capture_context(CharacterId::Crazy, ContextPosition::Latest)
                .unwrap();
            assert_eq!(captured.view.messages()[0].content, "independent input");
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn memory_replays_historical_branch_prefix_and_sqlite_reopen() {
        use domain::{MemoryId, MemoryPriority, MemoryRecord};
        let path = std::env::temp_dir().join(format!("neuromita-memory-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let root = BranchId::new();
        let memory_id = MemoryId::new();
        let initial;
        let updated;
        {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let original = MemoryRecord {
                id: memory_id,
                priority: MemoryPriority::High,
                content: "first fact".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            };
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 0,
                    content: json!([{"text":"One"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
                        original,
                    ))],
                })
                .unwrap();
            initial = timeline.anchor();
            let changed = MemoryRecord {
                id: memory_id,
                priority: MemoryPriority::Critical,
                content: "corrected fact".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            };
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 1,
                    content: json!([{"text":"Two"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
                        changed,
                    ))],
                })
                .unwrap();
            updated = timeline.anchor();
            timeline
                .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let first = timeline
                .character_memories_at(CharacterId::Crazy, initial)
                .unwrap()
                .list();
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].content, "first fact");
            let later = timeline
                .character_memories_at(CharacterId::Crazy, updated)
                .unwrap()
                .list();
            assert_eq!(later[0].id, memory_id);
            assert_eq!(later[0].content, "corrected fact");
            assert_eq!(
                timeline
                    .character_memories_at(CharacterId::Crazy, timeline.anchor())
                    .unwrap()
                    .list(),
                later
            );
            assert!(
                timeline
                    .character_memories_at(CharacterId::Kind, timeline.anchor())
                    .unwrap()
                    .list()
                    .is_empty()
            );
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn working_state_replays_from_sqlite_and_resets_at_historical_anchor() {
        let path =
            std::env::temp_dir().join(format!("neuromita-working-effect-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let root = BranchId::new();
        let original_anchor;
        let reset_anchor;
        let snapshot = CharacterWorkingState {
            focus: "Need to clarify the question".into(),
            open_loops: vec!["Confirm intent".into()],
            ..CharacterWorkingState::default()
        };
        {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 0,
                    content: json!([{"text":"Hello"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterWorkingStateReplaced(
                        snapshot.clone(),
                    )],
                })
                .unwrap();
            assert_eq!(timeline.anchor().through, 2);
            original_anchor = timeline.anchor();
            timeline
                .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
            reset_anchor = timeline.anchor();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            assert_eq!(
                timeline
                    .character_working_state_at(CharacterId::Crazy, original_anchor)
                    .unwrap(),
                snapshot
            );
            assert!(
                timeline
                    .character_working_state_at(CharacterId::Crazy, reset_anchor)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                timeline
                    .character_working_state_at(CharacterId::Kind, original_anchor)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                timeline
                    .character_behavior_at(CharacterId::Crazy, reset_anchor)
                    .unwrap(),
                CharacterBehavior::default()
            );
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn legacy_api_is_backed_by_the_only_canonical_event_store() {
        let mut timeline = SessionTimeline::new();
        let event = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, event, Arc::from("hello"))
            .unwrap();
        assert_eq!(timeline.store.world(timeline.branch).unwrap().len(), 1);
        assert_eq!(timeline.events().len(), 1);
        assert_eq!(timeline.history(CharacterId::Crazy).events.len(), 1);
        assert!(timeline.history(CharacterId::Kind).events.is_empty());
    }
}
