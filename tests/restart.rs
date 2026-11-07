use std::{
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{
        ApplicationConfig, ApplicationError, ApplicationLifecycle, ApplicationStatus, RuntimeLimits,
    },
    character::CharacterId,
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{
        DiscoveryError, ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState,
        SubmitError, UserInput,
    },
    settings::{
        MemorySettingsRepository, SettingsApi, SettingsConfig, SettingsError, SettingsRepository,
        SettingsSource, StoredSettings, VoiceSettings, Volume,
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    time::timeout,
};

type Gate = oneshot::Sender<oneshot::Sender<()>>;

struct Storage {
    inner: MemorySettingsRepository,
    save_gate: Mutex<Option<Gate>>,
    load_gate: Mutex<Option<Gate>>,
    fail: AtomicBool,
}

impl Storage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemorySettingsRepository::new(),
            save_gate: Mutex::new(None),
            load_gate: Mutex::new(None),
            fail: AtomicBool::new(false),
        })
    }
    fn gate(slot: &Mutex<Option<Gate>>) -> oneshot::Receiver<oneshot::Sender<()>> {
        let (entered, receive) = oneshot::channel();
        *slot.lock().unwrap() = Some(entered);
        receive
    }
    fn block(slot: &Mutex<Option<Gate>>) {
        let gate = slot.lock().unwrap().take();
        if let Some(entered) = gate {
            let (release, wait) = oneshot::channel();
            if entered.send(release).is_ok() {
                let _ = wait.blocking_recv();
            }
        }
    }
}

impl SettingsRepository for Storage {
    fn migrate(&self) -> Result<(), SettingsError> {
        self.inner.migrate()
    }
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        Self::block(&self.load_gate);
        self.inner.load()
    }
    fn save(&self, value: &StoredSettings) -> Result<(), SettingsError> {
        Self::block(&self.save_gate);
        if self.fail.load(Ordering::Acquire) {
            return Err(SettingsError::Storage("injected save failure".into()));
        }
        self.inner.save(value)
    }
}

struct Call {
    context: neuromita::generation::GenerationInput,
    model: String,
    reply: oneshot::Sender<Arc<str>>,
}

struct ControlledProvider(mpsc::UnboundedSender<Call>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(
        &self,
        context: neuromita::generation::GenerationInput,
        template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async move {
            let ProviderTemplate::Gemini(template) = template else {
                panic!("fixture requires Gemini")
            };
            let (reply, receive) = oneshot::channel();
            self.0
                .send(Call {
                    context,
                    model: template.model().to_owned(),
                    reply,
                })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture dropped".into()))
                .map(|text| neuromita::generation::StructuredResponse::spoken(text.to_string()))
        })
    }
}

fn preset(model: &str) -> GenerationPreset {
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            model,
            "fixture-key",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ))
}

fn config(
    model: &str,
    calls: &mpsc::UnboundedSender<Call>,
    storage: &Arc<Storage>,
    source: SettingsSource,
    address: SocketAddr,
) -> ApplicationConfig {
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(calls.clone()));
    let mut config = ApplicationConfig::minimal(
        preset(model),
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(16).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(16).unwrap(),
            retained_finished: NonZeroUsize::new(8).unwrap(),
        },
        address,
    );
    config.settings = SettingsConfig {
        source,
        repository: storage.clone(),
    };
    config
}

fn initial() -> SettingsSource {
    SettingsSource::initial(neuromita::settings::AppSettings::initial_profile())
}

fn reply(_model: &str, text: &str) -> Result<RequestHandle, SubmitError> {
    let event = ChatApi::publish_message(CharacterId::Crazy, UserInput::new(text.into()).unwrap())?;
    ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        event,
        RequestSource::Presenter,
    ))
}

