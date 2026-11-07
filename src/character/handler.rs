use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    sync::{Notify, mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    CharacterContext, CharacterId, CharacterState, CharacterView,
    model::{Character, GenerationFocus, ReactionCause},
};
use crate::{
    character::memory_access::{
        CharacterMemorySnapshot, MemoryEditResult, edit_memory_snapshot, read_memory_snapshot,
    },
    games::{
        GameError, GamePolicy, GameReceipt, GameReply, GameSlot, InviteCause, PendingInvite,
        ReactionInvite, UserGameCommand, seabattle::Side as GameSide,
    },
    identity::RequestId,
    interaction::{TurnIntent, UserInput},
    memory::MemoryCommand,
    reminders::{ReminderProjection, resolve_requests},
    settings::{RemindersSettings, SettingsOwner},
    timeline::domain::{AppliedEffect, CharacterBehavior},
    timeline::{
        ContextAnchor, ContextCaptureError, ContextPosition, EventId, GeneratedReply,
        SessionTimeline,
        conversation::ConversationAudience,
        selection::{TimelineAdmission, TimelineTurn},
    },
    turn::{ResponseEffect, ResponseSegment, TurnProposal},
    voice::AudioArtifact,
};

/// Runtime coupling that lets an accepted reply commit reminder effects and
/// wake the owned scheduler without a second schedule authority.
#[derive(Clone)]
pub(crate) struct ReminderBridge {
    pub(crate) settings: Arc<SettingsOwner>,
    pub(crate) wake: Arc<Notify>,
    pub(crate) clock: Arc<dyn crate::reminders::Clock>,
}

#[derive(Debug, Clone, Error)]
pub enum CharacterError {
    #[error("character owner is unavailable")]
    Unavailable,
    #[error("character state changed since context preparation")]
    StaleState,
    #[error("invalid character memory operation: {0}")]
    InvalidMemory(&'static str),
    #[error("character operation failed: {0}")]
    Internal(String),
    #[error("invalid mini-game command: {0}")]
    InvalidGame(GameError),
}

#[derive(Debug, Clone)]
pub struct AcceptedResponse {
    pub text: Arc<str>,
    pub segments: Arc<[ResponseSegment]>,
    pub effects: Arc<[AppliedEffect]>,
    pub state_after: CharacterState,
    pub event_id: EventId,
    pub context: ContextAnchor,
    pub audio: Option<AudioArtifact>,
    /// Follow-up reaction requested by an accepted match action.
    pub game_invite: Option<ReactionInvite>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AcceptanceContext {
    pub anchor: ContextAnchor,
    pub intent: TurnIntent,
    pub state_revision: u64,
    pub turn: TimelineTurn,
    pub game: Option<EventId>,
    pub game_guard: Option<EventId>,
}

enum Command {
    Context {
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
        reply: oneshot::Sender<Result<CharacterContext, CharacterError>>,
    },
    Accept {
        request: RequestId,
        context: Box<AcceptanceContext>,
        proposed: TurnProposal,
        audio: Option<AudioArtifact>,
        audience: ConversationAudience,
        reply: oneshot::Sender<Result<AcceptedResponse, CharacterError>>,
    },
    State(oneshot::Sender<Result<CharacterState, CharacterError>>),
    MemorySnapshot(oneshot::Sender<Result<CharacterMemorySnapshot, CharacterError>>),
    EditMemory {
        expected: CharacterMemorySnapshot,
        commands: Vec<MemoryCommand>,
        reply: oneshot::Sender<Result<MemoryEditResult, CharacterError>>,
    },
    View {
        at: ContextAnchor,
        reply: oneshot::Sender<Result<CharacterView, CharacterError>>,
    },
    Observe {
        input: UserInput,
        reply: oneshot::Sender<Result<EventId, CharacterError>>,
    },
    Game {
        command: UserGameCommand,
        reply: oneshot::Sender<Result<GameReply, CharacterError>>,
    },
    GameView {
        reply: oneshot::Sender<Result<GameReceipt, CharacterError>>,
    },
    GamePolicy(GamePolicy),
}

#[derive(Clone)]
pub struct CharacterHandle {
    commands: mpsc::Sender<Command>,
    memory_updates: watch::Receiver<u64>,
}

impl CharacterHandle {
    pub(crate) fn memory_updates(&self) -> watch::Receiver<u64> {
        self.memory_updates.clone()
    }

    pub async fn memories(&self) -> Result<CharacterMemorySnapshot, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::MemorySnapshot(reply))
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub async fn edit_memories(
        &self,
        expected: CharacterMemorySnapshot,
        commands: Vec<MemoryCommand>,
    ) -> Result<MemoryEditResult, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::EditMemory {
                expected,
                commands,
                reply,
            })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub async fn state(&self) -> Result<CharacterState, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::State(reply))
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub async fn get_context(&self, at: ContextAnchor) -> Result<CharacterView, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::View { at, reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }
    pub async fn observe(&self, input: UserInput) -> Result<EventId, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Observe { input, reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    /// Typed owner command for the character's own mini-game.
    pub async fn game(&self, command: UserGameCommand) -> Result<GameReply, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Game { command, reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    /// Read-only snapshot of the character's own match, if it still belongs
    /// to the selected timeline branch.
    pub async fn game_view(&self) -> Result<GameReceipt, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::GameView { reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub(crate) async fn configure_game_policy(
        &self,
        policy: GamePolicy,
    ) -> Result<(), CharacterError> {
        self.commands
            .send(Command::GamePolicy(policy))
            .await
            .map_err(|_| CharacterError::Unavailable)
    }
    pub(crate) async fn prepare(
        &self,
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
    ) -> Result<CharacterContext, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Context {
                request,
                guidance,
                intent,
                at,
                admission,
                reply,
            })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub(crate) async fn accept(
        &self,
        request: RequestId,
        context: AcceptanceContext,
        proposed: TurnProposal,
        audio: Option<AudioArtifact>,
        audience: ConversationAudience,
    ) -> Result<AcceptedResponse, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Accept {
                request,
                context: Box::new(context),
                proposed,
                audio,
                audience,
                reply,
            })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }
}

