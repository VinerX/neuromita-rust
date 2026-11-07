use std::{num::NonZeroUsize, sync::Arc, time::Duration};

#[cfg(feature = "sqlite-system")]
use neuromita::timeline::{
    TimelineConfig,
    domain::{BranchId, SessionId},
};
use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{
        ReactRequest, ReplyRequest, RequestOutcome, RequestSource, RequestState, SubmitError,
        UserInput,
    },
    timeline::{
        EventContent,
        conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
        domain::EventKind,
        ingress::{SemanticBatch, SemanticEvent},
    },
};
use serde_json::json;
use uuid::Uuid;

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

struct LlmCall {
    context: neuromita::generation::GenerationInput,
    reply: oneshot::Sender<Arc<str>>,
}

struct ControlledProvider(mpsc::UnboundedSender<LlmCall>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: neuromita::generation::GenerationInput,
        _template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(LlmCall { context, reply })
                .map_err(|_| GenerationError::Transport("test provider stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("test response dropped".into()))
                .map(|text| neuromita::generation::StructuredResponse::spoken(text.to_string()))
        })
    }
}

fn preset() -> GenerationPreset {
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test-model",
            "test-key",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ))
}

fn config(sender: mpsc::UnboundedSender<LlmCall>) -> ApplicationConfig {
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(sender));
    ApplicationConfig::minimal(
        preset(),
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(8).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(8).unwrap(),
            retained_finished: NonZeroUsize::new(8).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    )
}