async fn next(calls: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn finished(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

async fn stopped() {
    timeout(Duration::from_secs(5), ApplicationLifecycle::wait_stopped())
        .await
        .unwrap();
    assert_eq!(ApplicationLifecycle::status(), ApplicationStatus::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_apis_restart_without_crossing_application_lifetimes() {
    let address: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let storage = Storage::new();
    let (sent, mut calls) = mpsc::unbounded_channel();
    assert!(matches!(
        reply("first", "before start"),
        Err(SubmitError::Uninitialized)
    ));
    let app = ApplicationLifecycle::start(config("first", &sent, &storage, initial(), address))
        .await
        .unwrap();
    assert_eq!(ApplicationLifecycle::status(), ApplicationStatus::Running);
    assert!(matches!(
        ApplicationLifecycle::start(config("unused", &sent, &storage, initial(), address)).await,
        Err(ApplicationError::Busy)
    ));
    let old_anchor = app.at();
    let old_chat = app.presenter().chat.clone();
    let old_characters = app.presenter().characters.clone();
    let old_volume = app.presenter().settings.voice.volume.clone();
    let old_character = CharacterApi::get(CharacterId::Crazy).unwrap();
    let old_server = app.server().clone();
    let listen = old_server.address();
    let mut old_settings = SettingsApi::observe().unwrap();

    let accepted = reply("first", "accepted before restart").unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.context.history.events.len(), 1);
    assert_eq!(call.context.view.count(), 1);
    call.reply
        .send(Arc::from("first accepted response"))
        .unwrap();
    assert!(matches!(
        finished(&accepted).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert_eq!(old_character.state().await.unwrap().accepted_replies, 1);
    SettingsApi::flush().await.unwrap();

    let mut old_discovery = ChatApi::observe_requests().unwrap();
    let active = reply("first", "cancel running").unwrap();
    let call = next(&mut calls).await;
    let pending = reply("first", "cancel queued").unwrap();
    let gate = Storage::gate(&storage.save_gate);
    old_volume.set(Volume::new(42).unwrap()).unwrap();
    let release = timeout(Duration::from_secs(5), gate)
        .await
        .unwrap()
        .unwrap();
    let stopping = tokio::spawn(app.shutdown());
    timeout(
        Duration::from_secs(5),
        old_settings.wait_for(|snapshot| snapshot.closed),
    )
    .await
    .unwrap()
    .unwrap();
    timeout(Duration::from_secs(5), async {
        while ApplicationLifecycle::status() != ApplicationStatus::Stopping {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        reply("first", "during shutdown"),
        Err(SubmitError::Stopped)
    ));
    assert!(matches!(
        SettingsApi::snapshot::<VoiceSettings>(),
        Err(SettingsError::Stopped)
    ));
    assert!(CharacterApi::get(CharacterId::Crazy).is_err());
    let concurrent_rejections = tokio::task::spawn_blocking(|| {
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for _ in 0..64 {
                        assert!(matches!(
                            reply("first", "must reject during stopping"),
                            Err(SubmitError::Stopped)
                        ));
                        assert!(matches!(
                            SettingsApi::snapshot::<VoiceSettings>(),
                            Err(SettingsError::Stopped)
                        ));
                        assert!(CharacterApi::get(CharacterId::Crazy).is_err());
                    }
                });
            }
        });
    });
    concurrent_rejections.await.unwrap();
    assert!(matches!(
        ApplicationLifecycle::start(config("unused", &sent, &storage, initial(), address)).await,
        Err(ApplicationError::Busy)
    ));
    stopping.abort();
    assert!(stopping.await.unwrap_err().is_cancelled());
    assert_eq!(ApplicationLifecycle::status(), ApplicationStatus::Stopping);
    release.send(()).unwrap();
    stopped().await;

    assert!(call.reply.is_closed());
    assert!(matches!(
        finished(&active).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&pending).await.state,
        RequestState::Cancelled
    ));
    assert!(old_chat.snapshot().closed);
    assert!(
        old_chat
            .snapshot()
            .requests
            .iter()
            .all(|row| row.snapshot.state.is_terminal())
    );
    assert!(!old_server.snapshot().running);
    assert_eq!(old_server.snapshot().connected_clients, 0);
    assert!(old_character.state().await.is_err());
    assert!(matches!(
        old_volume.observe().next().await,
        Err(SettingsError::Stopped)
    ));
    let mut discovered = Vec::new();
    timeout(Duration::from_secs(5), async {
        loop {
            match old_discovery.next().await {
                Ok(handle) => discovered.push(handle.id()),
                Err(DiscoveryError::Closed) => break,
                Err(error) => panic!("unexpected discovery error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(discovered.contains(&active.id()) && discovered.contains(&pending.id()));

    let app = ApplicationLifecycle::start(config(
        "second",
        &sent,
        &storage,
        SettingsSource::Stored,
        listen,
    ))
    .await
    .unwrap();
    assert_eq!(app.server().address(), listen);
    assert_ne!(app.at().branch_id, old_anchor.branch_id);
    assert!(app.timeline().is_empty());
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert_eq!(
        SettingsApi::snapshot::<VoiceSettings>()
            .unwrap()
            .value
            .volume
            .get(),
        42
    );
    assert!(ChatApi::active_request(accepted.id()).unwrap().is_none());
    assert!(ChatApi::trace(accepted.id()).unwrap().is_none());
    assert!(
        old_chat
            .reply(CharacterId::Crazy, "stale presenter".into())
            .is_err()
    );
    assert!(old_chat.cancel(active.id()).is_err());
    assert!(matches!(
        old_volume.set(Volume::new(99).unwrap()),
        Err(SettingsError::Stopped)
    ));
    assert!(
        old_characters
            .observe(
                CharacterId::Crazy,
                UserInput::new("stale observation".into()).unwrap()
            )
            .await
            .is_err()
    );
    assert_eq!(
        SettingsApi::snapshot::<VoiceSettings>()
            .unwrap()
            .value
            .volume
            .get(),
        42
    );

    let request = reply("second", "new application").unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.model, "second");
    assert_eq!(call.context.history.events.len(), 1);
    assert_eq!(call.context.view.count(), 1);
    call.reply
        .send(Arc::from("second accepted response"))
        .unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let app = app
        .restart(config(
            "third",
            &sent,
            &storage,
            SettingsSource::Stored,
            listen,
        ))
        .await
        .unwrap();
    assert!(app.timeline().is_empty());
    let request = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "third application".into())
        .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.model, "third");
    drop(app);
    stopped().await;
    assert!(call.reply.is_closed());
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Cancelled
    ));

    let gate = Storage::gate(&storage.load_gate);
    let starting = tokio::spawn(ApplicationLifecycle::start(config(
        "cancelled-start",
        &sent,
        &storage,
        SettingsSource::Stored,
        listen,
    )));
    let release = timeout(Duration::from_secs(5), gate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ApplicationLifecycle::status(), ApplicationStatus::Starting);
    assert!(matches!(
        reply("unused", "during startup"),
        Err(SubmitError::Stopped)
    ));
    assert!(matches!(
        SettingsApi::snapshot::<VoiceSettings>(),
        Err(SettingsError::Stopped)
    ));
    assert!(CharacterApi::get(CharacterId::Crazy).is_err());
    assert!(matches!(
        ApplicationLifecycle::start(config("unused", &sent, &storage, initial(), address)).await,
        Err(ApplicationError::Busy)
    ));
    starting.abort();
    assert!(starting.await.err().unwrap().is_cancelled());
    stopped().await;
    release.send(()).unwrap();

    let occupied = TcpListener::bind(listen).await.unwrap();
    assert!(matches!(
        ApplicationLifecycle::start(config(
            "failed-bind",
            &sent,
            &storage,
            SettingsSource::Stored,
            listen
        ))
        .await,
        Err(ApplicationError::Server(_))
    ));
    stopped().await;
    assert!(matches!(
        ChatApi::observe_requests(),
        Err(SubmitError::Stopped)
    ));
    drop(occupied);

    let (left, right) = tokio::join!(
        ApplicationLifecycle::start(config(
            "concurrent-one",
            &sent,
            &storage,
            SettingsSource::Stored,
            listen
        )),
        ApplicationLifecycle::start(config(
            "concurrent-two",
            &sent,
            &storage,
            SettingsSource::Stored,
            listen
        )),
    );
    let app = match (left, right) {
        (Ok(app), Err(ApplicationError::Busy)) | (Err(ApplicationError::Busy), Ok(app)) => app,
        _ => panic!("exactly one application must start"),
    };
    SettingsApi::flush().await.unwrap();
    storage.fail.store(true, Ordering::Release);
    SettingsApi::edit::<VoiceSettings>(|voice| voice.volume = Volume::new(43).unwrap()).unwrap();
    assert!(matches!(
        app.shutdown().await,
        Err(ApplicationError::Settings(SettingsError::Storage(_)))
    ));
    stopped().await;
    storage.fail.store(false, Ordering::Release);

    let app = ApplicationLifecycle::start(config(
        "after-save-failure",
        &sent,
        &storage,
        SettingsSource::Stored,
        listen,
    ))
    .await
    .unwrap();
    let (read, mut write) = TcpStream::connect(app.server().address())
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(line.contains("connected"));
    let command = serde_json::json!({
        "command_id": uuid::Uuid::new_v4(),
        "command": "initiate",
        "character": "Crazy",
    });
    write
        .write_all(format!("{command}\n").as_bytes())
        .await
        .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.model, "after-save-failure");
    assert!(call.context.current_input.is_empty());
    call.reply.send(Arc::from("network after restart")).unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            if frame["type"] == "request_status" && frame["state"] == "finished" {
                break;
            }
        }
    })
    .await
    .unwrap();
    app.shutdown().await.unwrap();
    stopped().await;
}