#[derive(Clone)]
pub(crate) struct CharacterRegistry {
    handles: HashMap<CharacterId, CharacterHandle>,
}

impl CharacterRegistry {
    #[cfg(test)]
    pub(crate) fn spawn(
        timeline: Arc<Mutex<SessionTimeline>>,
        stop: CancellationToken,
        capacity: usize,
    ) -> (Self, Vec<JoinHandle<()>>) {
        Self::spawn_inner(timeline, stop, capacity, Duration::from_secs(60), None)
    }

    pub(crate) fn spawn_with_reminders(
        timeline: Arc<Mutex<SessionTimeline>>,
        stop: CancellationToken,
        capacity: usize,
        reminders: ReminderBridge,
    ) -> (Self, Vec<JoinHandle<()>>) {
        Self::spawn_inner(
            timeline,
            stop,
            capacity,
            Duration::from_secs(60),
            Some(reminders),
        )
    }

    fn maintenance_pass(
        timeline: &mut SessionTimeline,
        now: u64,
    ) -> Result<usize, crate::timeline::domain::TimelineError> {
        let selected = timeline.selection();
        let mut total = 0;
        for character in CharacterId::ALL {
            total += timeline.expire_character_memories(selected, character, now)?;
        }
        Ok(total)
    }

    pub(crate) fn now_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_maintenance_interval(
        timeline: Arc<Mutex<SessionTimeline>>,
        stop: CancellationToken,
        capacity: usize,
        interval: Duration,
    ) -> (Self, Vec<JoinHandle<()>>) {
        Self::spawn_inner(timeline, stop, capacity, interval, None)
    }

    fn spawn_inner(
        timeline: Arc<Mutex<SessionTimeline>>,
        stop: CancellationToken,
        capacity: usize,
        interval: Duration,
        reminders: Option<ReminderBridge>,
    ) -> (Self, Vec<JoinHandle<()>>) {
        assert!(
            !interval.is_zero(),
            "memory maintenance interval must be positive"
        );
        {
            let mut locked = timeline.lock().expect("timeline lock poisoned");
            Self::maintenance_pass(&mut locked, Self::now_unix_ms())
                .expect("memory expiration maintenance must commit at startup");
        }
        let mut handles = HashMap::new();
        let mut tasks = Vec::new();
        for id in CharacterId::ALL {
            let (send, receive) = mpsc::channel(capacity);
            let mut character = Character::create(id);
            character.state = {
                let timeline = timeline.lock().expect("timeline lock poisoned");
                state_at(&timeline, id, timeline.anchor()).expect("owned timeline is readable")
            };
            let handler = CharacterHandler {
                character,
                timeline: Arc::clone(&timeline),
                policy: GamePolicy::default(),
                reminders: reminders.clone(),
            };
            let memory_updates = timeline
                .lock()
                .expect("timeline lock poisoned")
                .observe_memory_changes(id);
            handles.insert(
                id,
                CharacterHandle {
                    commands: send,
                    memory_updates,
                },
            );
            tasks.push(tokio::spawn(handler.run(receive, stop.clone())));
        }
        let maintenance_timeline = Arc::clone(&timeline);
        tasks.push(tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    _ = timer.tick() => {
                        let result = {
                            let mut timeline = maintenance_timeline.lock().expect("timeline lock poisoned");
                            Self::maintenance_pass(&mut timeline, Self::now_unix_ms())
                        };
                        if let Err(error) = result {
                            tracing::warn!(%error, "periodic memory expiration pass failed");
                        }
                    }
                }
            }
        }));
        (Self { handles }, tasks)
    }

    pub(crate) fn get(&self, id: CharacterId) -> Option<CharacterHandle> {
        self.handles.get(&id).cloned()
    }
}

struct CharacterHandler {
    character: Character,
    timeline: Arc<Mutex<SessionTimeline>>,
    policy: GamePolicy,
    reminders: Option<ReminderBridge>,
}

