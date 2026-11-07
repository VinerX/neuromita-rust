use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    sync::{Notify, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use super::{
    clock::Clock,
    model::{
        CreateReminder, CreateTimer, DeliveryFailure, DeliveryId, DeliveryOutcome,
        DeliverySettlement, DeliveryState, DueSpec, FiredOccurrence, NextOccurrence,
        ReminderChange, ReminderConfig, ReminderError, ReminderId, ReminderKind, ReminderRecord,
        ReminderRequest, ReminderToolCommand, ReminderToolReply, ReminderView, RemindersSnapshot,
        RepeatRule, UpdateReminder,
    },
    projection::ReminderProjection,
    time::{LocalTimePolicy, TimeZoneId, UtcInstant},
    tool::{parse_friendly_due, parse_structured_due},
};
use crate::{
    character::CharacterId,
    identity::EventId,
    request::{
        CancelReason, ChatApiState, ReactRequest, RequestHandle, RequestOutcome, RequestSnapshot,
        RequestSource, RequestState, SubmitError,
    },
    settings::{RemindersSettings, SettingsError, SettingsOwner, SettingsSnapshot},
    timeline::{
        SessionTimeline,
        domain::{BranchId, EventDraft, TimelineError, TimelineFact},
    },
};

/// Static handle published through `ApiBindings`; the owning Core keeps the
/// single scheduler task alive for the lifetime of the application.
pub struct RemindersHandle {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<Arc<RemindersSnapshot>>,
}

impl RemindersHandle {
    pub fn snapshot(&self) -> Arc<RemindersSnapshot> {
        Arc::clone(&self.snapshot.borrow())
    }
    pub fn observe(&self) -> watch::Receiver<Arc<RemindersSnapshot>> {
        self.snapshot.clone()
    }
    async fn call<T>(
        &self,
        command: Command,
        reply: oneshot::Receiver<T>,
    ) -> Result<T, ReminderError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| ReminderError::Stopped)?;
        reply.await.map_err(|_| ReminderError::Stopped)
    }
    pub async fn create(&self, input: CreateReminder) -> Result<ReminderView, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::Create(input, reply), receive).await?
    }
    pub async fn create_timer(&self, input: CreateTimer) -> Result<ReminderView, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::CreateTimer(input, reply), receive)
            .await?
    }
    pub async fn update(&self, input: UpdateReminder) -> Result<ReminderView, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::Update(input, reply), receive).await?
    }
    pub async fn delete(
        &self,
        character: CharacterId,
        number: u64,
    ) -> Result<ReminderView, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(
            Command::Delete {
                character,
                number,
                reply,
            },
            receive,
        )
        .await?
    }
    pub async fn list(&self, character: CharacterId) -> Result<Vec<ReminderView>, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::List { character, reply }, receive)
            .await?
    }
    pub async fn set_enabled(&self, enabled: bool) -> Result<(), ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::SetEnabled { enabled, reply }, receive)
            .await?
    }
    pub async fn set_timezone(&self, timezone: TimeZoneId) -> Result<(), ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::SetTimezone { timezone, reply }, receive)
            .await?
    }
    pub async fn retry(&self, delivery: DeliveryId) -> Result<(), ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::Retry { delivery, reply }, receive)
            .await?
    }
    pub async fn now(&self) -> Result<UtcInstant, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::Now(reply), receive).await
    }
    pub async fn tick(&self) -> Result<usize, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(Command::Tick(reply), receive).await?
    }
    pub async fn run_tool(
        &self,
        character: CharacterId,
        command: ReminderToolCommand,
    ) -> Result<ReminderToolReply, ReminderError> {
        let (reply, receive) = oneshot::channel();
        self.call(
            Command::Tool {
                character,
                command,
                reply,
            },
            receive,
        )
        .await?
    }
}

enum Command {
    Create(
        CreateReminder,
        oneshot::Sender<Result<ReminderView, ReminderError>>,
    ),
    CreateTimer(
        CreateTimer,
        oneshot::Sender<Result<ReminderView, ReminderError>>,
    ),
    Update(
        UpdateReminder,
        oneshot::Sender<Result<ReminderView, ReminderError>>,
    ),
    Delete {
        character: CharacterId,
        number: u64,
        reply: oneshot::Sender<Result<ReminderView, ReminderError>>,
    },
    List {
        character: CharacterId,
        reply: oneshot::Sender<Result<Vec<ReminderView>, ReminderError>>,
    },
    SetEnabled {
        enabled: bool,
        reply: oneshot::Sender<Result<(), ReminderError>>,
    },
    SetTimezone {
        timezone: TimeZoneId,
        reply: oneshot::Sender<Result<(), ReminderError>>,
    },
    Retry {
        delivery: DeliveryId,
        reply: oneshot::Sender<Result<(), ReminderError>>,
    },
    Now(oneshot::Sender<UtcInstant>),
    Tick(oneshot::Sender<Result<usize, ReminderError>>),
    Tool {
        character: CharacterId,
        command: ReminderToolCommand,
        reply: oneshot::Sender<Result<ReminderToolReply, ReminderError>>,
    },
}

struct DeliveryEvent {
    branch: BranchId,
    fired: EventId,
    generation: u32,
    outcome: TerminalOutcome,
}

enum TerminalOutcome {
    Delivered,
    Blocked(String),
    Cancelled,
    Interrupted(CancelReason),
}

#[derive(Clone, Copy)]
enum TransitionExpect {
    InFlight,
    Blocked,
}

fn find_delivery(
    branch: BranchId,
    facts: &[TimelineFact],
    fired: EventId,
) -> Result<(CharacterId, ReminderProjection), ReminderError> {
    for &character in &CharacterId::ALL {
        let projection = ReminderProjection::from_facts(branch, character, facts)?;
        if projection.delivery_by_event(fired).is_some() {
            return Ok((character, projection));
        }
    }
    Err(ReminderError::DeliveryNotFound(DeliveryId(fired)))
}