fn character_message(text: &str, witnessed_by: &[CharacterId]) -> ConversationMessage {
    let mut message = ConversationMessage::new(
        "unity:dialogue:source",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!(text),
        witnessed_by.iter().copied(),
    );
    message
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Kind));
    message.audience.participants.extend([
        ConversationSpeaker::Character(CharacterId::Crazy),
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);
    message
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unity_decides_whether_an_observed_character_message_receives_a_reply() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let source = character_message("KindMita, are you ready?", &[CharacterId::Kind]);
    let source_id = source.id;
    app.publish_conversation_message(app.timeline_selection(), source)
        .unwrap();
    assert_eq!(app.at().through, 1);
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );

    let mut audience = ConversationAudience::private_to(CharacterId::Kind);
    audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Crazy));
    audience.participants.extend([
        ConversationSpeaker::Character(CharacterId::Crazy),
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);
    audience.seen_by.insert(CharacterId::Crazy);

    let handle = ChatApi::reply(
        ReplyRequest::new(CharacterId::Kind, source_id, RequestSource::Presenter)
            .with_audience(audience),
    )
    .unwrap();

    let generated = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(generated.context.view.count(), 1);
    assert_eq!(generated.context.view.messages()[0].event_id, source_id);
    assert!(generated.context.current_input.is_empty());
    generated.reply.send(Arc::from("Ready, Mita!")).unwrap();

    let completed = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    let RequestState::Finished(RequestOutcome::Success(accepted)) = &completed.state else {
        panic!("expected successful generated reply: {:?}", completed.state);
    };
    let accepted_id = accepted.event_id;
    let view_mita = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let view_kind = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(view_mita.view.count(), 2);
    assert_eq!(view_kind.view.count(), 2);
    assert_eq!(view_mita.view.messages()[1].event_id, accepted_id);
    assert_eq!(view_mita.view.provider_messages()[1]["role"], "user");
    assert_eq!(view_kind.view.provider_messages()[1]["role"], "assistant");

    let events = app.timeline();
    assert_eq!(app.at().through, 2);
    assert_eq!(events.len(), 1);
    assert!(!events.iter().any(|event| matches!(
        event.content,
        neuromita::timeline::EventContent::PlayerMessage { .. }
    )));
    assert!(events.iter().any(|event| match &event.content {
        neuromita::timeline::EventContent::CharacterReply { reply_to, .. } =>
            *reply_to == source_id,
        _ => false,
    }));
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_requires_a_target_known_to_the_selected_character() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let source = character_message("Private", &[]);
    let source_id = source.id;
    app.publish_conversation_message(app.timeline_selection(), source)
        .unwrap();
    let result = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        source_id,
        RequestSource::Presenter,
    ));
    assert!(matches!(result, Err(SubmitError::InvalidTargetEvent)));
    assert_eq!(app.at().through, 1);
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn react_accepts_an_observed_unity_event_but_reply_rejects_it() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let mut source = SemanticEvent::new(EventKind::UnityEvent, [CharacterId::Kind]);
    let source_id = source.id;
    source.payload.insert("name".into(), json!("DoorOpened"));
    app.publish_semantic_events(
        app.timeline_selection(),
        SemanticBatch::new("unity:door-open", vec![source]),
    )
    .unwrap();
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );

    let rejected = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        source_id,
        RequestSource::Presenter,
    ));
    assert!(matches!(rejected, Err(SubmitError::InvalidTargetEvent)));
    assert_eq!(app.at().through, 1);
    let handle = ChatApi::react(ReactRequest::new(
        CharacterId::Kind,
        source_id,
        Arc::clone(
            UserInput::new("React verbally to the observed door opening".into())
                .unwrap()
                .text(),
        ),
        RequestSource::Presenter,
    ))
    .unwrap();
    let generated = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(generated.context.view.world_events().len(), 1);
    assert_eq!(generated.context.view.world_events()[0].event_id, source_id);
    generated
        .reply
        .send(Arc::from("Who opened the door?"))
        .unwrap();
    let completed = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        completed.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let events = app.timeline();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.content, EventContent::PlayerMessage { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event.content,
        EventContent::Reaction { target, .. } if target == source_id
    )));
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unity_directed_reply_survives_sqlite_restart_without_player_message() {
    let _lock = TEST_LOCK.lock().await;
    let path = std::env::temp_dir().join(format!("neuromita-directed-reply-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let source = character_message("KindMita, over here!", &[CharacterId::Kind]);
    let source_id = source.id;
    let reply_id = {
        let (sent, mut calls) = mpsc::unbounded_channel();
        let mut configuration = config(sent);
        configuration.timeline = TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        };
        let app = ApplicationLifecycle::start(configuration).await.unwrap();
        app.publish_conversation_message(app.timeline_selection(), source)
            .unwrap();
        let handle = ChatApi::reply(ReplyRequest::new(
            CharacterId::Kind,
            source_id,
            RequestSource::Presenter,
        ))
        .unwrap();
        let call = timeout(Duration::from_secs(5), calls.recv())
            .await
            .unwrap()
            .unwrap();
        call.reply.send(Arc::from("Coming!")).unwrap();
        let finished = timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap()
            .unwrap();
        let RequestState::Finished(RequestOutcome::Success(accepted)) = &finished.state else {
            panic!("reply did not complete: {:?}", finished.state);
        };
        let id = accepted.event_id;
        app.shutdown().await.unwrap();
        id
    };
    let (sent, _calls) = mpsc::unbounded_channel();
    let mut configuration = config(sent);
    configuration.timeline = TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    };
    let app = ApplicationLifecycle::start(configuration).await.unwrap();
    let events = app.timeline();
    assert!(events.iter().any(|event| event.id == reply_id
        && matches!(
            event.content,
            EventContent::CharacterReply { reply_to, .. } if reply_to == source_id
        )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.content, EventContent::PlayerMessage { .. }))
    );
    let view = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(view.view.messages().len(), 2);
    assert_eq!(view.view.messages()[1].event_id, reply_id);
    app.shutdown().await.unwrap();
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unity_tcp_publishes_message_and_chooses_reply_react_or_initiate() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let stream = TcpStream::connect(app.server().address()).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    async fn receive(
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
        wanted: &str,
    ) -> serde_json::Value {
        timeout(Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                if value["type"] == wanted {
                    return value;
                }
            }
        })
        .await
        .expect("TCP response timed out")
    }

    async fn submit(write: &mut tokio::net::tcp::OwnedWriteHalf, value: serde_json::Value) {
        let line = serde_json::to_string(&value).unwrap();
        write.write_all(line.as_bytes()).await.unwrap();
        write.write_all(b"\n").await.unwrap();
    }

    receive(&mut reader, "connected").await;
    receive(&mut reader, "settings").await;

    let unseen = Uuid::new_v4();
    submit(
        &mut write,
        json!({
            "command":"publish_message", "command_id":Uuid::new_v4(),
            "event_id":unseen, "occurred_at_unix_ms":1762020000000u64, "speaker":{"character":"Crazy"},
            "content":"Only Mita saw this", "seen_by":["Crazy"]
        }),
    )
    .await;
    assert_eq!(
        receive(&mut reader, "published").await["event_id"],
        unseen.to_string()
    );
    assert!(calls.try_recv().is_err());

    submit(
        &mut write,
        json!({
            "command":"react", "command_id":Uuid::new_v4(),
            "event_id":unseen, "character":"Kind", "instruction":"React to what you heard"
        }),
    )
    .await;
    assert_eq!(
        receive(&mut reader, "error").await["error"],
        "target event is not observable or not a message when replying"
    );
    assert!(calls.try_recv().is_err());

    let message = Uuid::new_v4();
    let command_id = Uuid::new_v4();
    let publish = json!({
        "command":"publish_message", "command_id":command_id,
        "event_id":message, "occurred_at_unix_ms":1762020000001u64, "speaker":{"character":"Crazy"},
        "content":"KindMita, let's talk",
        "targets":[{"character":"Kind"}],
        "participants":[{"character":"Crazy"},{"character":"Kind"}],
        "seen_by":["Crazy","Kind"]
    });
    submit(&mut write, publish.clone()).await;
    assert_eq!(
        receive(&mut reader, "published").await["event_id"],
        message.to_string()
    );
    submit(&mut write, publish).await;
    assert_eq!(receive(&mut reader, "published").await["replayed"], true);
    assert!(calls.try_recv().is_err());

    let second = TcpStream::connect(app.server().address()).await.unwrap();
    let (second_read, mut second_write) = second.into_split();
    let mut second_reader = BufReader::new(second_read);
    receive(&mut second_reader, "connected").await;
    receive(&mut second_reader, "settings").await;
    let mut retry = json!({
        "command":"publish_message", "command_id":Uuid::new_v4(),
        "event_id":message, "occurred_at_unix_ms":1762020000001u64,
        "speaker":{"character":"Crazy"}, "content":"KindMita, let's talk",
        "targets":[{"character":"Kind"}],
        "participants":[{"character":"Crazy"},{"character":"Kind"}],
        "seen_by":["Crazy","Kind"]
    });
    submit(&mut second_write, retry.take()).await;
    let deduplicated = receive(&mut second_reader, "published").await;
    assert_eq!(deduplicated["event_id"], message.to_string());
    assert_eq!(app.at().through, 2);
    drop(second_write);
    drop(second_reader);

    submit(
        &mut write,
        json!({
            "command":"reply", "command_id":Uuid::new_v4(),
            "message_id":message, "character":"Kind", "seen_by":["Crazy"]
        }),
    )
    .await;
    receive(&mut reader, "accepted").await;
    let generated = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(generated.context.view.count(), 1);
    assert_eq!(generated.context.view.messages()[0].event_id.0, message);
    generated.reply.send(Arc::from("Yes, Mita")).unwrap();
    let mut completed = receive(&mut reader, "request_status").await;
    while completed["state"] != "finished" {
        completed = receive(&mut reader, "request_status").await;
    }

    submit(
        &mut write,
        json!({
            "command":"react", "command_id":Uuid::new_v4(),
            "event_id":message, "character":"Kind", "instruction":"React briefly"
        }),
    )
    .await;
    receive(&mut reader, "accepted").await;
    let reaction = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    reaction.reply.send(Arc::from("Interesting")).unwrap();
    let mut status = receive(&mut reader, "request_status").await;
    while status["state"] != "finished" {
        status = receive(&mut reader, "request_status").await;
    }

    submit(
        &mut write,
        json!({
            "command":"initiate", "command_id":Uuid::new_v4(),
            "character":"Kind", "instruction":"Start a fresh topic"
        }),
    )
    .await;
    receive(&mut reader, "accepted").await;
    let initiation = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    initiation.reply.send(Arc::from("New topic")).unwrap();
    let mut status = receive(&mut reader, "request_status").await;
    while status["state"] != "finished" {
        status = receive(&mut reader, "request_status").await;
    }

    assert_eq!(app.at().through, 5);
    app.shutdown().await.unwrap();
}