impl CharacterHandler {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>, stop: CancellationToken) {
        loop {
            let command = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                command = commands.recv() => match command { Some(command) => command, None => break },
            };
            match command {
                Command::Context {
                    request,
                    guidance,
                    intent,
                    at,
                    admission,
                    reply,
                } => {
                    let _ =
                        reply.send(self.context_admitted(request, guidance, intent, at, admission));
                }
                Command::Accept {
                    request,
                    context,
                    proposed,
                    audio,
                    audience,
                    reply,
                } => {
                    let _ = reply.send(self.accept(request, *context, proposed, audio, audience));
                }
                Command::MemorySnapshot(reply) => {
                    let result = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|timeline| {
                            read_memory_snapshot(&timeline, self.character.id)
                                .map_err(|error| CharacterError::Internal(error.to_string()))
                        });
                    let _ = reply.send(result);
                }
                Command::EditMemory {
                    expected,
                    commands,
                    reply,
                } => {
                    let result = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|mut timeline| {
                            edit_memory_snapshot(
                                &mut timeline,
                                self.character.id,
                                &expected,
                                &commands,
                            )
                            .map_err(|error| match error {
                                crate::timeline::domain::TimelineError::ConcurrentWrite => {
                                    CharacterError::StaleState
                                }
                                crate::timeline::domain::TimelineError::Invalid(reason) => {
                                    CharacterError::InvalidMemory(reason)
                                }
                                other => CharacterError::Internal(other.to_string()),
                            })
                        });
                    let _ = reply.send(result);
                }
                Command::State(reply) => {
                    let state = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|timeline| {
                            state_at(&timeline, self.character.id, timeline.anchor())
                        });
                    if let Ok(ref state) = state {
                        self.character.state = state.clone();
                    }
                    let _ = reply.send(state);
                }
                Command::View { at, reply } => {
                    let _ = reply.send(self.view(at));
                }
                Command::Observe { input, reply } => {
                    let result = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|mut timeline| {
                            timeline
                                .observe(
                                    self.character.id,
                                    input.event_id(),
                                    Arc::clone(input.text()),
                                )
                                .map_err(CharacterError::Internal)
                        });
                    let _ = reply.send(result);
                }
                Command::Game { command, reply } => {
                    let _ = reply.send(self.game_command(command));
                }
                Command::GameView { reply } => {
                    let _ = reply.send(self.game_view());
                }
                Command::GamePolicy(policy) => self.policy = policy,
            }
        }
    }

    #[cfg(test)]
    fn context(
        &mut self,
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
    ) -> Result<CharacterContext, CharacterError> {
        let admission = {
            let timeline = self.timeline.lock().expect("timeline mutex poisoned");
            timeline
                .admission(self.character.id, timeline.selection())
                .unwrap()
        };
        self.context_admitted(request, guidance, intent, at, admission)
    }

    fn context_admitted(
        &mut self,
        _request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
    ) -> Result<CharacterContext, CharacterError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        timeline
            .assert_admission(self.character.id, admission)
            .map_err(|_| CharacterError::StaleState)?;
        let captured =
            timeline
                .capture_context(self.character.id, at)
                .map_err(|error| match error {
                    ContextCaptureError::StaleKnowledge => CharacterError::StaleState,
                    ContextCaptureError::Invalid(reason) => CharacterError::Internal(reason),
                })?;
        let anchor = captured.history.at;
        let game_guard = if anchor == timeline.anchor() {
            captured.game_token
        } else {
            timeline
                .game_at(self.character.id, timeline.anchor())
                .map_err(CharacterError::Internal)?
                .1
        };
        let state = captured.state;
        let working_state = captured.working_state;
        let now = CharacterRegistry::now_unix_ms();
        let memories = captured.memories.list_at(now);
        let memory_islands = captured.memories.islands_at(now);
        let reminders = match &self.reminders {
            Some(_) => crate::reminders::projection::prompt_reminders_at(
                &timeline,
                anchor,
                self.character.id,
            )
            .map_err(|error| CharacterError::Internal(error.to_string()))?,
            None => Vec::new(),
        };
        let reaction = match intent {
            TurnIntent::Reply(event) | TurnIntent::React(event) => Some(
                captured
                    .known
                    .iter()
                    .find(|item| item.fact.draft.id == event)
                    .ok_or_else(|| {
                        CharacterError::Internal("cause not known at captured context".into())
                    })?
                    .fact
                    .clone(),
            ),
            TurnIntent::Initiate => None,
        }
        .map(|fact| ReactionCause {
            event_id: fact.draft.id,
            kind: fact.draft.kind,
            description: fact
                .draft
                .payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(Arc::from),
        });
        let turn = timeline
            .capture_turn(
                self.character.id,
                match intent {
                    TurnIntent::Reply(id) | TurnIntent::React(id) => Some(id),
                    TurnIntent::Initiate => None,
                },
                anchor,
            )
            .map_err(|error| CharacterError::Internal(error.to_string()))?;
        timeline
            .assert_turn(turn)
            .map_err(|_| CharacterError::StaleState)?;
        Ok(CharacterContext {
            character_id: self.character.id,
            description: Arc::clone(&self.character.description),
            prompt: Arc::clone(&self.character.prompt),
            runtime_context: None,
            state,
            working_state,
            memories,
            memory_islands,
            history: captured.history,
            view: captured.view,
            game: captured.game.projection(GameSide::Mita),
            reminders,
            current_input: guidance,
            focus: GenerationFocus { intent, reaction },
            anchor,
            turn,
            game_key: captured.game_token,
            game_guard,
        })
    }

    fn accept(
        &mut self,
        request: RequestId,
        context: AcceptanceContext,
        proposed: TurnProposal,
        audio: Option<AudioArtifact>,
        audience: ConversationAudience,
    ) -> Result<AcceptedResponse, CharacterError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        let current_at = timeline.anchor();
        let current = timeline
            .inspect_context_at(self.character.id, current_at)
            .map_err(CharacterError::Internal)?;
        self.character.state = current.state;
        if context.state_revision != self.character.state.revision {
            return Err(CharacterError::StaleState);
        }
        timeline
            .assert_turn(context.turn)
            .map_err(|_| CharacterError::StaleState)?;
        if context.game_guard != current.game_token {
            return Err(CharacterError::StaleState);
        }
        let branch = timeline.selection().branch_id;
        let mut next_game = current.game;
        let mut game_effects = Vec::new();
        let mut pending_invite = None;
        if let Some(actions) = proposed.effects().iter().find_map(|effect| match effect {
            ResponseEffect::Game(actions) => Some(actions.as_slice()),
            _ => None,
        }) {
            if !self.policy.enabled && actions.contains(&crate::games::ModelGameAction::Start) {
                return Err(CharacterError::InvalidGame(GameError::Disabled));
            }
            if context.game != current.game_token {
                return Err(CharacterError::StaleState);
            }
            let applied = next_game
                .apply_model(branch, actions, uuid::Uuid::new_v4().as_u128() as u64)
                .map_err(CharacterError::InvalidGame)?;
            for fact in applied.facts {
                game_effects.push(AppliedEffect::GameAdvanced(fact));
            }
            pending_invite = applied.invite;
        }
        if let TurnIntent::React(cause) = context.intent
            && self.reminders.is_some()
        {
            let selection = timeline.selection();
            let facts = timeline
                .reminder_facts(selection)
                .map_err(|error| CharacterError::Internal(error.to_string()))?;
            let projection =
                ReminderProjection::from_facts(selection.branch_id, self.character.id, &facts)
                    .map_err(|error| CharacterError::Internal(error.to_string()))?;
            if let Some(delivery) = projection.delivery_by_event(cause)
                && !matches!(
                    delivery.state,
                    crate::reminders::DeliveryState::Pending
                        | crate::reminders::DeliveryState::Retrying
                )
            {
                return Err(CharacterError::StaleState);
            }
        }
        let current_behavior = CharacterBehavior {
            attitude: self.character.state.attitude,
            boredom: self.character.state.boredom,
            stress: self.character.state.stress,
        };
        let mut effects = Vec::with_capacity(proposed.effects().len());
        let current_memories = current.memories;
        for effect in proposed.effects().iter() {
            match effect {
                ResponseEffect::ChangeCharacterState(delta) => {
                    let state = current_behavior.apply(*delta).ok_or_else(|| {
                        CharacterError::Internal("invalid character state change".into())
                    })?;
                    effects.push(AppliedEffect::CharacterStateChanged(state));
                }
                ResponseEffect::ReplaceWorkingState(state) => {
                    effects.push(AppliedEffect::CharacterWorkingStateReplaced(state.clone()));
                }
                ResponseEffect::MutateMemory(commands) => {
                    let changes = current_memories
                        .resolve(commands)
                        .map_err(|reason| CharacterError::Internal(reason.into()))?;
                    effects.extend(
                        changes
                            .into_iter()
                            .map(AppliedEffect::CharacterMemoryChanged),
                    );
                }
                ResponseEffect::Game(_) => {}
                ResponseEffect::MutateReminders(requests) => {
                    let Some(bridge) = &self.reminders else {
                        return Err(CharacterError::Internal(
                            "reminder settings are not wired".into(),
                        ));
                    };
                    let settings = bridge
                        .settings
                        .snapshot::<RemindersSettings>()
                        .map_err(|error| CharacterError::Internal(error.to_string()))?
                        .value;
                    let now = bridge.clock.now();
                    if !settings.enabled {
                        return Err(CharacterError::Internal(
                            "reminders are disabled in settings".into(),
                        ));
                    }
                    let selection = timeline.selection();
                    let facts = timeline
                        .reminder_facts(selection)
                        .map_err(|error| CharacterError::Internal(error.to_string()))?;
                    let mut projection = ReminderProjection::from_facts(
                        selection.branch_id,
                        self.character.id,
                        &facts,
                    )
                    .map_err(|error| CharacterError::Internal(error.to_string()))?;
                    let captured_anchor = ContextAnchor {
                        branch_id: context.turn.selection.branch_id.0,
                        through: context.turn.context_head,
                    };
                    let captured_facts = timeline
                        .reminder_facts_at(captured_anchor)
                        .map_err(|error| CharacterError::Internal(error.to_string()))?;
                    let captured = ReminderProjection::from_facts(
                        context.turn.selection.branch_id,
                        self.character.id,
                        &captured_facts,
                    )
                    .map_err(|error| CharacterError::Internal(error.to_string()))?;
                    if captured.reminder_state() != projection.reminder_state() {
                        return Err(CharacterError::StaleState);
                    }
                    let (changes, diagnostic) = resolve_requests(
                        &mut projection,
                        self.character.id,
                        requests,
                        now,
                        &settings.timezone,
                        settings.policy,
                    )
                    .map_err(|error| {
                        CharacterError::Internal(format!("invalid reminder effects: {error}"))
                    })?;
                    effects.extend(changes.into_iter().map(AppliedEffect::ReminderChanged));
                    if let Some(text) = diagnostic {
                        effects.push(AppliedEffect::ReminderToolResult(text));
                    }
                }
            }
        }
        let has_game_effects = proposed
            .effects()
            .iter()
            .any(|effect| matches!(effect, ResponseEffect::Game(_)));
        effects.extend(game_effects);
        let persisted_segments = proposed
            .segments()
            .iter()
            .map(|segment| segment.sanitized_for_persistence())
            .collect::<Vec<_>>();
        let accepted = timeline
            .append_reply_tracked(
                GeneratedReply {
                    request,
                    intent: context.intent,
                    turn: context.turn,
                    expected_revision: context.state_revision,
                    content: serde_json::to_value(&persisted_segments).map_err(|error| {
                        CharacterError::Internal(format!(
                            "invalid accepted message segments: {error}"
                        ))
                    })?,
                    audience,
                    effects: effects.clone(),
                },
                has_game_effects.then(|| crate::timeline::GameCommit {
                    state: next_game.clone(),
                    expected_head: current_at.through,
                }),
            )
            .map_err(|error| match error {
                crate::timeline::domain::TimelineError::ConcurrentWrite => {
                    CharacterError::StaleState
                }
                other => CharacterError::Internal(other.to_string()),
            })?;
        for event in &accepted.game {
            next_game.record_event(*event);
        }
        let game_invite = pending_invite.and_then(|pending| {
            Self::resolve_invite(pending, &accepted.game, next_game.last_event())
        });
        self.character.state = state_at(&timeline, self.character.id, timeline.anchor())?;
        if proposed
            .effects()
            .iter()
            .any(|effect| matches!(effect, ResponseEffect::MutateReminders(_)))
            && let Some(bridge) = &self.reminders
        {
            bridge.wake.notify_one();
        }
        Ok(AcceptedResponse {
            state_after: self.character.state.clone(),
            text: Arc::clone(proposed.text()),
            segments: Arc::from(proposed.segments()),
            effects: effects.into(),
            event_id: accepted.message,
            context: context.anchor,
            audio,
            game_invite,
        })
    }

    fn resolve_invite(
        pending: PendingInvite,
        events: &[EventId],
        last_event: Option<EventId>,
    ) -> Option<ReactionInvite> {
        let event_id = match pending.cause {
            InviteCause::Fact(index) => events.get(index).copied(),
            InviteCause::LastEvent => last_event,
        }?;
        Some(ReactionInvite {
            event_id,
            instruction: pending.instruction,
            auto_gated: pending.auto_gated,
        })
    }

    fn game_command(&mut self, command: UserGameCommand) -> Result<GameReply, CharacterError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        let branch = timeline.selection().branch_id;
        if !self.policy.enabled && matches!(command, UserGameCommand::Start { .. }) {
            return Err(CharacterError::InvalidGame(GameError::Disabled));
        }
        let at = timeline.anchor();
        let (mut next, _) = timeline
            .game_at(self.character.id, at)
            .map_err(CharacterError::Internal)?;
        let applied = next
            .apply_user(branch, command)
            .map_err(CharacterError::InvalidGame)?;
        let events = timeline
            .append_game_facts(self.character.id, &applied.facts, &mut next, at.through)
            .map_err(|error| CharacterError::Internal(error.to_string()))?;
        for event in &events {
            next.record_event(*event);
        }
        let invite = applied
            .invite
            .and_then(|pending| Self::resolve_invite(pending, &events, next.last_event()));
        let receipt = Self::game_receipt(&next, next.projection(GameSide::Player), events);
        Ok(GameReply { receipt, invite })
    }

    fn game_view(&mut self) -> Result<GameReceipt, CharacterError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        let (game, _) = timeline
            .game_at(self.character.id, timeline.anchor())
            .map_err(CharacterError::Internal)?;
        Ok(Self::game_receipt(
            &game,
            game.projection(GameSide::Player),
            Vec::new(),
        ))
    }

    fn game_receipt(
        slot: &GameSlot,
        view: Option<crate::games::GameProjection>,
        events: Vec<EventId>,
    ) -> GameReceipt {
        GameReceipt {
            game: crate::games::GameId::SeaBattle,
            key: slot.key(),
            view,
            events,
            auto_reactions: slot.auto_reactions(),
        }
    }
    fn view(&self, at: ContextAnchor) -> Result<CharacterView, CharacterError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        let captured = timeline
            .inspect_context_at(self.character.id, at)
            .map_err(CharacterError::Internal)?;
        Ok(CharacterView {
            character_id: self.character.id,
            description: Arc::clone(&self.character.description),
            prompt: Arc::clone(&self.character.prompt),
            state: captured.state,
            working_state: captured.working_state,
            game: captured.game.projection(GameSide::Mita),
            memories: captured.memories.list(),
            memory_islands: captured.memories.islands(),
            history: captured.history,
            view: captured.view,
        })
    }
}

