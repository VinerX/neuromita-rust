use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    reminders::{
        CreateTimer, DeliveryPolicy, DeliveryState, DueSpec, ManualClock, ReminderConfig,
        ReminderToolCommand, RemindersApi, UpdateReminder, UtcInstant,
    },
    request::{
        ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState, SubmitError,
        UserInput,
    },
};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    time::timeout,
};

/// One Core per process: tests must not start overlapping lifecycles.
static SERIAL: Mutex<()> = Mutex::const_new(());

struct Call {
    _input: GenerationInput,
    reply: oneshot::Sender<StructuredResponse>,
}

struct ControlledProvider(mpsc::UnboundedSender<Call>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(Call {
                    _input: input,
                    reply,
                })
                .map_err(|_| GenerationError::Transport("fixture closed".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))
        })
    }
}

async fn start(
    policy: ReminderConfig,
    request_queue: usize,
) -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, receive) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(send));
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "scheduler",
            "fixture",
            GenerationSettings::new(0.0, 1024, Duration::from_secs(10)).unwrap(),
        )
        .unwrap(),
    ));
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            // The character handler channel is sized from request_queue, so
            // this is also the transport queue that can report QueueFull.
            request_queue: NonZeroUsize::new(request_queue).unwrap(),
            discovery_buffer: NonZeroUsize::new(32).unwrap(),
            character_commands: NonZeroUsize::new(16).unwrap(),
            presenter_updates: NonZeroUsize::new(32).unwrap(),
            retained_finished: NonZeroUsize::new(16).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.reminders = policy;
    (ApplicationLifecycle::start(config).await.unwrap(), receive)
}

fn manual_clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(
        UtcInstant::parse_rfc3339("2026-11-03T09:00:00Z").unwrap(),
    ))
}

fn manual_config(clock: &Arc<ManualClock>) -> ReminderConfig {
    let mut config = ReminderConfig::system();
    config.clock = clock.clone();
    config.start_scheduler = false;
    config
}

fn try_request() -> Result<RequestHandle, SubmitError> {
    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("scheduler fixture".into()).unwrap(),
    )?;
    ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        event,
        RequestSource::Presenter,
    ))
}

fn request() -> RequestHandle {
    try_request().expect("character command channel must accept the request")
}

fn response() -> StructuredResponse {
    StructuredResponse::spoken("Ответ на напоминание")
}

async fn next(calls: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(10), calls.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn finish(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

async fn delivery_state(expected: DeliveryState) {
    let mut updates = RemindersApi::observe().unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            let reached = updates
                .borrow_and_update()
                .deliveries
                .first()
                .is_some_and(|delivery| delivery.state == expected);
            if reached {
                return;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn wait_enabled(expected: bool) {
    let mut updates = RemindersApi::observe().unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            if updates.borrow_and_update().enabled == expected {
                return;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

/// A full character command queue is a transport failure: the durable firing
/// must survive a bounded number of retries, keep one fired event identity,
/// block once the budget is spent, and deliver exactly once after an explicit
/// retry frees the queue.
#[tokio::test]
async fn queue_full_retries_are_bounded_and_preserve_one_fired_occurrence() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let mut config = manual_config(&clock);
    config.delivery = DeliveryPolicy {
        transport_retry_initial: Duration::from_secs(1),
        transport_retry_max: Duration::from_secs(1),
        transport_retry_limit: 2,
    };
    let (app, mut calls) = start(config, 1).await;
    let character = CharacterId::Crazy;

    // Hold one generation open so the transport queue has no spare worker,
    // then schedule the reminder that must hit the full queue.
    let busy = request();
    let busy_call = next(&mut calls).await;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "queue full occurrence",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));

    // Saturate immediately before tick: no await between the burst and the
    // tick command, so the handler cannot drain the channel before the
    // scheduler dispatches.
    let mut queued = vec![request()];
    let mut saw_full = false;
    // request_queue is 1 here: one queued request fills the channel and the
    // next submission must be rejected.
    for _ in 0..8 {
        match try_request() {
            Ok(handle) => queued.push(handle),
            Err(SubmitError::QueueFull) => {
                saw_full = true;
                break;
            }
            Err(_) => panic!("unexpected submit error"),
        }
    }
    assert!(saw_full, "the character command channel must saturate");

    RemindersApi::tick().await.unwrap();

    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    let fired = snapshot.deliveries[0].fired_event_id;
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Retrying);
    assert_eq!(snapshot.deliveries[0].transport_attempts, 1);
    let delivery = snapshot.deliveries[0].id;

    clock.advance(Duration::from_secs(1));
    RemindersApi::tick().await.unwrap();
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries[0].transport_attempts, 2);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Retrying);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);

    clock.advance(Duration::from_secs(1));
    RemindersApi::tick().await.unwrap();
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Blocked);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);

    // Free the queue, then retry the blocked occurrence explicitly.
    busy_call.reply.send(response()).unwrap();
    finish(&busy).await;
    for handle in &queued {
        let call = next(&mut calls).await;
        call.reply.send(response()).unwrap();
        finish(handle).await;
    }

    RemindersApi::retry(delivery).await.unwrap();
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let redelivered = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    next(&mut calls).await.reply.send(response()).unwrap();
    finish(&redelivered).await;
    delivery_state(DeliveryState::Delivered).await;

    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);
    assert!(RemindersApi::retry(delivery).await.is_err());
    RemindersApi::tick().await.unwrap();
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
}