impl TerminalOutcome {
    fn of(snapshot: &RequestSnapshot) -> Self {
        match &snapshot.state {
            RequestState::Finished(
                RequestOutcome::Success(_) | RequestOutcome::VoiceGenerationFailed { .. },
            ) => Self::Delivered,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
                Self::Blocked(format!("text generation failed: {error}"))
            }
            RequestState::Finished(RequestOutcome::InternalError(error)) => {
                Self::Blocked(format!("generation failed: {error}"))
            }
            RequestState::Cancelled => match snapshot.cancel_reason {
                Some(CancelReason::User) => Self::Cancelled,
                Some(reason) => Self::Interrupted(reason),
                None => Self::Interrupted(CancelReason::Shutdown),
            },
            RequestState::Pending | RequestState::Processing(_) => {
                Self::Interrupted(CancelReason::Shutdown)
            }
        }
    }
}

pub(crate) struct ReminderService {
    timeline: Arc<Mutex<SessionTimeline>>,
    chat: Arc<ChatApiState>,
    settings: Arc<SettingsOwner>,
    config: ReminderConfig,
    wake: Arc<Notify>,
    settings_updates: watch::Receiver<Arc<SettingsSnapshot>>,
    in_flight: HashMap<EventId, RequestHandle>,
    superseded_in_flight: HashSet<EventId>,
    monitors: JoinSet<()>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Receiver<DeliveryEvent>,
    events_tx: mpsc::Sender<DeliveryEvent>,
    snapshots: watch::Sender<Arc<RemindersSnapshot>>,
}

impl ReminderService {
    pub(crate) fn start(
        timeline: Arc<Mutex<SessionTimeline>>,
        chat: Arc<ChatApiState>,
        settings: Arc<SettingsOwner>,
        config: ReminderConfig,
        wake: Arc<Notify>,
        stop: CancellationToken,
    ) -> (Arc<RemindersHandle>, tokio::task::JoinHandle<()>) {
        let (commands, command_rx) = mpsc::channel(64);
        let (events_tx, events) = mpsc::channel(256);
        let (snapshots, snapshot_rx) = watch::channel(Arc::new(RemindersSnapshot {
            closed: false,
            branch_id: BranchId::new(),
            enabled: true,
            timezone: TimeZoneId::utc(),
            next_number: 1,
            reminders: Vec::new(),
            deliveries: Vec::new(),
        }));
        let settings_updates = settings.subscribe();
        let service = Self {
            timeline,
            chat,
            settings,
            config,
            wake,
            settings_updates,
            in_flight: HashMap::new(),
            superseded_in_flight: HashSet::new(),
            monitors: JoinSet::new(),
            commands: command_rx,
            events,
            events_tx,
            snapshots,
        };
        let handle = Arc::new(RemindersHandle {
            commands,
            snapshot: snapshot_rx,
        });
        let task = tokio::spawn(service.run(stop));
        (handle, task)
    }