fn state_at(
    timeline: &SessionTimeline,
    character: CharacterId,
    at: ContextAnchor,
) -> Result<CharacterState, CharacterError> {
    timeline
        .character_state_at(character, at)
        .map_err(|error| CharacterError::Internal(error.to_string()))
}

#[cfg(test)]
mod turn_tests {
    use super::*;
    use crate::generation::ProposedResponse;
    use crate::timeline::domain::{BranchId, EventDraft, EventKind};

    fn handler() -> CharacterHandler {
        CharacterHandler {
            character: Character::create(CharacterId::Crazy),
            timeline: Arc::new(Mutex::new(SessionTimeline::new())),
            policy: GamePolicy::default(),
            reminders: None,
        }
    }

    fn game_proposal() -> TurnProposal {
        let mut response = crate::generation::StructuredResponse::spoken("placed");
        response.attitude_change = 5.0;
        response.segments[0].commands = vec!["PlaceShip,A1,4,H".into()];
        TurnProposal::from_structured(response).unwrap()
    }

    #[test]
    fn stale_game_context_rejects_a_reply_without_game_commands() {
        let mut handler = handler();
        handler
            .game_command(UserGameCommand::Start {
                auto_reactions: false,
            })
            .unwrap();
        let (request, context) = prepare(&mut handler);
        handler
            .game_command(UserGameCommand::SetAutoReactions(true))
            .unwrap();
        let before = handler.timeline.lock().unwrap().anchor();
        let mut response = crate::generation::StructuredResponse::spoken("stale game commentary");
        response.attitude_change = 5.0;
        let result = handler.accept(
            request,
            acceptance(&context),
            TurnProposal::from_structured(response).unwrap(),
            None,
            ConversationAudience::private_to(CharacterId::Crazy),
        );
        assert!(matches!(result, Err(CharacterError::StaleState)));
        assert_eq!(handler.timeline.lock().unwrap().anchor(), before);
    }