/// Disabling reminders is a global pause: due occurrences stay scheduled and
/// no generation starts until the setting is enabled again.
#[tokio::test]
async fn global_disable_pauses_firing_and_dispatch_until_reenabled() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let (app, mut calls) = start(manual_config(&clock), 4).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "paused timer",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    RemindersApi::set_enabled(false).await.unwrap();
    wait_enabled(false).await;

    clock.advance(Duration::from_secs(2));
    assert_eq!(RemindersApi::tick().await.unwrap(), 0);
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.reminders.len(), 1);
    assert!(snapshot.deliveries.is_empty());

    clock.advance(Duration::from_secs(60));
    RemindersApi::tick().await.unwrap();
    assert!(RemindersApi::snapshot().unwrap().deliveries.is_empty());
    assert!(calls.try_recv().is_err());
    assert_eq!(RemindersApi::list(character).await.unwrap().len(), 1);

    RemindersApi::set_enabled(true).await.unwrap();
    wait_enabled(true).await;
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let fired = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    next(&mut calls).await.reply.send(response()).unwrap();
    finish(&fired).await;
    delivery_state(DeliveryState::Delivered).await;
    app.shutdown().await.unwrap();
}

/// A user cancellation is a terminal settlement: the occurrence is never
/// redispatched and no new generation is started later.
#[tokio::test]
async fn user_cancel_is_terminal_and_is_not_redispatched() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let (app, mut calls) = start(manual_config(&clock), 4).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "cancelled timer",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let handle = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    let call = next(&mut calls).await;
    let _ = handle.cancel();
    drop(call.reply);
    finish(&handle).await;
    delivery_state(DeliveryState::Cancelled).await;

    let fired = RemindersApi::snapshot().unwrap().deliveries[0].fired_event_id;
    clock.advance(Duration::from_secs(3_600));
    for _ in 0..3 {
        RemindersApi::tick().await.unwrap();
    }
    assert!(calls.try_recv().is_err());
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Cancelled);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);
    app.shutdown().await.unwrap();
}

/// Deleting a reminder while its occurrence is generating cancels that
/// generation; the late reply must not be delivered and nothing is
/// redispatched.
#[tokio::test]
async fn delete_while_pending_discards_the_late_reply() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let (app, mut calls) = start(manual_config(&clock), 4).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "deleted while pending",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let handle = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    let call = next(&mut calls).await;

    RemindersApi::delete(character, 1).await.unwrap();
    delivery_state(DeliveryState::Superseded).await;
    let _ = call.reply.send(response());
    finish(&handle).await;

    clock.advance(Duration::from_secs(3_600));
    RemindersApi::tick().await.unwrap();
    assert!(calls.try_recv().is_err());
    assert!(RemindersApi::list(character).await.unwrap().is_empty());
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Superseded);
    app.shutdown().await.unwrap();
}