    async fn run(mut self, stop: CancellationToken) {
        self.publish();
        loop {
            let deadline = self.next_deadline();
            let clock = self.config.clock.as_ref();
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                event = self.events.recv() => match event {
                    Some(event) => self.handle_delivery(event),
                    None => break,
                },
                _ = wait_settings(&mut self.settings_updates) => {}
                _ = self.wake.notified() => {}
                command = self.commands.recv() => match command {
                    Some(command) => self.handle(command).await,
                    None => break,
                },
                _ = wait_clock(clock, deadline) => {}
            }
            while let Ok(event) = self.events.try_recv() {
                self.handle_delivery(event);
            }
            while let Some(result) = self.monitors.try_join_next() {
                if let Err(error) = result {
                    tracing::warn!(%error, "reminder delivery observer failed");
                }
            }
            if stop.is_cancelled() {
                break;
            }
            if self.config.start_scheduler
                && let Err(error) = self.advance()
            {
                tracing::warn!(%error, "reminder scheduler pass failed");
            }
            self.publish();
        }
        self.monitors.abort_all();
        while self.monitors.join_next().await.is_some() {}
        self.in_flight.clear();
        self.publish_closed();
    }

    fn next_deadline(&self) -> Option<UtcInstant> {
        if !self.config.start_scheduler {
            return None;
        }
        let now = self.config.clock.now();
        let settings = match self.read_settings() {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "reminder settings unavailable");
                return None;
            }
        };
        if !settings.enabled {
            // Settings changes wake the loop; a disabled scheduler has no
            // automatic deadline and must not spin on pending deliveries.
            return None;
        }
        let cap = now.checked_add(self.config.idle_poll).unwrap_or(now);
        let timeline = match self.timeline.lock() {
            Ok(timeline) => timeline,
            Err(_) => return Some(cap),
        };
        let projections = match Self::projections(&timeline) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "reminder projection unavailable");
                return Some(cap);
            }
        };
        let mut deadline = cap;
        for (_, projection) in projections {
            for delivery in projection.delivery_views().into_iter().filter(|delivery| {
                matches!(
                    delivery.state,
                    DeliveryState::Pending | DeliveryState::Retrying
                )
            }) {
                if self.in_flight.contains_key(&delivery.fired_event_id) {
                    continue;
                }
                deadline = deadline.min(delivery.next_attempt.unwrap_or(now));
            }
            if let Some(due) = projection.next_due() {
                deadline = deadline.min(due);
            }
        }
        Some(deadline)
    }

    fn read_settings(&self) -> Result<RemindersSettings, ReminderError> {
        self.settings
            .snapshot::<RemindersSettings>()
            .map(|snapshot| snapshot.value)
            .map_err(settings_error)
    }

    fn projections(
        timeline: &SessionTimeline,
    ) -> Result<Vec<(CharacterId, ReminderProjection)>, ReminderError> {
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        CharacterId::ALL
            .iter()
            .map(|&character| {
                ReminderProjection::from_facts(selection.branch_id, character, &facts)
                    .map(|projection| (character, projection))
            })
            .collect()
    }

    fn publish(&self) {
        match self.snapshot_value(false) {
            Ok(snapshot) => {
                self.snapshots.send_replace(Arc::new(snapshot));
            }
            Err(error) => tracing::warn!(%error, "reminder snapshot unavailable"),
        }
    }

    fn publish_closed(&self) {
        // Preserve the last published state; never invent a branch or zone
        // when the final read fails during shutdown.
        let mut snapshot = match self.snapshot_value(false) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "final reminder snapshot unavailable");
                (*self.snapshots.borrow()).as_ref().clone()
            }
        };
        snapshot.closed = true;
        self.snapshots.send_replace(Arc::new(snapshot));
    }

    fn snapshot_value(&self, closed: bool) -> Result<RemindersSnapshot, ReminderError> {
        let settings = self.read_settings()?;
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let branch_id = timeline.selection().branch_id;
        let projections = Self::projections(&timeline)?;
        drop(timeline);
        let mut reminders = Vec::new();
        let mut deliveries = Vec::new();
        let mut next_number = 1;
        for (_, projection) in projections {
            next_number = next_number.max(projection.next_number());
            reminders.extend(projection.reminder_views());
            deliveries.extend(projection.delivery_views());
        }
        for delivery in &mut deliveries {
            delivery.request_id = self
                .in_flight
                .get(&delivery.fired_event_id)
                .map(RequestHandle::id);
        }
        for reminder in &mut reminders {
            if let Some(delivery) = &mut reminder.delivery {
                delivery.request_id = self
                    .in_flight
                    .get(&delivery.fired_event_id)
                    .map(RequestHandle::id);
            }
        }
        reminders.sort_by_key(|view| (view.character.to_string(), view.number));
        Ok(RemindersSnapshot {
            closed,
            branch_id,
            enabled: settings.enabled,
            timezone: settings.timezone,
            next_number,
            reminders,
            deliveries,
        })
    }

    fn advance(&mut self) -> Result<usize, ReminderError> {
        let settings = self.read_settings()?;
        let now = self.config.clock.now();
        if !settings.enabled {
            // Disabled pauses new firings and dispatches. Deliveries already
            // in flight keep running and settle normally.
            return Ok(0);
        }
        let fired = self.fire_due(now)?;
        self.dispatch(now)?;
        Ok(fired)
    }

    fn fire_due(&mut self, now: UtcInstant) -> Result<usize, ReminderError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        let mut fired = 0;
        for &character in &CharacterId::ALL {
            let projection =
                ReminderProjection::from_facts(selection.branch_id, character, &facts)?;
            for record in projection.due_records(now) {
                let occurrence = record.next_occurrence;
                let next = next_occurrence(&record, now)?;
                let fired_fact = FiredOccurrence {
                    reminder_id: record.id,
                    occurrence,
                    schedule_revision: record.schedule_revision,
                    character,
                    kind: record.kind,
                    text: record.text.clone(),
                    scheduled_for: record.due,
                    fired_at: now,
                    next,
                };
                let key = format!("reminder-fire:{}:{occurrence}", record.id);
                let draft = super::fired_draft(&fired_fact, now)?;
                match timeline.append_reminder_facts(selection, &key, vec![draft]) {
                    Ok(_) => fired += 1,
                    Err(error) => {
                        tracing::warn!(%error, reminder = %record.id, "could not commit reminder firing");
                    }
                }
            }
        }
        Ok(fired)
    }

    fn dispatch(&mut self, now: UtcInstant) -> Result<(), ReminderError> {
        let pending = {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
            let branch = timeline.selection().branch_id;
            let projections = Self::projections(&timeline)?;
            let mut pending = Vec::new();
            for (_, projection) in projections {
                for delivery in projection.pending_deliveries(now) {
                    if let Some(fired) = projection.fired_of(delivery.fired_event_id) {
                        let generation = projection
                            .generation_of(delivery.fired_event_id)
                            .unwrap_or_default();
                        pending.push((branch, generation, delivery, fired));
                    } else {
                        tracing::error!(
                            delivery = %delivery.id,
                            "pending delivery has no durable fired occurrence"
                        );
                    }
                }
            }
            pending
        };
        for (branch, generation, delivery, fired) in pending {
            if self.in_flight.contains_key(&delivery.fired_event_id) {
                continue;
            }
            if delivery.transport_attempts >= self.config.delivery.transport_retry_limit {
                if let Err(error) = self.settle(
                    branch,
                    delivery.fired_event_id,
                    generation,
                    DeliveryOutcome::Blocked,
                    "delivery retries exhausted".into(),
                    now,
                ) {
                    tracing::warn!(%error, "could not block exhausted delivery");
                }
                continue;
            }
            let guidance = format!(
                "Сработало {}: {}. Отреагируй от своего лица и напомни об этом игроку.",
                fired.kind.label(),
                fired.text
            );
            let request = ReactRequest::new(
                fired.character,
                delivery.fired_event_id,
                guidance,
                RequestSource::Scheduler,
            );
            match self.chat.send(request) {
                Ok(handle) => {
                    self.in_flight
                        .insert(delivery.fired_event_id, handle.clone());
                    self.monitor(branch, delivery.fired_event_id, generation, handle);
                }
                Err(SubmitError::QueueFull) => {
                    if let Err(error) = self.record_failure(
                        branch,
                        delivery.fired_event_id,
                        generation,
                        "request queue is full",
                        now,
                    ) {
                        tracing::warn!(%error, "could not record queue-full delivery failure");
                    }
                }
                Err(SubmitError::Stopped)
                | Err(SubmitError::QueueClosed)
                | Err(SubmitError::Uninitialized) => {
                    // Shutdown or a stopped Core leaves the occurrence pending
                    // for the next process; it is not a failed delivery.
                }
                Err(error) => {
                    if let Err(error) = self.record_failure(
                        branch,
                        delivery.fired_event_id,
                        generation,
                        &error.to_string(),
                        now,
                    ) {
                        tracing::warn!(%error, "could not record delivery failure");
                    }
                }
            }
        }
        Ok(())
    }

    fn monitor(
        &mut self,
        branch: BranchId,
        fired: EventId,
        generation: u32,
        handle: RequestHandle,
    ) {
        let events = self.events_tx.clone();
        self.monitors.spawn(async move {
            let Ok(snapshot) = handle.wait().await else {
                return;
            };
            let _ = events
                .send(DeliveryEvent {
                    branch,
                    fired,
                    generation,
                    outcome: TerminalOutcome::of(&snapshot),
                })
                .await;
        });
    }

    fn handle_delivery(&mut self, event: DeliveryEvent) {
        let now = self.config.clock.now();
        match event.outcome {
            TerminalOutcome::Delivered => {
                self.in_flight.remove(&event.fired);
            }
            TerminalOutcome::Blocked(reason) => {
                match self.settle(
                    event.branch,
                    event.fired,
                    event.generation,
                    DeliveryOutcome::Blocked,
                    reason,
                    now,
                ) {
                    Ok(()) => {
                        self.in_flight.remove(&event.fired);
                    }
                    Err(ReminderError::InvalidState { .. }) => {
                        self.in_flight.remove(&event.fired);
                    }
                    Err(error) => {
                        // Keep the occurrence marked in flight so a lost
                        // settlement cannot trigger a second paid generation
                        // in this process. A restart re-dispatches it.
                        tracing::error!(%error, "could not settle blocked reminder delivery");
                    }
                }
            }
            TerminalOutcome::Cancelled => {
                match self.settle(
                    event.branch,
                    event.fired,
                    event.generation,
                    DeliveryOutcome::Cancelled,
                    "cancelled by user".into(),
                    now,
                ) {
                    Ok(()) | Err(ReminderError::InvalidState { .. }) => {
                        self.in_flight.remove(&event.fired);
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not settle cancelled reminder delivery");
                    }
                }
            }
            TerminalOutcome::Interrupted(CancelReason::ContextChanged)
                if self.superseded_in_flight.remove(&event.fired) =>
            {
                // The reminder was updated or deleted while this occurrence
                // was generating; the stale reply must not be delivered.
                self.in_flight.remove(&event.fired);
                match self.settle(
                    event.branch,
                    event.fired,
                    event.generation,
                    DeliveryOutcome::Superseded,
                    "reminder changed or deleted".into(),
                    now,
                ) {
                    // The projection already derives Superseded from the
                    // updated/deleted record, so an already-settled state is
                    // expected here.
                    Ok(()) | Err(ReminderError::InvalidState { .. }) => {}
                    Err(error) => {
                        tracing::warn!(%error, "could not settle superseded reminder delivery");
                    }
                }
            }
            TerminalOutcome::Interrupted(CancelReason::ContextChanged) => {
                self.in_flight.remove(&event.fired);
                if let Err(error) = self.record_failure(
                    event.branch,
                    event.fired,
                    event.generation,
                    "context changed",
                    now,
                ) {
                    tracing::warn!(%error, "could not record context-change failure");
                }
            }
            TerminalOutcome::Interrupted(_) => {
                self.in_flight.remove(&event.fired);
            }
        }
    }

    /// Read one branch prefix, validate the delivery generation/state against
    /// it, and append the transition with that same head as CAS. A concurrent
    /// external write forces a rebuild and a fresh validation instead of
    /// re-applying the stale transition.
    fn write_transition<F>(
        &mut self,
        branch: BranchId,
        fired: EventId,
        expected_generation: Option<u32>,
        expect: TransitionExpect,
        mut build: F,
    ) -> Result<(), ReminderError>
    where
        F: FnMut(CharacterId, &ReminderProjection) -> Result<(String, EventDraft), ReminderError>,
    {
        for attempt in 0..2 {
            let mut timeline = self
                .timeline
                .lock()
                .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
            let (head, facts) = timeline
                .reminder_snapshot_on(branch)
                .map_err(timeline_error)?;
            let (character, projection) = find_delivery(branch, &facts, fired)?;
            let view = projection
                .delivery_by_event(fired)
                .ok_or(ReminderError::DeliveryNotFound(DeliveryId(fired)))?;
            let state_matches = match expect {
                TransitionExpect::InFlight => {
                    matches!(view.state, DeliveryState::Pending | DeliveryState::Retrying)
                }
                TransitionExpect::Blocked => view.state == DeliveryState::Blocked,
            };
            if expected_generation
                .is_some_and(|generation| projection.generation_of(fired) != Some(generation))
                || !state_matches
            {
                return Err(ReminderError::InvalidState {
                    delivery: view.id,
                    state: view.state,
                });
            }
            let (key, draft) = build(character, &projection)?;
            match timeline.append_delivery_transition(branch, head, &key, draft) {
                Ok(_) => return Ok(()),
                Err(TimelineError::ConcurrentWrite) if attempt == 0 => continue,
                Err(error) => return Err(timeline_error(error)),
            }
        }
        Err(ReminderError::Storage(
            "concurrent reminder transition".into(),
        ))
    }

    fn record_failure(
        &mut self,
        branch: BranchId,
        fired: EventId,
        generation: u32,
        error: &str,
        now: UtcInstant,
    ) -> Result<(), ReminderError> {
        let limit = self.config.delivery.transport_retry_limit;
        let policy = self.config.delivery;
        let mut exhausted = false;
        let result = self.write_transition(
            branch,
            fired,
            Some(generation),
            TransitionExpect::InFlight,
            |character, projection| {
                let view = projection
                    .delivery_by_event(fired)
                    .ok_or(ReminderError::DeliveryNotFound(DeliveryId(fired)))?;
                if view.transport_attempts >= limit {
                    exhausted = true;
                    return Err(ReminderError::Invalid("delivery retries exhausted".into()));
                }
                let attempt = view.attempts.saturating_add(1);
                let next_attempt = now.checked_add(backoff(policy, attempt)).unwrap_or(now);
                let failure = DeliveryFailure {
                    fired_event_id: fired,
                    character,
                    generation,
                    attempt,
                    error: error.to_owned(),
                    next_attempt,
                };
                let draft = super::failure_draft(&failure, now)?;
                Ok((
                    format!("reminder-failure:{}:{generation}:{attempt}", fired.0),
                    draft,
                ))
            },
        );
        match result {
            Ok(()) => Ok(()),
            Err(_) if exhausted => self.settle(
                branch,
                fired,
                generation,
                DeliveryOutcome::Blocked,
                format!("{error}; delivery retries exhausted"),
                now,
            ),
            Err(error) => Err(error),
        }
    }

    fn settle(
        &mut self,
        branch: BranchId,
        fired: EventId,
        generation: u32,
        outcome: DeliveryOutcome,
        reason: String,
        now: UtcInstant,
    ) -> Result<(), ReminderError> {
        self.write_transition(
            branch,
            fired,
            Some(generation),
            TransitionExpect::InFlight,
            |character, _| {
                let settlement = DeliverySettlement {
                    fired_event_id: fired,
                    character,
                    generation,
                    outcome,
                    reason: reason.clone(),
                    settled_at: now,
                };
                let draft = super::settlement_draft(&settlement, now)?;
                Ok((
                    format!("reminder-settlement:{}:{generation}", fired.0),
                    draft,
                ))
            },
        )
    }

    /// Cancel the generation of an occurrence whose schedule was changed or
    /// removed. The durable supersede settlement is written when the monitor
    /// reports the interruption, so a stale reply can never be delivered.
    fn supersede_in_flight(&mut self, projection: &ReminderProjection, reminder: ReminderId) {
        for fired in projection.unsettled_deliveries(reminder) {
            if let Some(handle) = self.in_flight.remove(&fired) {
                self.superseded_in_flight.insert(fired);
                let _ = handle.cancel_with_reason(CancelReason::ContextChanged);
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Create(input, reply) => {
                let _ = reply.send(self.create(input));
            }
            Command::CreateTimer(input, reply) => {
                let _ = reply.send(self.create_timer(input));
            }
            Command::Update(input, reply) => {
                let _ = reply.send(self.update(input));
            }
            Command::Delete {
                character,
                number,
                reply,
            } => {
                let _ = reply.send(self.delete(character, number));
            }
            Command::List { character, reply } => {
                let _ = reply.send(self.list(character));
            }
            Command::SetEnabled { enabled, reply } => {
                let result = self
                    .settings
                    .edit::<RemindersSettings>(|settings| settings.enabled = enabled)
                    .map(|_| ())
                    .map_err(settings_error);
                let _ = reply.send(result);
            }
            Command::SetTimezone { timezone, reply } => {
                let result = self
                    .settings
                    .edit::<RemindersSettings>(|settings| settings.timezone = timezone)
                    .map(|_| ())
                    .map_err(settings_error);
                let _ = reply.send(result);
            }
            Command::Retry { delivery, reply } => {
                let _ = reply.send(self.retry(delivery));
            }
            Command::Now(reply) => {
                let _ = reply.send(self.config.clock.now());
            }
            Command::Tick(reply) => {
                let _ = reply.send(self.tick().await);
            }
            Command::Tool {
                character,
                command,
                reply,
            } => {
                let _ = reply.send(self.tool(character, command));
            }
        }
    }

    async fn tick(&mut self) -> Result<usize, ReminderError> {
        while let Ok(event) = self.events.try_recv() {
            self.handle_delivery(event);
        }
        let fired = self.advance()?;
        self.publish();
        Ok(fired)
    }

    fn create(&mut self, input: CreateReminder) -> Result<ReminderView, ReminderError> {
        input.validate()?;
        let settings = self.read_settings()?;
        let now = self.config.clock.now();
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        let mut projection =
            ReminderProjection::from_facts(selection.branch_id, input.character, &facts)?;
        let record = projection.apply_create(
            input.character,
            input.kind,
            input.text,
            input.due,
            input.repeat,
            input.policy,
            &settings.timezone,
            now,
        )?;
        let change = ReminderChange::Scheduled(record.clone());
        let draft = super::change_draft(&change, None, now)?;
        let key = format!("reminder-create:{}", record.id);
        timeline
            .append_reminder_facts(selection, &key, vec![draft])
            .map_err(timeline_error)?;
        projection.apply_change(change)?;
        Ok(projection.view(&record))
    }

    fn create_timer(&mut self, input: CreateTimer) -> Result<ReminderView, ReminderError> {
        input.validate()?;
        self.create(
            CreateReminder::new(
                input.character,
                input.instruction,
                DueSpec::after(input.delay)?,
            )
            .kind(ReminderKind::Timer),
        )
    }

    fn update(&mut self, input: UpdateReminder) -> Result<ReminderView, ReminderError> {
        let settings = self.read_settings()?;
        let now = self.config.clock.now();
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        let mut projection =
            ReminderProjection::from_facts(selection.branch_id, input.character, &facts)?;
        let previous_schedule = projection
            .record_by_number(input.number)
            .map(|record| record.schedule_revision);
        let record = projection.apply_update(input, &settings.timezone, now)?;
        let change = ReminderChange::Updated(record.clone());
        let draft = super::change_draft(&change, None, now)?;
        let key = format!("reminder-update:{}:{}", record.id, record.revision);
        timeline
            .append_reminder_facts(selection, &key, vec![draft])
            .map_err(timeline_error)?;
        projection.apply_change(change)?;
        let view = projection.view(&record);
        let schedule_changed = previous_schedule != Some(record.schedule_revision);
        drop(timeline);
        if schedule_changed {
            self.supersede_in_flight(&projection, record.id);
        }
        Ok(view)
    }

    fn delete(
        &mut self,
        character: CharacterId,
        number: u64,
    ) -> Result<ReminderView, ReminderError> {
        let now = self.config.clock.now();
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        let mut projection =
            ReminderProjection::from_facts(selection.branch_id, character, &facts)?;
        let record = projection.apply_delete(character, number)?;
        let view = projection.view(&record);
        let change = ReminderChange::Deleted {
            id: record.id,
            number: record.number,
            character,
        };
        let draft = super::change_draft(&change, None, now)?;
        let key = format!("reminder-delete:{}", record.id);
        timeline
            .append_reminder_facts(selection, &key, vec![draft])
            .map_err(timeline_error)?;
        projection.apply_change(change)?;
        drop(timeline);
        self.supersede_in_flight(&projection, record.id);
        Ok(view)
    }

    fn list(&mut self, character: CharacterId) -> Result<Vec<ReminderView>, ReminderError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        Ok(
            ReminderProjection::from_facts(selection.branch_id, character, &facts)?
                .reminder_views(),
        )
    }

    fn retry(&mut self, delivery: DeliveryId) -> Result<(), ReminderError> {
        let now = self.config.clock.now();
        if self.in_flight.contains_key(&delivery.0) {
            return Err(ReminderError::InvalidState {
                delivery,
                state: DeliveryState::Retrying,
            });
        }
        let branch = {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
            timeline.selection().branch_id
        };
        self.write_transition(
            branch,
            delivery.0,
            None,
            TransitionExpect::Blocked,
            |character, projection| {
                let view = projection
                    .delivery_by_event(delivery.0)
                    .ok_or(ReminderError::DeliveryNotFound(delivery))?;
                let record = projection.record_by_id(view.reminder_id).ok_or(
                    ReminderError::InvalidState {
                        delivery,
                        state: DeliveryState::Superseded,
                    },
                )?;
                let fired = projection
                    .fired_of(delivery.0)
                    .ok_or(ReminderError::DeliveryNotFound(delivery))?;
                if record.schedule_revision != fired.schedule_revision {
                    return Err(ReminderError::InvalidState {
                        delivery,
                        state: DeliveryState::Superseded,
                    });
                }
                let generation = projection.retry_count(delivery.0) + 1;
                let retry = super::model::DeliveryRetry {
                    fired_event_id: delivery.0,
                    character,
                    generation,
                    at: now,
                };
                let draft = super::retry_draft(&retry, now)?;
                Ok((
                    format!("reminder-retry:{}:{generation}", delivery.0.0),
                    draft,
                ))
            },
        )
    }

    fn tool(
        &mut self,
        character: CharacterId,
        command: ReminderToolCommand,
    ) -> Result<ReminderToolReply, ReminderError> {
        let settings = self.read_settings()?;
        let now = self.config.clock.now();
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ReminderError::Storage("timeline lock poisoned".into()))?;
        let selection = timeline.selection();
        let facts = timeline.reminder_facts(selection).map_err(timeline_error)?;
        let mut projection =
            ReminderProjection::from_facts(selection.branch_id, character, &facts)?;
        let mut changes = Vec::new();
        let reply = resolve_tool(
            &mut projection,
            character,
            command,
            now,
            &settings.timezone,
            settings.policy,
            &mut changes,
        );
        let mut drafts = Vec::new();
        for change in &changes {
            drafts.push(super::change_draft(change, None, now)?);
        }
        drafts.push(super::tool_result_draft(character, &reply.text, None, now)?);
        // Every tool invocation is its own operation; the key must not depend
        // on clock granularity, otherwise two calls in one instant collide.
        let key = format!("reminder-tool:{}", uuid::Uuid::new_v4());
        timeline
            .append_reminder_facts(selection, &key, drafts)
            .map_err(timeline_error)?;
        Ok(reply)
    }
}

