use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    presentation::ChatPresenter,
    request::{
        CancelResult, ReactRequest, ReplyRequest, RequestHandle, RequestId, RequestOutcome,
        RequestSource, RequestState, SubmitError, UserInput,
    },
    timeline::EventContent,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn explicit_prompt_set_requires_a_catalog_before_provider_invocation() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    app.presenter()
        .settings
        .characters
        .for_character(CharacterId::Crazy)
        .prompt_set
        .set(Some("Default".into()))
        .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "Test prompt selection".into())
        .unwrap();
    let result = timeout(Duration::from_secs(2), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert!(
        server.calls.try_recv().is_err(),
        "a missing catalog must not fall back to the built-in prompt"
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn factory_preset_is_saved_selected_generated_and_reloaded_through_presenters() {
    use neuromita::generation::{
        JsonPresetRepository, PresetCatalog, PresetCatalogError, PresetCredentials,
    };
    use neuromita::presentation::PresetsPresenter;
    struct Credentials;
    impl PresetCredentials for Credentials {
        fn get(&self, key: &str) -> Result<String, PresetCatalogError> {
            if key == "FIXTURE_GEMINI_KEY" {
                Ok("test-key".into())
            } else {
                Err(PresetCatalogError::MissingCredential(key.into()))
            }
        }
    }
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let directory = std::env::temp_dir().join(format!("factory-sandbox-{}", Uuid::new_v4()));
    let setup = || {
        let mut config = ApplicationConfig::persistent_unconfigured(
            ResponseGenerator::new([
                Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
            ])
            .unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &directory,
        )
        .unwrap();
        config.presets = PresetCatalog::open(
            Default::default(),
            Arc::new(JsonPresetRepository::new(directory.join("presets.json"))),
            Arc::new(Credentials),
        )
        .unwrap();
        config
    };
    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    let (id, _) = app.presenter().presets.create_unconfigured().unwrap();
    assert!(app.presenter().presets.require_configured(id).is_err());
    let mut template = PresetsPresenter::template("gemini").unwrap();
    template.endpoint = server.endpoint.clone();
    assert!(
        template
            .configure("fixture".into(), "".into(), "FIXTURE_GEMINI_KEY".into())
            .is_err()
    );
    assert!(PresetsPresenter::template("unknown").is_err());
    let record = template
        .configure(
            "factory fixture".into(),
            "test-model".into(),
            "FIXTURE_GEMINI_KEY".into(),
        )
        .unwrap();
    app.presenter().presets.update(id, record).unwrap();
    app.presenter().settings.presets.active.set(id).unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "factory path".into())
        .unwrap();
    let call = server.next().await;
    assert_eq!(call.body["generationConfig"]["maxOutputTokens"], 3000);
    call.respond.send(Reply::Text("factory response")).unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    let stored = std::fs::read_to_string(directory.join("presets.json")).unwrap();
    assert!(stored.contains("FIXTURE_GEMINI_KEY"));
    assert!(!stored.contains("test-key"));
    let reopened = ApplicationLifecycle::start(setup()).await.unwrap();
    assert_eq!(reopened.presenter().settings.presets.active.get(), id);
    assert!(
        reopened
            .presenter()
            .conversation
            .snapshot(CharacterId::Crazy)
            .await
            .unwrap()
            .model
            .is_ok()
    );
    reopened.shutdown().await.unwrap();
    server.shutdown().await;
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn every_builtin_character_has_an_owner_and_independent_context() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let server = GeminiServer::start().await;
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(app.presenter().conversation.characters().len(), 9);
    for id in CharacterId::ALL {
        let view = app
            .character(id)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap();
        assert_eq!(view.character_id, id);
        assert_eq!(view.state.accepted_replies, 0);
        assert_eq!(
            app.presenter()
                .settings
                .characters
                .for_character(id)
                .reply
                .get(),
            neuromita::settings::PresetSelection::Active
        );
    }
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sandbox_reads_canonical_history_observes_external_requests_and_cancels_queue() {
    use neuromita::{settings::Volume, timeline::domain::MessageRole};
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let sandbox = &app.presenter().conversation;
    assert_eq!(sandbox.characters(), &CharacterId::ALL);
    assert!(
        sandbox
            .snapshot(CharacterId::Crazy)
            .await
            .unwrap()
            .messages
            .is_empty()
    );

    let first = sandbox
        .reply(CharacterId::Crazy, "sandbox input".into())
        .unwrap();
    let call = server.next().await;
    let queued = sandbox
        .reply(CharacterId::Crazy, "cancel before generation".into())
        .unwrap();
    shown(&app.presenter().chat, queued.id(), false).await;
    assert_eq!(
        sandbox.cancel(queued.id()).unwrap(),
        CancelResult::Cancelled
    );
    assert!(matches!(
        finished(&queued).await.state,
        RequestState::Cancelled
    ));
    let before = sandbox.snapshot(CharacterId::Crazy).await.unwrap();
    assert!(
        before
            .messages
            .iter()
            .all(|message| message.role != MessageRole::Assistant)
    );
    assert_eq!(before.messages.len(), 2);
    assert!(
        sandbox
            .snapshot(CharacterId::Kind)
            .await
            .unwrap()
            .messages
            .is_empty()
    );

    call.respond
        .send(Reply::Text("accepted sandbox answer"))
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    shown(&app.presenter().chat, first.id(), true).await;
    let after = sandbox.snapshot(CharacterId::Crazy).await.unwrap();
    assert_eq!(
        after
            .messages
            .iter()
            .filter(|m| m.content == "accepted sandbox answer")
            .count(),
        1
    );
    assert_eq!(after.state.accepted_replies, 1);
    assert!(
        after
            .messages
            .iter()
            .any(|m| m.request_id == Some(first.id()))
    );
    assert_eq!(after.model.as_deref().unwrap(), "test-model");
    assert!(
        after
            .messages
            .iter()
            .all(|m| m.sequence <= after.at.through)
    );

    let mut settings = sandbox.observe_settings().unwrap();
    sandbox.set_volume(Volume::new(27).unwrap()).unwrap();
    settings.changed().await.unwrap();
    assert_eq!(
        sandbox
            .snapshot(CharacterId::Crazy)
            .await
            .unwrap()
            .settings
            .values
            .voice
            .volume
            .get(),
        27
    );

    let external = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        ChatApi::publish_message(
            CharacterId::Kind,
            UserInput::new("external input".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Client(Uuid::new_v4()),
    ))
    .unwrap();
    let call = server.next().await;
    shown(&app.presenter().chat, external.id(), false).await;
    assert!(
        sandbox
            .snapshot(CharacterId::Kind)
            .await
            .unwrap()
            .requests
            .requests
            .iter()
            .any(|r| r.metadata.id == external.id())
    );
    call.respond
        .send(Reply::Text("external accepted answer"))
        .unwrap();
    finished(&external).await;
    let kind = sandbox.snapshot(CharacterId::Kind).await.unwrap();
    assert!(
        kind.messages
            .iter()
            .any(|m| m.content == "external accepted answer")
    );
    assert!(
        !kind
            .messages
            .iter()
            .any(|m| m.content == "accepted sandbox answer")
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[derive(Debug)]
struct Call {
    path: String,
    body: Value,
    respond: oneshot::Sender<Reply>,
}

#[derive(Debug)]
enum Reply {
    Text(&'static str),
    Structured(&'static str),
    Status(u16),
}

struct GeminiServer {
    endpoint: String,
    calls: mpsc::UnboundedReceiver<Call>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl GeminiServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1beta/", listener.local_addr().unwrap());
        let (send, calls) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = server_stop.cancelled() => break,
                    connection = listener.accept() => {
                        let (stream, _) = connection.unwrap();
                        clients.spawn(serve(stream, send.clone()));
                    }
                    result = clients.join_next(), if !clients.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Self {
            endpoint,
            calls,
            stop,
            task,
        }
    }

    async fn next(&mut self) -> Call {
        timeout(Duration::from_secs(10), self.calls.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn shutdown(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}

async fn serve(mut stream: TcpStream, calls: mpsc::UnboundedSender<Call>) {
    let mut bytes = Vec::new();
    let boundary;
    loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            boundary = index + 4;
            break;
        }
        assert!(bytes.len() < 64 * 1024);
    }
    let header = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let path = header.lines().next().unwrap().to_owned();
    let size: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(!header.to_ascii_lowercase().contains("x-goog-api-key:"));
    assert!(!header.to_ascii_lowercase().contains("authorization:"));
    assert!(path.contains(":generateContent?key=test-key "));
    while bytes.len() < boundary + size {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = serde_json::from_slice(&bytes[boundary..boundary + size]).unwrap();
    let (respond, response) = oneshot::channel();
    calls
        .send(Call {
            path,
            body,
            respond,
        })
        .unwrap();
    let Ok(response) = response.await else {
        return;
    };
    let (status, body) = match response {
        Reply::Text(text) => (200, json!({"candidates":[{"content":{"role":"model","parts":[{"text":"private reasoning","thought":true},{"text":json!({"segments":[{"text":text}]}).to_string()}]},"finishReason":"STOP"}]}).to_string()),
        Reply::Structured(text) => (200, json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"finishReason":"STOP"}]}).to_string()),
        Reply::Status(status) => (status, "{}".to_owned()),
    };
    let response = format!(
        "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn preset(endpoint: &str) -> GenerationPreset {
    let settings = GenerationSettings::new(0.7, 512, Duration::from_secs(10)).unwrap();
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(endpoint, "test-model", "test-key", settings).unwrap(),
    ))
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(2).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(16).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(20).unwrap(),
    }
}

struct CheckedProvider(GeminiProvider);

impl GenerationProvider for CheckedProvider {
    fn kind(&self) -> neuromita::generation::ProviderKind {
        neuromita::generation::ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: neuromita::generation::GenerationInput,
        template: ProviderTemplate,
    ) -> neuromita::generation::ProviderFuture<'_> {
        Box::pin(async move {
            assert_ne!(
                context
                    .view
                    .messages()
                    .last()
                    .map(|message| message.content.as_str())
                    .unwrap_or(""),
                "panic provider",
                "injected provider panic"
            );
            self.0.generate(context, template).await
        })
    }
}