    #[test]
    fn game_token_rejects_flag_change_and_epoch_rejects_game_proposal_atomically() {
        let mut handler = handler();
        handler
            .game_command(UserGameCommand::Start {
                auto_reactions: false,
            })
            .unwrap();
        let (request, context) = prepare(&mut handler);
        handler
            .game_command(UserGameCommand::SetAutoReactions(true))
            .unwrap();
        let before = handler.timeline.lock().unwrap().anchor();
        let result = handler.accept(
            request,
            acceptance(&context),
            game_proposal(),
            None,
            ConversationAudience::private_to(CharacterId::Crazy),
        );
        assert!(matches!(result, Err(CharacterError::StaleState)));
        assert_eq!(handler.timeline.lock().unwrap().anchor(), before);
        let (request, context) = prepare(&mut handler);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let selection = timeline.selection();
            timeline
                .reset_character_context(selection, CharacterId::Crazy, EventId::new())
                .unwrap();
        }
        let before = handler.timeline.lock().unwrap().anchor();
        let result = handler.accept(
            request,
            acceptance(&context),
            game_proposal(),
            None,
            ConversationAudience::private_to(CharacterId::Crazy),
        );
        assert!(matches!(result, Err(CharacterError::StaleState)));
        assert_eq!(handler.timeline.lock().unwrap().anchor(), before);
    }

    #[test]
    fn private_game_commit_rejects_a_head_changed_after_preparation() {
        let mut handler = handler();
        handler
            .game_command(UserGameCommand::Start {
                auto_reactions: false,
            })
            .unwrap();
        let mut timeline = handler.timeline.lock().unwrap();
        let at = timeline.anchor();
        let (mut next, _) = timeline.game_at(CharacterId::Crazy, at).unwrap();
        let applied = next
            .apply_user(
                timeline.selection().branch_id,
                UserGameCommand::SetAutoReactions(true),
            )
            .unwrap();
        timeline
            .publish_player_message(
                CharacterId::Crazy,
                EventId::new(),
                Arc::from("concurrent write"),
            )
            .unwrap();
        let before = timeline.anchor();
        assert!(matches!(
            timeline.append_game_facts(CharacterId::Crazy, &applied.facts, &mut next, at.through),
            Err(crate::timeline::domain::TimelineError::ConcurrentWrite)
        ));
        assert_eq!(timeline.anchor(), before);
        assert!(
            !timeline
                .game_at(CharacterId::Crazy, before)
                .unwrap()
                .0
                .auto_reactions()
        );
    }

    #[test]
    fn private_game_snapshot_boundary_rejects_leaks_bad_replay_and_unknown_fields() {
        use crate::timeline::domain::{SessionId, TimelineStore};
        use serde_json::json;
        let mut handler = handler();
        handler
            .game_command(UserGameCommand::Start {
                auto_reactions: false,
            })
            .unwrap();
        handler
            .game_command(UserGameCommand::PlaceShip {
                coord: crate::games::seabattle::Coordinate::parse("A1").unwrap(),
                length: 4,
                orientation: crate::games::seabattle::Orientation::Horizontal,
            })
            .unwrap();
        let timeline = handler.timeline.lock().unwrap();
        let (game, _) = timeline
            .game_at(CharacterId::Crazy, timeline.anchor())
            .unwrap();
        let canonical = serde_json::to_value(game).unwrap();
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();
        for mode in 0..6 {
            let mut draft = EventDraft::new(EventKind::GameStateCaptured);
            draft.actor = Some("mita".into());
            draft.observers.insert(CharacterId::Crazy);
            let mut state = canonical.clone();
            match mode {
                0 => {
                    draft.observers.insert(CharacterId::Kind);
                }
                1 => {
                    state["unknown"] = json!(true);
                }
                2 => {
                    state["current"]["game"]["actions"][0]["start"] = json!("K11");
                }
                3 => {
                    state["current"]["game"]["actions"][0]["length"] = json!(5);
                }
                4 => {
                    state["current"]["generation"] = json!(123);
                }
                _ => {
                    draft.actor = Some("kind-mita".into());
                }
            }
            draft.payload.insert("game_state".into(), state);
            assert!(
                store
                    .append(branch, 0, &format!("bad:{mode}"), json!({}), vec![draft])
                    .is_err(),
                "mode {mode}"
            );
            assert!(store.world(branch).unwrap().is_empty());
        }
    }

    fn prepare(handler: &mut CharacterHandler) -> (RequestId, CharacterContext) {
        let request = RequestId::new();
        let event = EventId::new();
        handler
            .timeline
            .lock()
            .unwrap()
            .publish_player_message(CharacterId::Crazy, event, Arc::from("hello"))
            .unwrap();
        let context = handler
            .context(
                request,
                Arc::from(""),
                TurnIntent::Reply(event),
                ContextPosition::Latest,
            )
            .unwrap();
        (request, context)
    }

    fn acceptance(context: &CharacterContext) -> AcceptanceContext {
        AcceptanceContext {
            anchor: context.anchor,
            intent: context.focus.intent,
            state_revision: context.state.revision,
            turn: context.turn,
            game: context.game_key,
            game_guard: context.game_guard,
        }
    }

    fn accept(
        handler: &mut CharacterHandler,
        request: RequestId,
        context: AcceptanceContext,
    ) -> Result<AcceptedResponse, CharacterError> {
        handler.accept(
            request,
            context,
            ProposedResponse::from_structured(crate::generation::StructuredResponse::spoken(
                "answer",
            ))
            .unwrap(),
            None,
            ConversationAudience::private_to(CharacterId::Crazy),
        )
    }

    fn accept_with_state_change(
        handler: &mut CharacterHandler,
        request: RequestId,
        context: AcceptanceContext,
        attitude: f64,
        boredom: f64,
        stress: f64,
    ) -> Result<AcceptedResponse, CharacterError> {
        let mut structured = crate::generation::StructuredResponse::spoken("stateful reply");
        structured.attitude_change = attitude;
        structured.boredom_change = boredom;
        structured.stress_change = stress;
        handler.accept(
            request,
            context,
            ProposedResponse::from_structured(structured).unwrap(),
            None,
            ConversationAudience::private_to(CharacterId::Crazy),
        )
    }

    #[test]
    fn state_change_is_atomic_with_reply_and_survives_context_reset() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let before = context.anchor;
        let response = accept_with_state_change(
            &mut handler,
            request,
            acceptance(&context),
            80.0,
            -80.0,
            1.235,
        )
        .unwrap();
        let after = handler.timeline.lock().unwrap().anchor();
        assert_eq!(after.through, before.through + 2);
        let state = &handler.character.state;
        assert_eq!(state.accepted_replies, 1);
        assert_eq!(state.attitude, 66.0);
        assert_eq!(state.boredom, 4.0);
        assert_eq!(state.stress, 6.24);
        assert_eq!(
            response.effects.as_ref(),
            &[AppliedEffect::CharacterStateChanged(CharacterBehavior {
                attitude: 66.0,
                boredom: 4.0,
                stress: 6.24,
            })]
        );
        let timeline = handler.timeline.lock().unwrap();
        assert_ne!(response.event_id, EventId::default());
        assert_eq!(
            state_at(&timeline, CharacterId::Crazy, before)
                .unwrap()
                .attitude,
            60.0
        );
        assert_eq!(
            state_at(&timeline, CharacterId::Kind, after)
                .unwrap()
                .attitude,
            60.0
        );
        drop(timeline);
        let reset = EventId::new();
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let expected = timeline.selection();
            timeline
                .reset_character_context(expected, CharacterId::Crazy, reset)
                .unwrap();
            let now = timeline.anchor();
            assert_eq!(
                state_at(&timeline, CharacterId::Crazy, now)
                    .unwrap()
                    .attitude,
                66.0
            );
            assert_eq!(
                state_at(&timeline, CharacterId::Crazy, now)
                    .unwrap()
                    .accepted_replies,
                1
            );
        }
    }

    #[test]
    fn rejected_stale_response_does_not_change_state_or_append_side_effects() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Crazy);
            timeline
                .append_test_fact("cancel-before-accept", reset)
                .unwrap();
        }
        let before = handler.timeline.lock().unwrap().anchor().through;
        assert!(matches!(
            accept_with_state_change(&mut handler, request, token, 2.0, 2.0, 2.0),
            Err(CharacterError::StaleState)
        ));
        assert_eq!(handler.timeline.lock().unwrap().anchor().through, before);
        assert_eq!(handler.character.state.attitude, 60.0);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn sqlite_state_change_reopens_with_same_values_and_one_commit() {
        use crate::timeline::{TimelineConfig, domain::SessionId};
        let path =
            std::env::temp_dir().join(format!("neuromita-effects-{}.db", uuid::Uuid::new_v4()));
        let root = BranchId::new();
        let session = SessionId::new();
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let mut handler = CharacterHandler {
                character: Character::create(CharacterId::Crazy),
                timeline: Arc::new(Mutex::new(timeline)),
                policy: GamePolicy::default(),
                reminders: None,
            };
            let (request, context) = prepare(&mut handler);
            accept_with_state_change(&mut handler, request, acceptance(&context), 3.2, -1.0, 2.0)
                .unwrap();
            assert_eq!(handler.timeline.lock().unwrap().anchor().through, 3);
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let at = timeline.anchor();
            assert_eq!(at.through, 3);
            let state = state_at(&timeline, CharacterId::Crazy, at).unwrap();
            assert_eq!(state.accepted_replies, 1);
            assert_eq!(state.attitude, 63.2);
            assert_eq!(state.boredom, 9.0);
            assert_eq!(state.stress, 7.0);
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn reselect_same_branch_invalidates_pending_acceptance() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let current = timeline.selection();
            timeline
                .select_branch(BranchId(current.branch_id.0), current)
                .unwrap();
        }
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
        assert_eq!(handler.character.state.accepted_replies, 0);
        assert_eq!(handler.timeline.lock().unwrap().events().len(), 1);
    }

    #[test]
    fn reset_invalidates_pending_turn_but_later_unrelated_fact_does_not() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .append_test_fact("unrelated", EventDraft::new(EventKind::UnityEvent))
                .unwrap();
        }
        assert!(accept(&mut handler, request, token).is_ok());
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Crazy);
            timeline.append_test_fact("reset", reset).unwrap();
        }
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
    }

    #[test]
    fn accepted_state_survives_reset_and_hidden_history() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let reply = accept(&mut handler, request, acceptance(&context)).unwrap();
        let before_reset = handler.timeline.lock().unwrap().anchor();
        assert_eq!(
            state_at(
                &handler.timeline.lock().unwrap(),
                CharacterId::Crazy,
                before_reset
            )
            .unwrap()
            .accepted_replies,
            1
        );
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let mut hidden = EventDraft::new(EventKind::MessageHidden);
            hidden.observers.insert(CharacterId::Crazy);
            hidden.payload.insert(
                "event_ids".into(),
                serde_json::json!([reply.event_id.0.to_string()]),
            );
            timeline.append_test_fact("hide-accepted", hidden).unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Crazy);
            timeline
                .append_test_fact("reset-after-accepted", reset)
                .unwrap();
            let now = timeline.anchor();
            assert_eq!(
                timeline
                    .accepted_replies_at(CharacterId::Crazy, now)
                    .unwrap(),
                1
            );
            assert_eq!(
                timeline
                    .accepted_replies_at(CharacterId::Kind, now)
                    .unwrap(),
                0
            );
        }
        let (next_request, next_context) = prepare(&mut handler);
        assert_eq!(next_context.state.accepted_replies, 1);
        assert_eq!(next_context.state.revision, 1);
        assert_eq!(next_context.view.messages().len(), 1);
        assert_eq!(next_context.view.messages()[0].content, "hello");
        accept(&mut handler, next_request, acceptance(&next_context)).unwrap();
        assert_eq!(handler.character.state.accepted_replies, 2);
        assert_eq!(
            handler.view(before_reset).unwrap().state.accepted_replies,
            1
        );
    }

    #[test]
    fn reactive_context_includes_later_known_dialogue_and_world_knowledge() {
        use crate::timeline::domain::MessageRole;
        let mut handler = handler();
        let cause = EventId::new();
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .observe(CharacterId::Crazy, cause, Arc::from("door opened"))
                .unwrap();
            timeline
                .publish_player_message(
                    CharacterId::Crazy,
                    EventId::new(),
                    Arc::from("later, unrelated user input"),
                )
                .unwrap();
            let mut note = EventDraft::new(EventKind::SessionNote);
            note.observers.insert(CharacterId::Crazy);
            note.payload
                .insert("content".into(), serde_json::json!("weather became rainy"));
            timeline.append_test_fact("world-note", note).unwrap();
        }
        let request = RequestId::new();
        let context = handler
            .context(
                request,
                Arc::from("react to the door"),
                TurnIntent::React(cause),
                ContextPosition::Latest,
            )
            .unwrap();
        assert_eq!(context.anchor.through, 3);
        assert_eq!(context.view.count(), 3);
        let messages = context.view.messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "door opened");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "later, unrelated user input");
        assert_eq!(messages[2].role, MessageRole::System);
        assert_eq!(messages[2].content, "weather became rainy");
        assert_eq!(context.current_input.as_ref(), "react to the door");

        let earlier = handler
            .timeline
            .lock()
            .unwrap()
            .capture_context(
                CharacterId::Crazy,
                ContextPosition::At(ContextAnchor {
                    branch_id: context.anchor.branch_id,
                    through: 1,
                }),
            )
            .unwrap();
        assert_eq!(earlier.view.messages().len(), 1);
    }

    #[test]
    fn reacting_to_cause_not_known_at_snapshot_has_no_side_effect() {
        let mut handler = handler();
        let cause = EventId::new();
        let old = handler.timeline.lock().unwrap().anchor();
        handler
            .timeline
            .lock()
            .unwrap()
            .observe(CharacterId::Crazy, cause, Arc::from("door opened"))
            .unwrap();
        let result = handler.context(
            RequestId::new(),
            Arc::from("react"),
            TurnIntent::React(cause),
            ContextPosition::At(old),
        );
        assert!(matches!(result, Err(CharacterError::Internal(_))));
        assert_eq!(handler.timeline.lock().unwrap().anchor().through, 1);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn persisted_accepted_state_survives_reopen_and_reset() {
        use crate::timeline::{TimelineConfig, domain::SessionId};
        let path =
            std::env::temp_dir().join(format!("neuromita-state-{}.db", uuid::Uuid::new_v4()));
        let root = BranchId::new();
        let session = SessionId::new();
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let mut handler = CharacterHandler {
                character: Character::create(CharacterId::Crazy),
                timeline: Arc::new(Mutex::new(timeline)),
                policy: GamePolicy::default(),
                reminders: None,
            };
            let (request, context) = prepare(&mut handler);
            accept(&mut handler, request, acceptance(&context)).unwrap();
            let mut timeline = handler.timeline.lock().unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Crazy);
            timeline.append_test_fact("persisted-reset", reset).unwrap();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let mut handler = CharacterHandler {
                character: Character::create(CharacterId::Crazy),
                timeline: Arc::new(Mutex::new(timeline)),
                policy: GamePolicy::default(),
                reminders: None,
            };
            let (request, context) = prepare(&mut handler);
            assert_eq!(context.state.accepted_replies, 1);
            assert_eq!(context.view.count(), 1);
            accept(&mut handler, request, acceptance(&context)).unwrap();
            assert_eq!(handler.character.state.accepted_replies, 2);
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn prepared_context_uses_single_prefix_and_does_not_repeat_new_input() {
        let mut handler = handler();
        let original = EventId::new();
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .observe(CharacterId::Crazy, original, Arc::from("seen by Mita"))
                .unwrap();
        }
        let input_id = EventId::new();
        handler
            .timeline
            .lock()
            .unwrap()
            .publish_player_message(CharacterId::Crazy, input_id, Arc::from("new request"))
            .unwrap();
        let snapshot = handler
            .context(
                RequestId::new(),
                Arc::from(""),
                TurnIntent::Reply(input_id),
                ContextPosition::Latest,
            )
            .unwrap();
        assert_eq!(snapshot.anchor.through, 2);
        assert_eq!(snapshot.view.through, snapshot.anchor.through);
        assert_eq!(snapshot.history.at, snapshot.anchor);
        assert_eq!(snapshot.view.count(), 2);
        assert!(snapshot.view.contains(&original.0.to_string()));
        assert!(snapshot.view.contains(&input_id.0.to_string()));
        assert!(snapshot.current_input.is_empty());
        assert_eq!(handler.timeline.lock().unwrap().anchor().through, 2);

        let old = handler.view(snapshot.anchor).unwrap();
        assert_eq!(old.view.count(), 2);
        assert_eq!(old.view.through, snapshot.anchor.through);
        assert_eq!(old.history.at, snapshot.anchor);
        let first = handler.timeline.lock().unwrap().anchor();
        let empty = handler
            .view(ContextAnchor {
                through: 0,
                ..first
            })
            .unwrap();
        assert_eq!(empty.view.count(), 0);
        assert_eq!(empty.state.accepted_replies, 0);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn concurrent_sqlite_selection_change_invalidates_pending_acceptance() {
        use crate::timeline::selection::TimelineSelectionRepository;
        use crate::timeline::{
            TimelineConfig, domain::SessionId, storage::SqliteTimelineRepository,
        };
        let path =
            std::env::temp_dir().join(format!("neuromita-turn-pin-{}.db", uuid::Uuid::new_v4()));
        let root = BranchId::new();
        let session = SessionId::new();
        let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        let mut handler = CharacterHandler {
            character: Character::create(CharacterId::Crazy),
            timeline: Arc::new(Mutex::new(timeline)),
            policy: GamePolicy::default(),
            reminders: None,
        };
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        let mut other = SqliteTimelineRepository::open(&path).unwrap();
        let previous = other.selection().unwrap().unwrap();
        other.select_branch(root, Some(previous)).unwrap();
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
        drop(other);
        drop(handler);
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }
}
