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
        CreateTimer, DeliveryState, ManualClock, ReminderConfig, RemindersApi, UtcInstant,
    },
    request::{ReplyRequest, RequestHandle, RequestSource, UserInput},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

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

async fn start(policy: ReminderConfig) -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, receive) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(send));
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "acceptance",
            "fixture",
            GenerationSettings::new(0.0, 1024, Duration::from_secs(10)).unwrap(),
        )
        .unwrap(),
    ));
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
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

fn request() -> RequestHandle {
    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("game acceptance".into()).unwrap(),
    )
    .unwrap();
    ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        event,
        RequestSource::Presenter,
    ))
    .unwrap()
}

fn response(commands: &[&str]) -> StructuredResponse {
    let mut response = StructuredResponse::spoken("Проверенный игровой ответ");
    response.segments[0].commands = commands.iter().map(|s| (*s).to_owned()).collect();
    response
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

#[tokio::test]
async fn failed_delivery_blocks_until_explicit_retry_and_keeps_one_occurrence() {
    let clock = Arc::new(ManualClock::new(
        UtcInstant::parse_rfc3339("2026-11-03T09:00:00Z").unwrap(),
    ));
    let mut config = ReminderConfig::system();
    config.clock = clock.clone();
    config.start_scheduler = false;
    let (app, mut calls) = start(config).await;
    let character = CharacterId::Crazy;

    let before = CharacterApi::get(character).unwrap().state().await.unwrap();
    let atomic = request();
    let call = next(&mut calls).await;
    let mut invalid = response(&[]);
    invalid.timer_add = Some(vec!["1|must not be scheduled".into()]);
    invalid.reminder_delete = Some(vec!["999999".into()]);
    invalid.attitude_change = 5.0;
    call.reply.send(invalid).unwrap();
    finish(&atomic).await;
    assert!(RemindersApi::list(character).await.unwrap().is_empty());
    let after = CharacterApi::get(character).unwrap().state().await.unwrap();
    assert_eq!(before.attitude, after.attitude);
    assert_eq!(before.accepted_replies, after.accepted_replies);

    RemindersApi::create_timer(CreateTimer::new(
        character,
        "one durable cause",
        Duration::from_secs(1),
    ))
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    let mut discovery = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let first = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    let call = next(&mut calls).await;
    drop(call.reply);
    finish(&first).await;
    delivery_state(DeliveryState::Blocked).await;
    RemindersApi::tick().await.unwrap();
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    let blocked = &snapshot.deliveries[0];
    assert_eq!(blocked.state, DeliveryState::Blocked);
    let id = blocked.id;
    let fired = blocked.fired_event_id;
    clock.advance(Duration::from_secs(3600));
    for _ in 0..3 {
        RemindersApi::tick().await.unwrap();
    }
    assert!(calls.try_recv().is_err());
    assert_eq!(
        RemindersApi::snapshot().unwrap().deliveries[0].fired_event_id,
        fired
    );

    RemindersApi::retry(id).await.unwrap();
    RemindersApi::tick().await.unwrap();
    let retried = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    drop(next(&mut calls).await.reply);
    finish(&retried).await;
    delivery_state(DeliveryState::Blocked).await;
    RemindersApi::tick().await.unwrap();
    assert_eq!(
        RemindersApi::snapshot().unwrap().deliveries[0].state,
        DeliveryState::Blocked
    );
    RemindersApi::retry(id).await.unwrap();
    RemindersApi::tick().await.unwrap();
    let retried = timeout(Duration::from_secs(10), discovery.next())
        .await
        .unwrap()
        .unwrap();
    next(&mut calls).await.reply.send(response(&[])).unwrap();
    finish(&retried).await;
    delivery_state(DeliveryState::Delivered).await;
    RemindersApi::tick().await.unwrap();
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Delivered);
    assert!(RemindersApi::retry(id).await.is_err());
    RemindersApi::tick().await.unwrap();
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
}