/// Moving a reminder to a new due time while its occurrence is generating
/// supersedes that occurrence, keeps the monotonically advanced occurrence
/// index, and drops the late reply.
#[tokio::test]
async fn update_while_pending_discards_late_reply_and_keeps_occurrence_index() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let (app, mut calls) = start(manual_config(&clock), 4).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "updated while pending",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let handle = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    let call = next(&mut calls).await;

    let updated = RemindersApi::update(
        UpdateReminder::new(character, 1).due(DueSpec::after(Duration::from_secs(3_600)).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.next_occurrence, 1);
    delivery_state(DeliveryState::Superseded).await;
    let _ = call.reply.send(response());
    finish(&handle).await;

    clock.advance(Duration::from_secs(60));
    RemindersApi::tick().await.unwrap();
    assert!(calls.try_recv().is_err());
    let list = RemindersApi::list(character).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].revision, 2);
    assert_eq!(list[0].next_occurrence, 1);
    assert!(list[0].due_utc > RemindersApi::now().await.unwrap());
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Superseded);
    app.shutdown().await.unwrap();
}

/// A reminder deleted through the typed tool while its occurrence is
/// generating must not receive that occurrence's reply. The tool path appends
/// the deletion without cancelling the generation, so the accept guard is what
/// rejects the stale reply.
#[tokio::test]
async fn tool_delete_while_pending_rejects_the_late_delivery_reply() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let (app, mut calls) = start(manual_config(&clock), 8).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "tool deleted while pending",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let delivery = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    let delivery_call = next(&mut calls).await;

    // The typed tool deletes the reminder without cancelling the in-flight
    // generation; only the accept guard can keep the stale reply out.
    let deleted = RemindersApi::run_tool(character, ReminderToolCommand::Delete { number: 1 })
        .await
        .unwrap();
    assert!(deleted.ok, "{}", deleted.text);
    assert!(RemindersApi::list(character).await.unwrap().is_empty());
    delivery_state(DeliveryState::Superseded).await;

    let accepted_before = CharacterApi::get(character)
        .unwrap()
        .state()
        .await
        .unwrap()
        .accepted_replies;

    delivery_call.reply.send(response()).unwrap();
    let finished = finish(&delivery).await;
    assert!(
        matches!(
            finished.state,
            RequestState::Finished(RequestOutcome::InternalError(_))
        ),
        "the stale delivery reply must be rejected"
    );

    let accepted_after = CharacterApi::get(character)
        .unwrap()
        .state()
        .await
        .unwrap()
        .accepted_replies;
    assert_eq!(accepted_before, accepted_after);
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Superseded);
    RemindersApi::tick().await.unwrap();
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
}

/// The automatic scheduler must wake on the injected clock advance instead of
/// waiting for the idle poll, and it must fire without an explicit tick.
#[tokio::test]
async fn automatic_scheduler_fires_immediately_on_manual_clock_advance() {
    let _guard = SERIAL.lock().await;
    let clock = manual_clock();
    let mut config = manual_config(&clock);
    config.start_scheduler = true;
    let (app, mut calls) = start(config, 4).await;
    let character = CharacterId::Crazy;

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "auto fired",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    assert!(calls.try_recv().is_err(), "the timer is not due yet");

    clock.advance(Duration::from_secs(2));
    let call = timeout(Duration::from_secs(5), calls.recv())
        .await
        .expect("the scheduler must wake on the clock advance, not the idle poll")
        .unwrap();
    call.reply.send(response()).unwrap();
    delivery_state(DeliveryState::Delivered).await;

    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Delivered);
    app.shutdown().await.unwrap();
}