/// Resolve a batch of typed reminder requests against a staged projection.
/// The whole batch is all-or-nothing: if one operation is invalid, no record
/// mutation is returned and only a diagnostic is produced.
pub(crate) fn resolve_requests(
    projection: &mut ReminderProjection,
    character: CharacterId,
    requests: &[ReminderRequest],
    now: UtcInstant,
    zone: &TimeZoneId,
    policy: LocalTimePolicy,
) -> Result<(Vec<ReminderChange>, Option<String>), ReminderError> {
    let mut staged = projection.clone();
    let mut changes = Vec::new();
    let mut diagnostic = None;
    for request in requests {
        if let ReminderRequest::Tool(command) = request {
            let reply = resolve_tool(
                &mut staged,
                character,
                command.clone(),
                now,
                zone,
                policy,
                &mut changes,
            );
            if !reply.ok {
                // A tool error is a canonical, visible result. It carries no
                // record mutation and does not invalidate the whole reply.
                return Ok((Vec::new(), Some(reply.text)));
            }
            diagnostic = Some(reply.text);
            continue;
        }
        apply_request(
            &mut staged,
            character,
            request,
            now,
            zone,
            policy,
            &mut changes,
        )?;
    }
    *projection = staged;
    Ok((changes, diagnostic))
}