async fn finished(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

async fn shown(presenter: &ChatPresenter, id: RequestId, terminal: bool) {
    timeout(Duration::from_secs(10), async {
        let mut model = presenter.observe();
        loop {
            let present = {
                model.borrow_and_update().requests.iter().any(|row| {
                    row.metadata.id == id && (!terminal || row.snapshot.state.is_terminal())
                })
            };
            if present {
                return;
            }
            model.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

fn user_texts(body: &Value) -> Vec<&str> {
    body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .map(|item| item["parts"][0]["text"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presenter_to_provider_observation_cancellation_and_commit() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    assert!(matches!(
        ChatApi::observe_requests(),
        Err(SubmitError::Uninitialized | SubmitError::Stopped)
    ));
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let config = ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let presenter = &app.presenter().chat;
    assert!(presenter.reply(CharacterId::Crazy, " ".into()).is_err());

    let first = presenter.reply(CharacterId::Crazy, "first".into()).unwrap();
    let first_call = server.next().await;
    assert_eq!(
        first_call.path,
        "POST /v1beta/models/test-model:generateContent?key=test-key HTTP/1.1"
    );
    assert_eq!(user_texts(&first_call.body), ["first"]);
    assert_eq!(first_call.body["generationConfig"]["candidateCount"], 1);
    assert_eq!(
        first_call.body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        first_call.body["generationConfig"]["responseSchema"]["required"],
        json!(["segments"])
    );
    assert!(
        first_call.body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Мита")
    );

    let queued = presenter
        .reply(CharacterId::Crazy, "never starts".into())
        .unwrap();
    let running_cancel = presenter
        .reply(CharacterId::Crazy, "cancel during generation".into())
        .unwrap();
    assert!(matches!(
        presenter.reply(CharacterId::Crazy, "queue overflow".into()),
        Err(neuromita::presentation::PresenterError::Submit(
            SubmitError::QueueFull
        ))
    ));
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(matches!(
        finished(&queued).await.state,
        RequestState::Cancelled
    ));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );

    let external = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Kind,
            UserInput::new("external client".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Client(Uuid::new_v4()),
    ))
    .unwrap();
    let external_call = server.next().await;
    assert_eq!(user_texts(&external_call.body), ["external client"]);
    shown(presenter, external.id(), false).await;
    external_call
        .respond
        .send(Reply::Text("kind reply"))
        .unwrap();
    assert!(matches!(
        finished(&external).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    shown(presenter, external.id(), true).await;
    assert!(matches!(
        first.snapshot().state,
        RequestState::Processing(_)
    ));

    first_call.respond.send(Reply::Text("first reply")).unwrap();
    let first_result = finished(&first).await;
    match &first_result.state {
        RequestState::Finished(RequestOutcome::Success(response)) => {
            assert_eq!(&*response.text, "first reply")
        }
        state => panic!("unexpected state: {state:?}"),
    }
    assert!(matches!(first.cancel(), CancelResult::AlreadyFinished));
    let mut late_observer = first.observe();
    assert!(
        late_observer
            .next()
            .await
            .unwrap()
            .unwrap()
            .state
            .is_terminal()
    );
    assert!(late_observer.next().await.unwrap().is_none());

    let cancelled_call = server.next().await;
    let cancelled_user = user_texts(&cancelled_call.body);
    assert_eq!(cancelled_user.len(), 4);
    assert_eq!(cancelled_user[0], "first");
    assert_eq!(cancelled_user[1], "never starts");
    assert!(cancelled_user[2].contains("cancel during generation"));
    assert!(cancelled_user[2].contains("на которое готовится текущий ответ"));
    assert_eq!(cancelled_user[3], "queue overflow");
    assert_eq!(cancelled_call.body["contents"][4]["role"], "model");
    assert_eq!(
        cancelled_call.body["contents"][4]["parts"][0]["text"],
        "first reply"
    );
    assert_eq!(running_cancel.cancel(), CancelResult::Requested);
    assert!(matches!(
        finished(&running_cancel).await.state,
        RequestState::Cancelled
    ));
    let _ = cancelled_call
        .respond
        .send(Reply::Text("must not be accepted"));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        1
    );

    let failed = presenter
        .reply(CharacterId::Crazy, "HTTP failure".into())
        .unwrap();
    server
        .next()
        .await
        .respond
        .send(Reply::Status(503))
        .unwrap();
    assert!(matches!(
        finished(&failed).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));

    let panicked = presenter
        .reply(CharacterId::Crazy, "panic provider".into())
        .unwrap();
    assert!(matches!(
        finished(&panicked).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));

    let next = presenter
        .reply(CharacterId::Crazy, "after failure".into())
        .unwrap();
    let next_call = server.next().await;
    assert!(!next_call.body.to_string().contains("must not be accepted"));
    assert!(next_call.body.to_string().contains("never starts"));
    assert!(next_call.body.to_string().contains("queue overflow"));
    next_call.respond.send(Reply::Text("second reply")).unwrap();
    assert!(matches!(
        finished(&next).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    shown(presenter, next.id(), true).await;
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        2
    );
    let events = app.timeline();
    let replies: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.content, EventContent::CharacterReply { .. }))
        .collect();
    assert_eq!(replies.len(), 3);
    assert!(
        replies
            .iter()
            .any(|event| event.request_id == Some(first.id()))
    );
    assert!(events.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "never starts")
    }));
    assert!(!events.iter().any(|event| {
        event.request_id == Some(queued.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));

    let interrupted = presenter
        .reply(CharacterId::Crazy, "stop application".into())
        .unwrap();
    let stopped_call = server.next().await;
    let pending = presenter
        .reply(CharacterId::Crazy, "stop pending".into())
        .unwrap();
    app.shutdown().await.unwrap();
    assert!(matches!(
        finished(&interrupted).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&pending).await.state,
        RequestState::Cancelled
    ));
    drop(stopped_call);
    assert!(matches!(
        ChatApi::observe_requests(),
        Err(SubmitError::Stopped)
    ));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_structured_segments_keep_emotions_and_order_in_canonical_timeline() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let config = ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let first = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Hi".into())
        .unwrap();
    let incoming = server.next().await;
    assert!(incoming.body["generationConfig"]["responseSchema"]["properties"]
        ["segments"]["items"]["properties"]["emotions"].is_object());
    incoming.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"Hello","emotions":["happy"]},{"text":"How are you?","emotions":["curious"]}]}"#,
    )).unwrap();
    let snapshot = finished(&first).await;
    let accepted = match &snapshot.state {
        RequestState::Finished(RequestOutcome::Success(response)) => response,
        other => panic!("unexpected state: {other:?}"),
    };
    assert_eq!(accepted.text.as_ref(), "Hello How are you?");
    assert_eq!(accepted.segments.len(), 2);
    assert_eq!(accepted.segments[0].emotions, ["happy"]);
    assert_eq!(accepted.segments[1].emotions, ["curious"]);

    let context = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let last = context.view.provider_messages().last().cloned().unwrap();
    assert_eq!(last["content"][0]["text"], "Hello");
    assert_eq!(last["content"][0]["emotions"], json!(["happy"]));
    assert_eq!(last["content"][1]["text"], "How are you?");
    assert_eq!(last["content"][1]["emotions"], json!(["curious"]));
    assert_eq!(
        context.view.messages().last().unwrap().content,
        "Hello How are you?"
    );
    assert_eq!(context.history.events.last().unwrap().id, accepted.event_id);

    let second = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Again".into())
        .unwrap();
    let followup = server.next().await;
    assert!(followup.body.to_string().contains("Hello\\nHow are you?"));
    followup.respond.send(Reply::Text("OK")).unwrap();
    assert!(matches!(
        finished(&second).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn structured_state_deltas_apply_only_with_accepted_reply() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "How do you feel?".into())
        .unwrap();
    let incoming = server.next().await;
    assert!(
        incoming.body["generationConfig"]["responseSchema"]["properties"]["attitude_change"]
            .is_object()
    );
    incoming.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"I'm delighted!","emotions":["happy"]}],"attitude_change":1.25,"boredom_change":-2.0}"#
    )).unwrap();
    let snapshot = finished(&handle).await;
    assert!(matches!(
        snapshot.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert_eq!(app.at().through, 3); // input + message + state change in one guarded commit
    let state = app
        .character(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(state.accepted_replies, 1);
    assert_eq!(state.attitude, 61.25);
    assert_eq!(state.boredom, 8.0);
    assert_eq!(state.stress, 5.0);
    let state_other = app
        .character(CharacterId::Kind)
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(state_other.attitude, 60.0);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn working_state_is_atomic_private_and_available_only_to_its_character() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let first = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "First".into())
        .unwrap();
    let call = server.next().await;
    assert!(
        call.body["generationConfig"]["responseSchema"]["properties"]["working_state"].is_object()
    );
    call.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"I understand"}],"attitude_change":2,"working_state":{"focus":"Offer some tea","open_loops":["Ask about the cup"]}}"#
    )).unwrap();
    let first_snapshot = finished(&first).await;
    let accepted = match &first_snapshot.state {
        RequestState::Finished(RequestOutcome::Success(accepted)) => accepted,
        state => panic!("unexpected result: {state:?}"),
    };
    assert_eq!(accepted.effects.len(), 2);
    assert_eq!(accepted.state_after.attitude, 62.0);
    assert_eq!(app.at().through, 4); // User message + reply + two effects in one commit
    let state = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(state.working_state.focus, "Offer some tea");
    assert_eq!(state.working_state.open_loops, ["Ask about the cup"]);
    assert_eq!(state.view.messages().len(), 2);
    assert!(state.view.world_events().is_empty());
    assert!(
        app.character(CharacterId::Kind)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .working_state
            .is_empty()
    );

    let second = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Continue".into())
        .unwrap();
    let call = server.next().await;
    assert!(call.body.to_string().contains("[WORKING STATE]"));
    assert!(call.body.to_string().contains("Offer some tea"));
    call.respond.send(Reply::Text("All right")).unwrap();
    assert!(matches!(
        finished(&second).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .working_state
            .focus,
        "Offer some tea"
    );

    let third = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Clear".into())
        .unwrap();
    server
        .next()
        .await
        .respond
        .send(Reply::Structured(
            r#"{"segments":[{"text":"Done"}],"working_state":{}}"#,
        ))
        .unwrap();
    assert!(matches!(
        finished(&third).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .working_state
            .is_empty()
    );

    let fourth = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "After clear".into())
        .unwrap();
    let call = server.next().await;
    assert!(!call.body.to_string().contains("[WORKING STATE]"));
    call.respond.send(Reply::Text("OK")).unwrap();
    assert!(matches!(
        finished(&fourth).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_memories_are_atomic_and_visible_in_followup_context() {
    use neuromita::timeline::domain::MemoryPriority;

    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let add = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Remember this".into())
        .unwrap();
    let call = server.next().await;
    assert!(
        call.body["generationConfig"]["responseSchema"]["properties"]["memory_add"].is_object()
    );
    call.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"I remember"}],"memory_add":["high|The player prefers tea", "low|The player mentioned milk"]}"#
    )).unwrap();
    let accepted = finished(&add).await;
    let response = match &accepted.state {
        RequestState::Finished(RequestOutcome::Success(response)) => response,
        state => panic!("unexpected result: {state:?}"),
    };
    assert_eq!(response.effects.len(), 2);
    assert_eq!(app.at().through, 4); // user + message + two independent memory facts
    let original_anchor = app.at();
    let mem = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap()
        .memories;
    assert_eq!(mem.len(), 2);
    assert!(
        mem.iter()
            .any(|item| item.content.contains("tea") && item.priority == MemoryPriority::High)
    );
    assert!(
        app.character(CharacterId::Kind)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    let tea = mem
        .iter()
        .find(|item| item.content.contains("tea"))
        .unwrap()
        .id;
    let milk = mem
        .iter()
        .find(|item| item.content.contains("milk"))
        .unwrap()
        .id;

    let second = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Consolidate".into())
        .unwrap();
    let call = server.next().await;
    assert!(call.body.to_string().contains("[LONG-TERM MEMORY]"));
    assert!(call.body.to_string().contains(&tea.0.to_string()));
    let merged = serde_json::json!({
        "segments": [{"text":"Updated"}],
        "memory_merge": [format!("{},{}:The player prefers tea with milk", tea.0,milk.0)]
    })
    .to_string();
    call.respond
        .send(Reply::Structured(Box::leak(merged.into_boxed_str())))
        .unwrap();
    assert!(matches!(
        finished(&second).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let now = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap()
        .memories;
    assert_eq!(now.len(), 1);
    assert_eq!(now[0].id, tea);
    assert_eq!(now[0].content, "The player prefers tea with milk");
    let retained_after_merge = app
        .presenter()
        .characters
        .memories(CharacterId::Crazy)
        .await
        .unwrap();
    assert_eq!(retained_after_merge.records().len(), 2);
    assert_eq!(
        retained_after_merge
            .records()
            .iter()
            .find(|item| item.id == milk)
            .unwrap()
            .state,
        neuromita::memory::MemoryState::Deleted
    );
    let at_before_bad = app.at();

    let invalid = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Bad memory command".into())
        .unwrap();
    let call = server.next().await;
    let bad = serde_json::json!({
        "segments": [{"text":"Must not apply"}],
        "memory_add": ["critical|This must roll back"],
        "memory_update": [format!("{}|different",uuid::Uuid::new_v4())]
    })
    .to_string();
    call.respond
        .send(Reply::Structured(Box::leak(bad.into_boxed_str())))
        .unwrap();
    assert!(matches!(
        finished(&invalid).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert_eq!(app.at().through, at_before_bad.through + 1); // only the incoming player message
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .memories,
        now
    );
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(original_anchor)
            .await
            .unwrap()
            .memories,
        mem
    );

    let delete = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Forget tea".into())
        .unwrap();
    let call = server.next().await;
    let delete_body =
        serde_json::json!({"segments":[{"text":"Forgotten"}],"memory_delete":[tea.0.to_string()]})
            .to_string();
    call.respond
        .send(Reply::Structured(Box::leak(delete_body.into_boxed_str())))
        .unwrap();
    assert!(matches!(
        finished(&delete).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    let retained_after_delete = app
        .presenter()
        .characters
        .memories(CharacterId::Crazy)
        .await
        .unwrap();
    assert_eq!(
        retained_after_delete
            .records()
            .iter()
            .find(|item| item.id == tea)
            .unwrap()
            .state,
        neuromita::memory::MemoryState::Deleted
    );

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_retrieval_is_bounded_and_duplicate_add_keeps_stable_identity() {
    use neuromita::timeline::domain::MemoryPriority;

    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let first = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Save memories".into())
        .unwrap();
    let call = server.next().await;
    let mut facts: Vec<String> = (0..15)
        .map(|i| format!("critical|Unrelated historical artifact number {i}"))
        .collect();
    facts.push("low|The player prefers jasmine tea".to_owned());
    let response = json!({
        "segments": [{"text": "Saved"}],
        "memory_add": facts
    })
    .to_string();
    call.respond
        .send(Reply::Structured(Box::leak(response.into_boxed_str())))
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let all = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap()
        .memories;
    assert_eq!(all.len(), 16);
    let tea = all
        .iter()
        .find(|record| record.content.contains("tea"))
        .unwrap();
    assert_eq!(tea.priority, MemoryPriority::Low);

    let second = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Tell me about jasmine tea".into())
        .unwrap();
    let call = server.next().await;
    let prompt = call.body.to_string();
    assert!(prompt.contains("[LONG-TERM MEMORY]"));
    assert!(prompt.contains(&tea.id.0.to_string()));
    assert!(prompt.contains("The player prefers jasmine tea"));
    let visible = all
        .iter()
        .filter(|record| prompt.contains(&record.id.0.to_string()))
        .count();
    assert!(visible <= 12, "only bounded memory context is allowed");
    assert!(
        visible < all.len(),
        "not all memories may be sent to the provider"
    );
    let memory_block = call.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part["parts"][0]["text"].as_str())
        .find(|text| text.starts_with("[LONG-TERM MEMORY]"))
        .unwrap();
    assert!(memory_block.chars().count() <= 6_000);
    call.respond.send(Reply::Text("OK")).unwrap();
    assert!(matches!(
        finished(&second).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let third = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Remember jasmine tea again".into())
        .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"Already remembered"}],"memory_add":["critical|  the  PLAYER prefers jasmine TEA "]}"#
    )).unwrap();
    let accepted = finished(&third).await;
    let applied = match &accepted.state {
        RequestState::Finished(RequestOutcome::Success(response)) => response,
        state => panic!("unexpected result: {state:?}"),
    };
    assert_eq!(applied.effects.len(), 1);
    let after = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap()
        .memories;
    assert_eq!(after.len(), 16);
    let same_tea = after.iter().find(|record| record.id == tea.id).unwrap();
    assert_eq!(same_tea.priority, MemoryPriority::Critical);
    assert_eq!(same_tea.content, tea.content);

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_working_state_is_rejected_before_acceptance() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Try invalid".into())
        .unwrap();
    server.next().await.respond.send(Reply::Structured(
        r#"{"segments":[{"text":"Unsafe"}],"working_state":{"open_loops":["1","2","3","4","5","6","7"]}}"#
    )).unwrap();
    assert!(matches!(
        finished(&handle).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert_eq!(app.at().through, 1);
    assert!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(app.at())
            .await
            .unwrap()
            .working_state
            .is_empty()
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_structured_effect_fails_without_committing_or_synthesizing() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "act now".into())
        .unwrap();
    let call = server.next().await;
    call.respond
        .send(Reply::Structured(
            r#"{"segments":[{"text":"I will move","commands":["move"]}]}"#,
        ))
        .unwrap();

    assert!(matches!(
        finished(&handle).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert!(
        !app.timeline()
            .iter()
            .any(|event| { matches!(event.content, EventContent::CharacterReply { .. }) })
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_generation_pinned_to_a_reselected_branch_cannot_commit() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let pending = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "pinned".into())
        .unwrap();
    let active = server.next().await;
    let selected = app.timeline_selection();
    let changed = app
        .select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    assert!(changed.generation > selected.generation);
    assert!(matches!(
        finished(&pending).await.state,
        RequestState::Cancelled
    ));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert!(
        !app.timeline()
            .iter()
            .any(|event| { matches!(event.content, EventContent::CharacterReply { .. }) })
    );
    let fresh = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "new selection".into())
        .unwrap();
    let next = server.next().await;
    drop(active);
    next.respond.send(Reply::Text("current result")).unwrap();
    assert!(matches!(
        finished(&fresh).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_reaction_uses_newer_known_player_message_at_dispatch() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("first message".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let first_call = server.next().await;
    let cause = app
        .character(CharacterId::Crazy)
        .unwrap()
        .observe(UserInput::new("door opened".into()).unwrap())
        .await
        .unwrap();
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        cause,
        Arc::clone(UserInput::new("React to the door".into()).unwrap().text()),
        RequestSource::Presenter,
    ))
    .unwrap();
    let question = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Why did you open the door?".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    assert!(app.timeline().iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "Why did you open the door?")
    }));

    first_call
        .respond
        .send(Reply::Text("first answer"))
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let reaction_call = server.next().await;
    let user = user_texts(&reaction_call.body);
    assert!(user.contains(&"Why did you open the door?"));
    assert!(user.iter().any(|text| text.contains("React to the door")));
    assert!(
        user.iter()
            .any(|text| text.contains("отреагировать на отмеченное событие"))
    );
    assert_eq!(
        user.iter()
            .filter(|text| **text == "Why did you open the door?")
            .count(),
        1
    );
    reaction_call
        .respond
        .send(Reply::Text("because it was closed"))
        .unwrap();
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let question_call = server.next().await;
    assert_eq!(
        user_texts(&question_call.body)
            .iter()
            .filter(|text| text.contains("Why did you open the door?"))
            .count(),
        1
    );
    question_call
        .respond
        .send(Reply::Text("the door is open"))
        .unwrap();
    assert!(matches!(
        finished(&question).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_input_survives_selection_change_but_stale_generation_does_not_start() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("active request".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let blocked = server.next().await;
    let waiting = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("queued and already spoken".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let selected = app.timeline_selection();
    app.select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&waiting).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    let events = app.timeline();
    assert!(events.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "queued and already spoken")
    }));
    assert!(!events.iter().any(|event| {
        event.request_id == Some(waiting.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));
    drop(blocked);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_player_message_is_visible_without_scheduling_generation() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let input = UserInput::new("Message without an LLM request".into()).unwrap();
    let event_id = input.event_id();
    assert_eq!(
        ChatApi::publish_message(CharacterId::Crazy, input).unwrap(),
        event_id
    );
    ChatApi::publish_message(
        CharacterId::Kind,
        UserInput::new("Only KindMita can hear this".into()).unwrap(),
    )
    .unwrap();
    let world = app.timeline();
    assert_eq!(world.len(), 2);
    assert!(
        world
            .iter()
            .any(|event| event.id == event_id && event.request_id.is_none())
    );

    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("New question".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    assert_eq!(
        user_texts(&call.body),
        ["Message without an LLM request", "New question"]
    );
    assert!(
        !call
            .body
            .to_string()
            .contains("Only KindMita can hear this")
    );
    call.respond.send(Reply::Text("response")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn character_reset_cancels_only_its_running_generation_and_keeps_input_fact() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let mita = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Question before reset".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let mita_call = server.next().await;
    let kind = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Kind,
            UserInput::new("KindMita stays active".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let kind_call = server.next().await;

    let selected = app.timeline_selection();
    let reset_id = neuromita::timeline::EventId::new();
    assert_eq!(
        app.reset_character_context(selected, CharacterId::Crazy, reset_id),
        Ok(reset_id)
    );
    assert!(matches!(
        finished(&mita).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(kind.snapshot().state, RequestState::Processing(_)));
    assert_eq!(app.at().through, 3);
    assert_eq!(
        app.reset_character_context(selected, CharacterId::Crazy, reset_id),
        Ok(reset_id)
    );
    assert_eq!(app.at().through, 3);

    let mita_view = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert!(mita_view.view.messages().is_empty());
    assert_eq!(mita_view.state.accepted_replies, 0);

    kind_call
        .respond
        .send(Reply::Text("KindMita answered"))
        .unwrap();
    assert!(matches!(
        finished(&kind).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let old = app.timeline();
    assert!(old.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "Question before reset")
    }));
    assert!(!old.iter().any(|event| {
        event.request_id == Some(mita.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));

    let next = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("After reset".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let next_call = server.next().await;
    assert_eq!(user_texts(&next_call.body), ["After reset"]);
    next_call.respond.send(Reply::Text("New answer")).unwrap();
    assert!(matches!(
        finished(&next).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    drop(mita_call);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_reset_is_rejected_without_cancelling_current_request() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let selected = app.timeline_selection();
    app.select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Valid after reselection".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    let at = app.at();
    assert!(
        app.reset_character_context(
            selected,
            CharacterId::Crazy,
            neuromita::timeline::EventId::new()
        )
        .is_err()
    );
    assert_eq!(app.at(), at);
    assert!(matches!(
        reply.snapshot().state,
        RequestState::Processing(_)
    ));
    call.respond.send(Reply::Text("answer")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

struct PausedVoice(mpsc::UnboundedSender<oneshot::Sender<()>>);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_voice_discards_memory_state_and_working_state_before_the_next_turn() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let (voiced, mut voices) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.voice = Arc::new(PausedVoice(voiced));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(true)
        .unwrap();
    let request = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "first turn".into())
        .unwrap();
    server.next().await.respond.send(Reply::Structured(r#"{"segments":[{"text":"unaccepted reply"}],"attitude_change":2.0,"memory_add":["high|hidden-unaccepted-secret"],"working_state":{"focus":"discard this focus"}}"#)).unwrap();
    let release = timeout(Duration::from_secs(5), voices.recv())
        .await
        .unwrap()
        .unwrap();
    let before = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(before.state.accepted_replies, 0);
    assert_eq!(before.state.attitude, 60.0);
    assert!(before.memories.is_empty());
    assert!(before.working_state.is_empty());
    assert_eq!(request.cancel(), CancelResult::Requested);
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Cancelled
    ));
    assert!(release.is_closed());
    assert!(
        !app.timeline()
            .iter()
            .any(|event| event.request_id == Some(request.id())
                && matches!(event.content, EventContent::CharacterReply { .. }))
    );
    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(false)
        .unwrap();
    let next = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "next turn".into())
        .unwrap();
    let call = server.next().await;
    let prompt = call.body.to_string();
    assert!(!prompt.contains("hidden-unaccepted-secret"));
    assert!(!prompt.contains("unaccepted reply"));
    assert!(!prompt.contains("discard this focus"));
    call.respond
        .send(Reply::Text("accepted next turn"))
        .unwrap();
    assert!(matches!(
        finished(&next).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let after = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(after.state.accepted_replies, 1);
    assert_eq!(after.state.attitude, 60.0);
    assert!(after.memories.is_empty());
    assert!(after.working_state.is_empty());
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

impl neuromita::voice::VoiceProvider for PausedVoice {
    fn synthesize(
        &self,
        _text: Arc<str>,
        _options: neuromita::voice::VoiceOptions,
    ) -> neuromita::voice::VoiceFuture<'_> {
        Box::pin(async {
            let (send, receive) = oneshot::channel();
            self.0.send(send).unwrap();
            let _ = receive.await;
            Ok(neuromita::voice::AudioArtifact {
                format: neuromita::voice::AudioFormat::Wave,
                bytes: Arc::from([0u8; 4]),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_during_voice_discards_the_unaccepted_response() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let (voice_started, mut voice_calls) = mpsc::unbounded_channel();
    let mut config = ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.voice = Arc::new(PausedVoice(voice_started));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(true)
        .unwrap();
    app.presenter()
        .settings
        .voice
        .voice
        .set("voice-id".into())
        .unwrap();
    app.presenter()
        .settings
        .voice
        .volume
        .set(neuromita::settings::Volume::new(80).unwrap())
        .unwrap();
    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Question requiring voice".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let provider_call = server.next().await;
    provider_call
        .respond
        .send(Reply::Text("spoken reply"))
        .unwrap();
    let voice_release = timeout(Duration::from_secs(10), voice_calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    let reset_id = neuromita::timeline::EventId::new();
    assert_eq!(
        app.reset_character_context(app.timeline_selection(), CharacterId::Crazy, reset_id),
        Ok(reset_id)
    );
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Cancelled
    ));
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert!(voice_release.is_closed());
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_during_voice_discards_the_unaccepted_reply() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let (voice_started, mut voice_calls) = mpsc::unbounded_channel();
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.voice = Arc::new(PausedVoice(voice_started));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(true)
        .unwrap();
    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Voice must not hold the commit open".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    server
        .next()
        .await
        .respond
        .send(Reply::Text("accepted text"))
        .unwrap();
    let release = timeout(Duration::from_secs(10), voice_calls.recv())
        .await
        .unwrap()
        .unwrap();
    let stopping = tokio::spawn(async move { app.shutdown().await });
    let result = finished(&reply).await;
    assert!(matches!(result.state, RequestState::Cancelled));
    assert!(release.send(()).is_err());
    timeout(Duration::from_secs(10), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_cancels_queued_reaction_admitted_before_reset() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let cause = app
        .character(CharacterId::Crazy)
        .unwrap()
        .observe(UserInput::new("Known door opened".into()).unwrap())
        .await
        .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("First blocking request".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let blocked = server.next().await;
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        cause,
        Arc::clone(UserInput::new("React to door".into()).unwrap().text()),
        RequestSource::Presenter,
    ))
    .unwrap();
    let reset = neuromita::timeline::EventId::new();
    app.reset_character_context(app.timeline_selection(), CharacterId::Crazy, reset)
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    assert_eq!(app.at().through, 3);
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    drop(blocked);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn historical_reply_cannot_restore_knowledge_across_a_character_reset() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Private knowledge before reset".into()).unwrap(),
    )
    .unwrap();
    let old = app.at();
    app.reset_character_context(
        app.timeline_selection(),
        CharacterId::Crazy,
        neuromita::timeline::EventId::new(),
    )
    .unwrap();

    let stale = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::Crazy,
            neuromita::api::ChatApi::publish_message(
                CharacterId::Crazy,
                UserInput::new("Try old context".into()).unwrap(),
            )
            .unwrap(),
            RequestSource::Presenter,
        )
        .at(neuromita::timeline::ContextPosition::At(old)),
    )
    .unwrap();
    assert!(matches!(
        finished(&stale).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .get_context(old)
            .await
            .unwrap()
            .view
            .messages()[0]
            .content,
        "Private knowledge before reset"
    );

    let independent = ChatApi::reply(ReplyRequest::new(
        CharacterId::Kind,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Kind,
            UserInput::new("Your own earlier view".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    assert_eq!(user_texts(&call.body), ["Your own earlier view"]);
    assert!(
        !call
            .body
            .to_string()
            .contains("Private knowledge before reset")
    );
    call.respond
        .send(Reply::Text("independent answer"))
        .unwrap();
    assert!(matches!(
        finished(&independent).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_character_messages_use_shared_conversation_audience() {
    use neuromita::timeline::conversation::{
        ConversationAudience, ConversationMessage, ConversationSpeaker,
    };
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let generator = ResponseGenerator::new([
        Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
    ])
    .unwrap();
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        generator,
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let mut audience = ConversationAudience::private_to(CharacterId::Crazy);
    audience.seen_by.insert(CharacterId::Kind);
    audience.targets.extend([
        ConversationSpeaker::Player,
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);
    audience.participants.extend([
        ConversationSpeaker::Player,
        ConversationSpeaker::Character(CharacterId::Crazy),
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);

    let mut shared_input = ConversationAudience::player_to(CharacterId::Crazy);
    shared_input.seen_by.insert(CharacterId::Kind);
    shared_input
        .participants
        .insert(ConversationSpeaker::Character(CharacterId::Kind));

    let mut incoming = ConversationMessage::new(
        "shared-player-input",
        ConversationSpeaker::Player,
        json!("Say hello to Kind Mita"),
        [],
    );
    incoming.audience = shared_input;
    let message_id = incoming.id;
    app.publish_conversation_message(app.timeline_selection(), incoming)
        .unwrap();
    let reply = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::Crazy,
            message_id,
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("Hello, Kind Mita!")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let source = mita.view.provider_messages();
    let listener = kind.view.provider_messages();
    assert_eq!(source.len(), 2);
    assert_eq!(listener.len(), 2);
    assert_eq!(listener[0]["speaker"], "player");
    assert_eq!(listener[0]["targets"], json!(["Crazy"]));
    assert_eq!(
        listener[0]["timeline_event_id"],
        source[0]["timeline_event_id"]
    );
    assert_eq!(source[1]["role"], "assistant");
    assert_eq!(listener[1]["role"], "user");
    assert_eq!(listener[1]["speaker"], "Crazy");
    assert_eq!(
        source[1]["timeline_event_id"],
        listener[1]["timeline_event_id"]
    );
    assert_eq!(source[1]["targets"], json!(["player", "Kind"]));
    assert_eq!(
        listener[1]["participants"],
        json!(["player", "Crazy", "Kind"])
    );
    assert_eq!(
        listener[1]["content"],
        json!([{"text":"Hello, Kind Mita!"}])
    );

    let mut private = ConversationAudience::private_to(CharacterId::Kind);
    private
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Crazy));
    private.participants.extend([
        ConversationSpeaker::Character(CharacterId::Kind),
        ConversationSpeaker::Character(CharacterId::Crazy),
    ]);
    let private_reply = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::Kind,
            neuromita::api::ChatApi::publish_message(
                CharacterId::Kind,
                UserInput::new("A private reply".into()).unwrap(),
            )
            .unwrap(),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(private.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("Secret response")).unwrap();
    assert!(matches!(
        finished(&private_reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita_now = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind_now = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(mita_now.view.count(), 2);
    assert_eq!(kind_now.view.count(), 4);
    assert_eq!(
        kind_now.view.provider_messages()[3]["targets"],
        json!(["Crazy"])
    );
    assert_eq!(kind_now.view.provider_messages()[3]["role"], "assistant");

    let react = ChatApi::react(
        ReactRequest::new(
            CharacterId::Kind,
            neuromita::timeline::EventId(
                Uuid::parse_str(source[1]["timeline_event_id"].as_str().unwrap()).unwrap(),
            ),
            Arc::clone(UserInput::new("React to Mita".into()).unwrap().text()),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("I heard you!")).unwrap();
    assert!(matches!(
        finished(&react).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let now_mita = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let now_kind = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(now_mita.view.count(), 3);
    assert_eq!(now_kind.view.count(), 5);
    assert_eq!(now_mita.view.provider_messages()[2]["speaker"], "Kind");
    assert_eq!(now_kind.view.provider_messages()[4]["role"], "assistant");
    let initiate = ChatApi::initiate(
        neuromita::request::InitiateRequest::new(
            CharacterId::Crazy,
            Arc::clone(
                UserInput::new("Say something to both".into())
                    .unwrap()
                    .text(),
            ),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience),
    )
    .unwrap();
    let call = server.next().await;
    call.respond
        .send(Reply::Text("Everyone can hear me"))
        .unwrap();
    assert!(matches!(
        finished(&initiate).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita_final = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind_final = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(mita_final.view.count(), 4);
    assert_eq!(kind_final.view.count(), 6);
    assert_eq!(mita_final.view.provider_messages()[3]["role"], "assistant");
    assert_eq!(kind_final.view.provider_messages()[5]["role"], "user");

    let missing_speaker = ConversationAudience::private_to(CharacterId::Kind);
    let previous_head = app.at().through;
    let mut message = ConversationMessage::new(
        "player-unseen-by-mita",
        ConversationSpeaker::Player,
        json!("Mita cannot hear this"),
        [],
    );
    message.audience = missing_speaker;
    let message_id = message.id;
    app.publish_conversation_message(app.timeline_selection(), message)
        .unwrap();
    assert!(matches!(
        ChatApi::reply(ReplyRequest::new(
            CharacterId::Crazy,
            message_id,
            RequestSource::Client(Uuid::new_v4())
        )),
        Err(SubmitError::InvalidTargetEvent)
    ));
    assert_eq!(app.at().through, previous_head + 1);

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preset_is_resolved_only_when_queued_generation_begins() {
    use std::collections::BTreeMap;

    use neuromita::{
        generation::PresetCatalog,
        settings::{CharactersSettings, PresetId, PresetSelection, PresetSettings, SettingsApi},
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let named = |name: &str| {
        GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::new(
                &server.endpoint,
                name,
                "test-key",
                GenerationSettings::new(0.7, 512, Duration::from_secs(10)).unwrap(),
            )
            .unwrap(),
        ))
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        named("alpha"),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.presets = PresetCatalog::new(BTreeMap::from([
        (PresetId(0), named("alpha")),
        (PresetId(1), named("beta")),
    ]));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let first = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "one".into())
        .unwrap();
    let first_call = server.next().await;
    assert!(first_call.path.contains("models/alpha:generateContent"));
    let waiting = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "two".into())
        .unwrap();

    SettingsApi::edit::<PresetSettings>(|settings| {
        settings.active = PresetId(1);
    })
    .unwrap();
    SettingsApi::edit::<CharactersSettings>(|settings| {
        settings.crazy.presets.react = PresetSelection::Specific(PresetId(0));
    })
    .unwrap();

    first_call.respond.send(Reply::Text("first reply")).unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let second_call = server.next().await;
    assert!(second_call.path.contains("models/beta:generateContent"));
    second_call
        .respond
        .send(Reply::Text("second reply"))
        .unwrap();
    assert!(matches!(
        finished(&waiting).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("react to this".into()).unwrap(),
    )
    .unwrap();
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        event,
        "react",
        RequestSource::Presenter,
    ))
    .unwrap();
    let reaction_call = server.next().await;
    assert!(reaction_call.path.contains("models/alpha:generateContent"));
    reaction_call.respond.send(Reply::Text("reaction")).unwrap();
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    SettingsApi::edit::<CharactersSettings>(|settings| {
        settings.crazy.presets.reply = PresetSelection::Specific(PresetId(999));
    })
    .unwrap();
    let missing = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "missing".into())
        .unwrap();
    assert!(matches!(
        finished(&missing).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert!(server.calls.try_recv().is_err());

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_api_exposes_separate_catalog_crud_without_touching_settings() {
    use neuromita::{
        api::PresetApi,
        generation::{GeminiPresetDefinition, PresetCatalogError, PresetDefinition, PresetRecord},
        settings::SettingsApi,
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    assert!(matches!(
        PresetApi::list(),
        Err(PresetCatalogError::Unavailable)
    ));
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset("http://localhost:8800/v1beta/"),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let settings_before = SettingsApi::snapshot_all().unwrap();
    let make = |model: &str| PresetRecord {
        name: model.into(),
        definition: PresetDefinition::Gemini(GeminiPresetDefinition {
            options: Default::default(),
            transforms: Vec::new(),
            endpoint: "http://127.0.0.1:8800/v1beta/".into(),
            model: model.into(),
            credential: "EXAMPLE_GEMINI_KEY".into(),
            temperature: 0.7,
            max_output_tokens: 128,
            timeout_ms: 2000,
        }),
    };
    let id = PresetApi::create(make("first")).unwrap();
    assert_eq!(PresetApi::list().unwrap().len(), 1);
    PresetApi::update(id, make("second")).unwrap();
    assert_eq!(PresetApi::list().unwrap()[0].1.name, "second");
    assert_eq!(
        SettingsApi::snapshot_all().unwrap().revision,
        settings_before.revision
    );
    PresetApi::delete(id).unwrap();
    assert!(PresetApi::list().unwrap().is_empty());
    app.shutdown().await.unwrap();
    assert!(matches!(
        PresetApi::list(),
        Err(PresetCatalogError::Unavailable)
    ));
}

#[cfg(feature = "sqlite-system")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_bootstrap_and_preset_presenter_survive_restart() {
    use neuromita::{
        generation::{GeminiPresetDefinition, PresetDefinition, PresetRecord},
        settings::{PresetSelection, SettingsApi},
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let directory = std::env::temp_dir().join(format!("neuromita-catalog-{}", Uuid::new_v4()));
    let record = |model: &str| PresetRecord {
        name: model.to_owned(),
        definition: PresetDefinition::Gemini(GeminiPresetDefinition {
            options: Default::default(),
            transforms: Vec::new(),
            endpoint: "http://127.0.0.1:8100/v1beta/".to_owned(),
            model: model.to_owned(),
            credential: "EXAMPLE_GEMINI_KEY".into(),
            temperature: 0.7,
            max_output_tokens: 256,
            timeout_ms: 5000,
        }),
    };
    let endpoint = server.endpoint.clone();
    let setup = || {
        let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
        ApplicationConfig::persistent(
            preset(&endpoint),
            ResponseGenerator::new([provider]).unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &directory,
        )
        .unwrap()
    };

    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    let original_branch = app.at().branch_id;
    let request = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "Remember tea".into())
        .unwrap();
    server.next().await.respond.send(Reply::Structured(r#"{"segments":[{"text":"Remembered tea"}],"attitude_change":1.0,"memory_add":["high|Player likes green tea"]}"#)).unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let presets = &app.presenter().presets;
    let mut changes = presets.observe();
    assert_eq!(changes.borrow_and_update().revision, 0);
    let initial_settings_revision = SettingsApi::snapshot_all().unwrap().revision;
    let id = presets.create(record("first-model")).unwrap();
    changes.changed().await.unwrap();
    assert_eq!(changes.borrow_and_update().entries[&id].name, "first-model");
    assert_eq!(
        SettingsApi::snapshot_all().unwrap().revision,
        initial_settings_revision
    );
    presets.update(id, record("second-model")).unwrap();
    changes.changed().await.unwrap();
    assert_eq!(
        changes.borrow_and_update().entries[&id].name,
        "second-model"
    );
    app.presenter()
        .settings
        .characters
        .for_character(CharacterId::Crazy)
        .reply
        .set(PresetSelection::Specific(id))
        .unwrap();
    app.presenter().settings.presets.active.set(id).unwrap();
    app.shutdown().await.unwrap();

    assert!(directory.join("settings.json").exists());
    let stored = std::fs::read_to_string(directory.join("presets.json")).unwrap();
    assert!(stored.contains("second-model"));
    assert!(stored.contains("EXAMPLE_GEMINI_KEY"));
    assert!(!stored.contains("test-key"));

    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    assert_eq!(app.at().branch_id, original_branch);
    let restored = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(restored.state.accepted_replies, 1);
    assert_eq!(restored.state.attitude, 61.0);
    assert_eq!(restored.memories.len(), 1);
    assert_eq!(restored.memories[0].content, "Player likes green tea");
    assert_eq!(app.presenter().presets.list()[0].0, id);
    assert_eq!(app.presenter().presets.list()[0].1.name, "second-model");
    assert_eq!(app.presenter().settings.presets.active.get(), id);
    assert_eq!(
        app.presenter()
            .settings
            .characters
            .for_character(CharacterId::Crazy)
            .reply
            .get(),
        PresetSelection::Specific(id)
    );
    app.presenter().presets.delete(id).unwrap();
    app.shutdown().await.unwrap();
    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    assert!(app.presenter().presets.list().is_empty());
    assert_eq!(app.presenter().settings.presets.active.get(), id);
    app.shutdown().await.unwrap();
    server.shutdown().await;
    std::fs::remove_dir_all(&directory).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unconfigured_core_administers_settings_and_reloads_without_fallback() {
    use neuromita::{
        generation::{
            GeminiPresetDefinition, PresetCatalogError, PresetDefinition, PresetId, PresetRecord,
        },
        settings::{PresetSelection, Volume},
    };
    let _serial = CORE_TEST_MUTEX.lock().await;
    let directory = std::env::temp_dir().join(format!("neuromita-unconfigured-{}", Uuid::new_v4()));
    let setup = || {
        ApplicationConfig::persistent_unconfigured(
            ResponseGenerator::new([
                Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
            ])
            .unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &directory,
        )
        .unwrap()
    };
    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    let view = app
        .presenter()
        .conversation
        .snapshot(CharacterId::Crazy)
        .await
        .unwrap();
    assert!(view.messages.is_empty());
    assert!(matches!(
        view.model,
        Err(PresetCatalogError::NotFound(PresetId(0)))
    ));
    assert!(app.presenter().presets.require(PresetId(0)).is_err());
    let id = app
        .presenter()
        .presets
        .create(PresetRecord {
            name: "Explicit".into(),
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                options: Default::default(),
                transforms: Vec::new(),
                endpoint: "http://127.0.0.1:1/v1beta/".into(),
                model: "explicit-model".into(),
                credential: "NEUROMITA_MISSING_FIXTURE_CREDENTIAL_42".into(),
                temperature: 0.7,
                max_output_tokens: 100,
                timeout_ms: 1000,
            }),
        })
        .unwrap();
    app.presenter().presets.require(id).unwrap();
    app.presenter().settings.presets.active.set(id).unwrap();
    app.presenter()
        .settings
        .characters
        .for_character(CharacterId::Kind)
        .reply
        .set(PresetSelection::Specific(id))
        .unwrap();
    app.presenter()
        .settings
        .voice
        .volume
        .set(Volume::new(37).unwrap())
        .unwrap();
    let view = app
        .presenter()
        .conversation
        .snapshot(CharacterId::Crazy)
        .await
        .unwrap();
    assert!(matches!(
        view.model,
        Err(PresetCatalogError::MissingCredential(_))
    ));
    let mut connections = app.presenter().connections.observe();
    connections.borrow_and_update();
    let client = TcpStream::connect(app.server().address()).await.unwrap();
    timeout(Duration::from_secs(2), connections.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connections.borrow_and_update().connected_clients, 1);
    drop(client);
    app.shutdown().await.unwrap();
    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    assert_eq!(app.presenter().settings.voice.volume.get().get(), 37);
    assert_eq!(
        app.presenter()
            .settings
            .characters
            .for_character(CharacterId::Kind)
            .reply
            .get(),
        PresetSelection::Specific(id)
    );
    app.presenter().presets.delete(id).unwrap();
    assert_eq!(app.presenter().settings.presets.active.get(), id);
    assert!(
        matches!(app.presenter().conversation.snapshot(CharacterId::Crazy).await.unwrap().model, Err(PresetCatalogError::NotFound(value)) if value == id)
    );
    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_clear_preview_is_guarded_and_does_not_clear_other_characters() {
    use neuromita::{
        presentation::{ConversationChange, ConversationError},
        request::UserInput,
    };
    let _serial = CORE_TEST_MUTEX.lock().await;
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset("http://127.0.0.1:1/v1beta/"),
        ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let api = app.presenter();
    api.characters
        .observe(CharacterId::Crazy, UserInput::new("first".into()).unwrap())
        .await
        .unwrap();
    api.characters
        .observe(CharacterId::Kind, UserInput::new("private".into()).unwrap())
        .await
        .unwrap();
    let stale = api
        .conversation
        .preview_change(CharacterId::Crazy, ConversationChange::ClearMessages)
        .await
        .unwrap();
    api.characters
        .observe(CharacterId::Crazy, UserInput::new("second".into()).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        api.conversation.apply_change(stale),
        Err(ConversationError::StalePreview)
    ));
    let preview = api
        .conversation
        .preview_change(CharacterId::Crazy, ConversationChange::ClearMessages)
        .await
        .unwrap();
    assert_eq!(preview.message_count(), 2);
    api.conversation.apply_change(preview).unwrap();
    assert!(
        api.conversation
            .snapshot(CharacterId::Crazy)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        api.conversation
            .snapshot(CharacterId::Kind)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    let reset = api
        .conversation
        .preview_change(CharacterId::Crazy, ConversationChange::ResetContext)
        .await
        .unwrap();
    api.conversation.apply_change(reset).unwrap();
    app.shutdown().await.unwrap();
}
