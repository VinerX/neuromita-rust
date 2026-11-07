#![cfg(feature = "sqlite-system")]

use std::{num::NonZeroUsize, path::Path, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
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
    request::{CancelReason, RequestOutcome, RequestSource, RequestState, TurnIntent},
    timeline::{
        TimelineConfig,
        domain::{BranchId, SessionId},
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

struct Call {
    input: GenerationInput,
    reply: oneshot::Sender<StructuredResponse>,
}

struct Controlled(mpsc::UnboundedSender<Call>);

impl GenerationProvider for Controlled {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(Call { input, reply })
                .map_err(|_| GenerationError::Transport("fixture closed".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture dropped".into()))
        })
    }
}

async fn start(
    database: &Path,
    session: SessionId,
    branch: BranchId,
    clock: Arc<ManualClock>,
) -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, receive) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(Controlled(send));
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "fixture-unused",
            GenerationSettings::new(0.0, 512, Duration::from_secs(10)).unwrap(),
        )
        .unwrap(),
    ));
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(16).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(16).unwrap(),
            retained_finished: NonZeroUsize::new(16).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.timeline = TimelineConfig::Sqlite {
        database: database.to_owned(),
        session,
        root: branch,
    };
    let mut reminders = ReminderConfig::system();
    reminders.clock = clock;
    reminders.start_scheduler = false;
    config.reminders = reminders;
    (ApplicationLifecycle::start(config).await.unwrap(), receive)
}

async fn next(calls: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(10), calls.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn delivered() {
    let mut updates = RemindersApi::observe().unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            if updates
                .borrow_and_update()
                .deliveries
                .first()
                .is_some_and(|d| d.state == DeliveryState::Delivered)
            {
                return;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn shutdown_interrupted_occurrence_resumes_once_and_accepted_reply_survives_restart() {
    let directory = std::env::temp_dir().join(format!(
        "neuromita-reminder-restart-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&directory).unwrap();
    let database = directory.join("timeline.db");
    let session = SessionId::new();
    let branch = BranchId::new();
    let clock = Arc::new(ManualClock::new(
        UtcInstant::parse_rfc3339("2026-11-03T09:00:00Z").unwrap(),
    ));
    let character = CharacterId::Crazy;

    let (app, mut calls) = start(&database, session, branch, clock.clone()).await;
    let created = RemindersApi::create_timer(CreateTimer::new(
        character,
        "survive restart",
        Duration::from_secs(5),
    ))
    .await
    .unwrap();
    app.shutdown().await.unwrap();
    clock.advance(Duration::from_secs(3600));
    let (app, new_calls) = start(&database, session, branch, clock.clone()).await;
    assert!(calls.try_recv().is_err());
    calls = new_calls;
    assert_eq!(
        RemindersApi::list(character).await.unwrap()[0].id,
        created.id
    );
    let mut requests = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let handle = timeout(Duration::from_secs(10), requests.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        handle.snapshot().metadata.source,
        RequestSource::Scheduler
    ));
    let pending = next(&mut calls).await;
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    let fired = snapshot.deliveries[0].fired_event_id;
    assert_eq!(pending.input.context.focus.intent, TurnIntent::React(fired));
    assert_eq!(
        pending
            .input
            .context
            .focus
            .reaction
            .as_ref()
            .unwrap()
            .event_id,
        fired
    );
    assert!(
        pending
            .input
            .context
            .current_input
            .contains("survive restart")
    );
    app.shutdown().await.unwrap();
    let stopped = timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(stopped.state, RequestState::Cancelled));
    assert_eq!(stopped.cancel_reason, Some(CancelReason::Shutdown));
    assert!(
        pending
            .reply
            .send(StructuredResponse::spoken("late discarded"))
            .is_err()
    );

    let (app, mut calls) = start(&database, session, branch, clock.clone()).await;
    let mut requests = ChatApi::observe_requests().unwrap();
    RemindersApi::tick().await.unwrap();
    let handle = timeout(Duration::from_secs(10), requests.next())
        .await
        .unwrap()
        .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.input.context.focus.intent, TurnIntent::React(fired));
    call.reply
        .send(StructuredResponse::spoken("Reminder delivered"))
        .unwrap();
    let accepted = timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap();
    let event = match &accepted.state {
        RequestState::Finished(
            RequestOutcome::Success(reply)
            | RequestOutcome::VoiceGenerationFailed {
                response: reply, ..
            },
        ) => reply.event_id,
        other => panic!("expected accepted reply: {other:?}"),
    };
    delivered().await;
    assert_eq!(RemindersApi::snapshot().unwrap().deliveries.len(), 1);
    app.shutdown().await.unwrap();

    let (app, mut calls) = start(&database, session, branch, clock.clone()).await;
    RemindersApi::tick().await.unwrap();
    let snapshot = RemindersApi::snapshot().unwrap();
    assert_eq!(snapshot.deliveries.len(), 1);
    assert_eq!(snapshot.deliveries[0].fired_event_id, fired);
    assert_eq!(snapshot.deliveries[0].state, DeliveryState::Delivered);
    assert!(calls.try_recv().is_err());
    assert!(app.at_event(event).is_some());
    assert!(RemindersApi::list(character).await.unwrap().is_empty());
    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