fn apply_request(
    projection: &mut ReminderProjection,
    character: CharacterId,
    request: &ReminderRequest,
    now: UtcInstant,
    zone: &TimeZoneId,
    policy: LocalTimePolicy,
    changes: &mut Vec<ReminderChange>,
) -> Result<(), ReminderError> {
    match request {
        ReminderRequest::StructuredAdd { due_iso, text } => {
            let due = parse_structured_due(due_iso, zone)?;
            let record = projection.apply_create(
                character,
                ReminderKind::Reminder,
                text.clone(),
                due,
                RepeatRule::Once,
                policy,
                zone,
                now,
            )?;
            changes.push(ReminderChange::Scheduled(record));
        }
        ReminderRequest::StructuredTimer {
            delay_seconds,
            instruction,
        } => {
            let delay = duration_of(*delay_seconds)?;
            let record = projection.apply_create(
                character,
                ReminderKind::Timer,
                instruction.clone(),
                DueSpec::after(delay)?,
                RepeatRule::Once,
                policy,
                zone,
                now,
            )?;
            changes.push(ReminderChange::Scheduled(record));
        }
        ReminderRequest::StructuredDelete { number } => {
            let record = projection.apply_delete(character, *number)?;
            changes.push(ReminderChange::Deleted {
                id: record.id,
                number: record.number,
                character,
            });
        }
        ReminderRequest::Tool(_) => {
            return Err(ReminderError::Invalid(
                "tool requests are resolved by the batch resolver".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn resolve_tool(
    projection: &mut ReminderProjection,
    character: CharacterId,
    command: ReminderToolCommand,
    now: UtcInstant,
    zone: &TimeZoneId,
    policy: LocalTimePolicy,
    changes: &mut Vec<ReminderChange>,
) -> ReminderToolReply {
    let outcome = match command {
        ReminderToolCommand::List => return ReminderToolReply::ok(format_list(projection)),
        ReminderToolCommand::Add { text, due, repeat } => {
            parse_friendly_due(&due, now, zone, policy)
                .and_then(|due| {
                    projection.apply_create(
                        character,
                        ReminderKind::Reminder,
                        text,
                        due,
                        repeat.unwrap_or(RepeatRule::Once),
                        policy,
                        zone,
                        now,
                    )
                })
                .map(|record| {
                    let number = record.number;
                    changes.push(ReminderChange::Scheduled(record));
                    format!("Создано напоминание #{number}")
                })
        }
        ReminderToolCommand::Timer {
            instruction,
            delay_seconds,
        } => duration_of(delay_seconds)
            .and_then(|delay| {
                projection.apply_create(
                    character,
                    ReminderKind::Timer,
                    instruction,
                    DueSpec::after(delay)?,
                    RepeatRule::Once,
                    policy,
                    zone,
                    now,
                )
            })
            .map(|record| {
                let number = record.number;
                changes.push(ReminderChange::Scheduled(record));
                format!("Запущен таймер #{number}")
            }),
        ReminderToolCommand::Delete { number } => {
            projection.apply_delete(character, number).map(|record| {
                let number = record.number;
                changes.push(ReminderChange::Deleted {
                    id: record.id,
                    number,
                    character,
                });
                format!("Удалено напоминание #{number}")
            })
        }
        ReminderToolCommand::Update {
            number,
            text,
            due,
            repeat,
        } => {
            let due = match due {
                Some(due) => match parse_friendly_due(&due, now, zone, policy) {
                    Ok(due) => Some(due),
                    Err(error) => return ReminderToolReply::error(error.to_string()),
                },
                None => None,
            };
            let mut input = UpdateReminder::new(character, number);
            if let Some(text) = text {
                input = input.text(text);
            }
            if let Some(due) = due {
                input = input.due(due);
            }
            if let Some(repeat) = repeat {
                input = input.repeat(repeat);
            }
            projection.apply_update(input, zone, now).map(|record| {
                let number = record.number;
                changes.push(ReminderChange::Updated(record));
                format!("Обновлено напоминание #{number}")
            })
        }
    };
    match outcome {
        Ok(text) => ReminderToolReply::ok(text),
        Err(error) => ReminderToolReply::error(error.to_string()),
    }
}

fn format_list(projection: &ReminderProjection) -> String {
    let views = projection.reminder_views();
    if views.is_empty() {
        return "Нет активных напоминаний.".into();
    }
    let mut lines = vec![format!("Активные напоминания: {}", views.len())];
    for view in views {
        let state = match view.delivery.as_ref().map(|delivery| delivery.state) {
            Some(DeliveryState::Blocked) => " [доставка заблокирована, нужен повтор]",
            Some(DeliveryState::Retrying) => " [повтор доставки]",
            Some(DeliveryState::Cancelled) => " [доставка отменена]",
            Some(DeliveryState::Superseded) => " [доставка устарела]",
            _ => "",
        };
        lines.push(format!("{}{state}", view.summary()));
    }
    lines.join("\n")
}

fn duration_of(seconds: f64) -> Result<Duration, ReminderError> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(ReminderError::Invalid(
            "delay must be a positive finite number".into(),
        ));
    }
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| ReminderError::Invalid("delay is outside the supported range".into()))
}

/// The next occurrence is the first one strictly after `now`; missed
/// occurrences are coalesced instead of fired as a burst.
fn next_occurrence(
    record: &ReminderRecord,
    now: UtcInstant,
) -> Result<Option<NextOccurrence>, ReminderError> {
    let out_of_range = || ReminderError::Invalid("repeat is out of range".into());
    match record.repeat {
        RepeatRule::Once => Ok(None),
        RepeatRule::EverySeconds { seconds } => {
            if seconds == 0 {
                return Err(out_of_range());
            }
            let interval = u128::from(seconds);
            let elapsed = now.duration_since(record.due).as_micros() / 1_000_000;
            let steps = elapsed / interval + 1;
            let steps = u64::try_from(steps).map_err(|_| out_of_range())?;
            let total = steps.checked_mul(seconds).ok_or_else(out_of_range)?;
            let total = i64::try_from(total).map_err(|_| out_of_range())?;
            let due = record
                .due
                .checked_add_seconds(total)
                .ok_or_else(out_of_range)?;
            let occurrence = record
                .next_occurrence
                .checked_add(steps)
                .ok_or_else(out_of_range)?;
            let zone = record.zone.zone();
            Ok(Some(NextOccurrence {
                occurrence,
                due,
                due_local: due.local(&zone),
            }))
        }
        RepeatRule::EveryLocalDays { days } => {
            if days == 0 {
                return Err(out_of_range());
            }
            let zone = record.zone.zone();
            // Calendar recurrence must be derived from local dates, not from
            // elapsed UTC hours: a 23h or 25h DST day would otherwise skip or
            // repeat a candidate.
            let elapsed_days = now
                .local(&zone)
                .date()
                .signed_duration_since(record.due_local.date())
                .num_days()
                .max(0) as u64;
            let mut steps = (elapsed_days / u64::from(days)).max(1);
            let mut local = record
                .due_local
                .checked_add_days(day_offset(steps, days)?)
                .ok_or_else(out_of_range)?;
            let mut due = zone.resolve(local, record.policy)?;
            let mut guard = 0;
            while due <= now {
                if guard == 3 {
                    return Err(out_of_range());
                }
                steps = steps.checked_add(1).ok_or_else(out_of_range)?;
                local = local.checked_add_days(days).ok_or_else(out_of_range)?;
                due = zone.resolve(local, record.policy)?;
                guard += 1;
            }
            // A DST shift can make the derived candidate one day too late; step
            // back while the previous candidate is still strictly in future.
            for _ in 0..2 {
                if steps <= 1 {
                    break;
                }
                let previous_local = record
                    .due_local
                    .checked_add_days(day_offset(steps - 1, days)?)
                    .ok_or_else(out_of_range)?;
                let previous_due = zone.resolve(previous_local, record.policy)?;
                if previous_due <= now {
                    break;
                }
                steps -= 1;
                local = previous_local;
                due = previous_due;
            }
            let occurrence = record
                .next_occurrence
                .checked_add(steps)
                .ok_or_else(out_of_range)?;
            Ok(Some(NextOccurrence {
                occurrence,
                due,
                due_local: local,
            }))
        }
    }
}

fn day_offset(steps: u64, days: u32) -> Result<u32, ReminderError> {
    steps
        .checked_mul(u64::from(days))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| ReminderError::Invalid("repeat is out of range".into()))
}

fn backoff(policy: crate::reminders::DeliveryPolicy, attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(16);
    policy
        .transport_retry_initial
        .saturating_mul(factor)
        .min(policy.transport_retry_max)
}

async fn wait_clock(clock: &dyn Clock, deadline: Option<UtcInstant>) {
    match deadline {
        Some(deadline) => clock.sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

async fn wait_settings(updates: &mut watch::Receiver<Arc<SettingsSnapshot>>) {
    loop {
        match updates.changed().await {
            Ok(()) => return,
            Err(_) => std::future::pending::<()>().await,
        }
    }
}

fn settings_error(error: SettingsError) -> ReminderError {
    match error {
        SettingsError::Uninitialized => ReminderError::Uninitialized,
        SettingsError::Stopped => ReminderError::Stopped,
        other => ReminderError::Storage(other.to_string()),
    }
}

fn timeline_error(error: TimelineError) -> ReminderError {
    ReminderError::Storage(error.to_string())
}

#[cfg(test)]
mod recurrence_tests {
    use super::*;
    use crate::reminders::{LocalDateTime, ReminderKind};

    fn record(local: &str, zone: &str, repeat: RepeatRule) -> ReminderRecord {
        let zone = TimeZoneId::parse(zone).unwrap();
        ReminderProjection::empty(BranchId::new(), CharacterId::Crazy)
            .apply_create(
                CharacterId::Crazy,
                ReminderKind::Reminder,
                "calendar recurrence".into(),
                DueSpec::local(LocalDateTime::parse(local).unwrap(), zone.clone()),
                repeat,
                LocalTimePolicy::LENIENT,
                &zone,
                UtcInstant::parse_rfc3339("2026-01-01T00:00:00Z").unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn calendar_repeats_keep_local_hour_across_short_and_long_days() {
        for (local, expected, hours) in [
            ("2026-03-28T12:00:00", "2026-03-29T10:00:00Z", 23),
            ("2026-10-24T12:00:00", "2026-10-25T11:00:00Z", 25),
        ] {
            let record = record(
                local,
                "Europe/Berlin",
                RepeatRule::EveryLocalDays { days: 1 },
            );
            let next = next_occurrence(&record, record.due).unwrap().unwrap();
            assert_eq!(next.due.to_rfc3339(), expected);
            assert_eq!(next.due.duration_since(record.due).as_secs(), hours * 3600);
            assert_eq!(next.occurrence, 1);
        }
    }

    #[test]
    fn long_outage_coalesces_without_a_burst_or_occurrence_reset() {
        for repeat in [
            RepeatRule::EverySeconds { seconds: 1 },
            RepeatRule::EveryLocalDays { days: 1 },
        ] {
            let mut record = record("2026-01-02T12:00:00", "UTC", repeat);
            record.next_occurrence = 7;
            let now = record
                .due
                .checked_add(Duration::from_secs(30 * 86400))
                .unwrap();
            let next = next_occurrence(&record, now).unwrap().unwrap();
            let steps = if matches!(repeat, RepeatRule::EverySeconds { .. }) {
                30 * 86400 + 1
            } else {
                31
            };
            assert_eq!(next.occurrence, 7 + steps);
            assert!(next.due > now);
        }
    }
}
