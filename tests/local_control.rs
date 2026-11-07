use std::{future::Future, net::SocketAddr, num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    game_link::GameLinkConfig,
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::UserInput,
    request::{RequestHandle, RequestId, RequestOutcome, RequestSource, RequestState},
    server::{ServerConfig, ServerHandle, ServerLifecycle, ServerSnapshot},
    settings::{SettingsApi, VoiceSettings, Volume},
    voice::{AudioArtifact, AudioFormat, VoiceFuture, VoiceOptions, VoiceProvider},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, MutexGuard, mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

static APPLICATION: Mutex<()> = Mutex::const_new(());

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("contract operation timed out")
}

struct GenerationCall {
    context: neuromita::generation::GenerationInput,
    reply: oneshot::Sender<Result<Arc<str>, GenerationError>>,
}

impl GenerationCall {
    fn finish(self, text: &str) {
        self.reply
            .send(Ok(Arc::from(text)))
            .expect("generation still running");
    }
}

struct ControlledGeneration(mpsc::UnboundedSender<GenerationCall>);

impl GenerationProvider for ControlledGeneration {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: neuromita::generation::GenerationInput,
        _: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(GenerationCall { context, reply })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))?
                .map(|text| StructuredResponse::spoken(text.to_string()))
        })
    }
}

struct FixtureVoice;
impl VoiceProvider for FixtureVoice {
    fn synthesize(&self, _text: Arc<str>, _options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async {
            Ok(AudioArtifact {
                format: AudioFormat::Wave,
                bytes: Arc::from(b"RIFF-fixture-wave".as_slice()),
            })
        })
    }
}

struct Fixture {
    app: ApplicationLifecycle,
    calls: mpsc::UnboundedReceiver<GenerationCall>,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    async fn start(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::with_queue(8, configure).await
    }

    async fn with_queue(queue: usize, configure: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::with_voice(queue, configure, false).await
    }

    async fn with_voice(
        queue: usize,
        configure: impl FnOnce(&mut ServerConfig),
        enabled: bool,
    ) -> Self {
        let serial = APPLICATION.lock().await;
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::google(
                "fixture",
                "unused-fixture-key",
                GenerationSettings::new(0.5, 32, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        let (send, calls) = mpsc::unbounded_channel();
        let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledGeneration(send));
        let mut config = ApplicationConfig::minimal(
            preset,
            ResponseGenerator::new([provider]).unwrap(),
            RuntimeLimits {
                request_queue: positive(queue),
                discovery_buffer: positive(64),
                character_commands: positive(32),
                presenter_updates: positive(64),
                retained_finished: positive(32),
            },
            "127.0.0.1:0".parse().unwrap(),
        );
        configure(&mut config.server);
        if enabled {
            config.voice = Arc::new(FixtureVoice);
        }
        config.game_link = Some(GameLinkConfig::new("127.0.0.1:0".parse().unwrap()));
        let app = bounded(ApplicationLifecycle::start(config)).await.unwrap();
        bounded(SettingsApi::flush()).await.unwrap();
        if enabled {
            SettingsApi::edit::<VoiceSettings>(|voice| voice.in_game = true).unwrap();
        }
        Self {
            app,
            calls,
            _serial: serial,
        }
    }

    async fn connect(&self) -> Client {
        Client::connect(self.app.server().address()).await
    }

    async fn call(&mut self) -> GenerationCall {
        bounded(self.calls.recv())
            .await
            .expect("provider fixture closed")
    }

    async fn shutdown(self) {
        bounded(self.app.shutdown()).await.unwrap();
    }
}

fn positive(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

struct Client {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    id: Uuid,
    initial_settings: Value,
}

impl Client {
    async fn connect(address: SocketAddr) -> Self {
        let stream = bounded(TcpStream::connect(address)).await.unwrap();
        let (read, write) = stream.into_split();
        let mut client = Self {
            read: BufReader::new(read),
            write,
            id: Uuid::nil(),
            initial_settings: Value::Null,
        };
        let connected = client.next().await;
        assert_eq!(connected["type"], "connected");
        client.id = uuid(&connected["client_id"]);
        client.initial_settings = client.next().await;
        assert_eq!(client.initial_settings["type"], "settings");
        client
    }

    async fn bytes(&mut self, bytes: &[u8]) {
        bounded(self.write.write_all(bytes)).await.unwrap();
    }

    async fn send(&mut self, command: Value) {
        self.bytes(&line(command)).await;
    }

    async fn next(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            bounded(self.read.read_line(&mut line)).await.unwrap(),
            0,
            "unexpected EOF"
        );
        serde_json::from_str(&line).expect("server JSON frame")
    }

    async fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        bounded(async {
            loop {
                let frame = self.next().await;
                if predicate(&frame) {
                    return frame;
                }
            }
        })
        .await
    }

    async fn accepted(&mut self, command_id: Uuid) -> Value {
        self.until(|v| v["type"] == "accepted" && v["command_id"] == command_id.to_string())
            .await
    }

    async fn terminal(&mut self, request_id: Uuid) -> Value {
        self.until(|v| {
            v["type"] == "request_status"
                && v["request_id"] == request_id.to_string()
                && (v["state"] == "finished" || v["state"] == "cancelled")
        })
        .await
    }

    async fn error(&mut self, message: &str) -> Value {
        let frame = self.until(|v| v["type"] == "error").await;
        assert!(
            frame["error"].as_str().unwrap().contains(message),
            "{frame}"
        );
        assert!(
            frame["code"].is_string(),
            "missing stable error code: {frame}"
        );
        frame
    }

    async fn closed(&mut self) {
        bounded(async {
            loop {
                let mut line = String::new();
                match self.read.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let frame: Value = serde_json::from_str(&line).unwrap();
                        assert_eq!(
                            frame["type"], "settings",
                            "unexpected frame before close: {frame}"
                        );
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break;
                    }
                    result => panic!("expected closed connection, got {result:?}: {line}"),
                }
            }
        })
        .await;
    }
}

fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn command(name: &str) -> Value {
    json!({"command_id": Uuid::new_v4(), "command": name})
}
fn reply(id: Uuid, character: &str, text: &str) -> Value {
    let message = CharacterId::parse(character)
        .and_then(|character| {
            UserInput::new(text.to_owned())
                .ok()
                .map(|input| ChatApi::publish_message(character, input).unwrap().0)
        })
        .unwrap_or_else(Uuid::new_v4);
    json!({"command_id": id, "command": "reply", "character": character, "message_id": message})
}
fn cancel(id: Uuid) -> Value {
    json!({"command_id": Uuid::new_v4(), "command": "cancel", "request_id": id})
}
fn uuid(value: &Value) -> Uuid {
    Uuid::parse_str(value.as_str().unwrap()).unwrap()
}
fn active(id: Uuid) -> RequestHandle {
    ChatApi::active_request(RequestId(id)).unwrap().unwrap()
}

#[tokio::test]
async fn voice_failure_keeps_the_accepted_message_identity_in_status_and_replay() {
    let mut fixture = Fixture::start(|_| {}).await;
    SettingsApi::edit::<VoiceSettings>(|voice| voice.in_launcher = true).unwrap();
    let mut client = fixture.connect().await;
    let command_id = Uuid::new_v4();
    let request = reply(command_id, "Crazy", "reply with unavailable voice");
    client.send(request.clone()).await;
    let accepted = client.accepted(command_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let handle = active(request_id);
    fixture
        .call()
        .await
        .finish("Accepted text despite voice failure");
    let final_snapshot = bounded(handle.wait()).await.unwrap();
    let RequestState::Finished(RequestOutcome::VoiceGenerationFailed { response, .. }) =
        &final_snapshot.state
    else {
        panic!("expected a committed reply with a voice failure");
    };
    assert!(response.audio.is_none());
    let terminal = client.terminal(request_id).await;
    assert_eq!(terminal["outcome"], "voice_error");
    assert_eq!(terminal["text"], response.text.as_ref());
    assert_eq!(terminal["event_id"], json!(response.event_id));
    assert_eq!(terminal["can_cancel"], false);
    assert!(terminal.get("provisional").is_none());
    assert!(terminal.get("audio").is_none());

    client.send(request).await;
    let replay = client.accepted(command_id).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.terminal(request_id).await, terminal);
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

async fn server_state(server: &ServerHandle, predicate: impl Fn(&ServerSnapshot) -> bool) {
    let mut changes = server.observe();
    bounded(async {
        loop {
            if predicate(&changes.borrow_and_update()) {
                return;
            }
            changes.changed().await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn discovery_matches_executable_commands_and_all_control_replies_are_correlated() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    assert!(client.initial_settings.get("command_id").is_none());
    let methods = command("methods");
    client.send(methods.clone()).await;
    let catalog = client.next().await;
    assert_eq!(catalog["type"], "methods");
    assert_eq!(catalog["command_id"], methods["command_id"]);
    let expected = [
        "ping",
        "methods",
        "server_status",
        "publish_message",
        "reply",
        "react",
        "initiate",
        "cancel",
        "diagnostics",
        "diagnostics_recent",
        "diagnostics_errors",
        "settings",
        "set_volume",
    ];
    assert_eq!(catalog["methods"], json!(expected));
    for name in expected {
        let mut cmd = command(name);
        match name {
            "reply" => {
                cmd = reply(uuid(&cmd["command_id"]), "Crazy", "catalog execution");
            }
            "publish_message" => {
                cmd["event_id"] = json!(Uuid::new_v4());
                cmd["occurred_at_unix_ms"] = json!(123);
                cmd["speaker"] = json!("player");
                cmd["content"] = json!("network publication");
                cmd["seen_by"] = json!(["Crazy"]);
            }
            "react" | "initiate" => {
                cmd["character"] = json!("Crazy");
                cmd["instruction"] = json!("catalog execution");
                if name == "react" {
                    cmd["event_id"] =
                        reply(Uuid::new_v4(), "Crazy", "reaction target")["message_id"].clone();
                }
            }
            "cancel" => {
                let mut start = command("initiate");
                start["character"] = json!("Crazy");
                client.send(start.clone()).await;
                let accepted = client.accepted(uuid(&start["command_id"])).await;
                let call = fixture.call().await;
                cmd["request_id"] = accepted["request_id"].clone();
                client.send(cmd.clone()).await;
                let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
                assert_eq!(response["type"], "cancel");
                assert_eq!(response["result"], "Requested");
                client.terminal(uuid(&accepted["request_id"])).await;
                drop(call);
                continue;
            }
            "diagnostics" => {
                cmd["character"] = json!("Crazy");
                cmd["request_id"] = json!(Uuid::new_v4());
                client.send(cmd.clone()).await;
                let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
                assert_eq!(response["type"], "error");
                assert_eq!(response["code"], "diagnostics_not_found");
                continue;
            }
            "diagnostics_recent" | "diagnostics_errors" => {
                cmd["character"] = json!("Crazy");
            }
            "set_volume" => cmd["value"] = json!(42),
            _ => {}
        }
        client.send(cmd.clone()).await;
        let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
        let response_type = match name {
            "ping" => "pong",
            "reply" | "react" | "initiate" => "accepted",
            "publish_message" => "published",
            "set_volume" => "settings_applied",
            other => other,
        };
        assert_eq!(response["type"], response_type, "{name}: {response}");
        if matches!(name, "reply" | "react" | "initiate") {
            let id = uuid(&response["request_id"]);
            fixture.call().await.finish("catalog result");
            assert_eq!(client.terminal(id).await["text"], "catalog result");
        }
    }
    let first = command("settings");
    let mut change = command("set_volume");
    change["value"] = json!(43);
    let last = command("settings");
    let mut pipeline = line(first.clone());
    pipeline.extend(line(change.clone()));
    pipeline.extend(line(last.clone()));
    client.bytes(&pipeline).await;
    let mut responses = Vec::new();
    let mut observed = false;
    bounded(async {
        while responses.len() < 3 || !observed {
            let frame = client.next().await;
            if frame.get("command_id").is_some() {
                responses.push(frame);
            } else if frame["type"] == "settings" && frame["settings"]["voice"]["volume"] == 43 {
                observed = true;
            }
        }
    })
    .await;
    assert_eq!(responses[0]["command_id"], first["command_id"]);
    assert_eq!(responses[0]["settings"]["voice"]["volume"], 42);
    assert_eq!(responses[1]["command_id"], change["command_id"]);
    assert_eq!(responses[1]["type"], "settings_applied");
    assert_eq!(responses[2]["command_id"], last["command_id"]);
    assert_eq!(responses[2]["settings"]["voice"]["volume"], 43);
    let instant = json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"});
    client.send(instant.clone()).await;
    fixture.call().await.finish("immediate result");
    let accepted = client.until(|v| v["type"] != "settings").await;
    assert_eq!(
        accepted["type"], "accepted",
        "status must follow acceptance"
    );
    assert_eq!(accepted["command_id"], instant["command_id"]);
    let finished = client.terminal(uuid(&accepted["request_id"])).await;
    client.send(instant.clone()).await;
    let replay = client.until(|v| v["type"] != "settings").await;
    assert_eq!(replay["type"], "accepted");
    assert_eq!(replay["command_id"], instant["command_id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.next().await, finished);
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn errors_have_stable_codes_recoverable_ids_and_no_mutations() {
    let mut fixture = Fixture::start(|s| s.receipt_capacity = positive(1)).await;
    let mut client = fixture.connect().await;
    let id = Uuid::new_v4();
    let cases = [
        (b"{\n".to_vec(), "malformed_json", None),
        (line(json!({"command_id": id, "command":"unknown"})), "unknown_command", Some(id)),
        (line(json!({"command_id": id, "command":"set_volume", "value":101})), "invalid_command", Some(id)),
        (line(json!({"command_id": id, "command":"ping", "extra":1})), "invalid_command", Some(id)),
        (line(reply(id, "Crazy", " ")), "invalid_value", Some(id)),
        (line(json!({"command_id": id, "command":"cancel", "request_id":Uuid::new_v4()})), "ownership_denied", Some(id)),
        (format!("{{\"command_id\":\"{id}\",\"command_id\":\"{id}\",\"command\":\"set_volume\",\"value\":10}}\n").into_bytes(), "invalid_command", None),
        (format!("{{\"command_id\":\"{id}\",\"command\":\"set_volume\",\"value\":10,\"value\":11}}\n").into_bytes(), "invalid_command", Some(id)),
        (format!("{{\"command_id\":\"{id}\",\"command\":\"ping\",\"command\":\"settings\"}}\n").into_bytes(), "invalid_command", Some(id)),
        (line(json!({"command_id":"bad", "command":"ping"})), "invalid_command", None),
    ];
    let before = SettingsApi::snapshot::<VoiceSettings>().unwrap();
    for (bytes, code, correlation) in cases {
        client.bytes(&bytes).await;
        let frame = client.next().await;
        assert_eq!(frame["type"], "error", "{bytes:?}");
        assert_eq!(frame["code"], code, "{frame}");
        assert_eq!(frame.get("command_id").map(uuid), correlation);
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().revision,
            before.revision
        );
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().value,
            before.value
        );
        assert!(fixture.app.timeline().is_empty());
        assert!(fixture.calls.try_recv().is_err());
        let ping = command("ping");
        client.send(ping.clone()).await;
        assert_eq!(
            client.next().await,
            json!({"type":"pong", "command_id":ping["command_id"]})
        );
    }
    let start = reply(id, "Crazy", "held");
    client.send(start).await;
    let accepted = client.accepted(id).await;
    let call = fixture.call().await;
    let conflicting = json!({"command_id":id, "command":"ping"});
    client.send(conflicting).await;
    let error = client.error("different intent").await;
    assert_eq!(error["code"], "command_id_conflict");
    assert_eq!(error["command_id"], id.to_string());
    let full = json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"});
    client.send(full.clone()).await;
    let error = client.error("too many tracked").await;
    assert_eq!(error["code"], "receipt_capacity");
    assert_eq!(error["command_id"], full["command_id"]);
    call.finish("finished");
    client.terminal(uuid(&accepted["request_id"])).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn diagnostics_report_actual_listener_clients_limits_and_sanitized_error_state() {
    let fixture = Fixture::start(|s| {
        s.max_clients = positive(3);
        s.receipt_capacity = positive(2);
    })
    .await;
    let server = fixture.app.server().clone();
    let mut client = fixture.connect().await;
    let second = fixture.connect().await;
    server_state(&server, |s| s.connected_clients == 2).await;
    {
        let cmd = command("server_status");
        client.send(cmd.clone()).await;
        let frame = client.next().await;
        assert_eq!(frame["type"], "server_status");
        assert_eq!(frame["command_id"], cmd["command_id"]);
        assert_eq!(frame["address"], server.address().to_string());
        assert_eq!(frame["running"], true);
        assert_eq!(frame["connected_clients"], 2);
        assert_eq!(frame["accepted_connections"], 2);
        assert_eq!(
            frame["limits"],
            json!({"max_clients":3,"receipt_capacity":2,"outbound_capacity":64,"max_command_bytes":65536})
        );
        assert_eq!(frame["has_error"], false);
        assert!(frame.get("last_error").is_none());
    }
    drop(second);
    server_state(&server, |s| s.connected_clients == 1).await;
    client.send(command("server_status")).await;
    let frame = client.next().await;
    assert_eq!(frame["connected_clients"], 1);
    assert_eq!(frame["accepted_connections"], 2);
    let mut invalid = fixture.connect().await;
    invalid.bytes(&[0xff, b'\n']).await;
    invalid.closed().await;
    server_state(&server, |s| {
        s.connected_clients == 1 && s.last_error.is_some()
    })
    .await;
    client.send(command("server_status")).await;
    let frame = client.next().await;
    assert_eq!(frame["has_error"], true);
    assert!(frame.get("last_error").is_none());
    assert_eq!(frame["accepted_connections"], 3);
    assert!(!frame.to_string().contains("unused-fixture-key"));
    assert!(frame.get("ready").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn rejected_commands_are_atomic_and_connection_recovers() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut invalid = vec![
        b"{\n".to_vec(),
        b"test\n".to_vec(),
        line(command("test")),
        b"[]\n".to_vec(),
        b"null\n".to_vec(),
        b"\n".to_vec(),
        line(json!({"command":"settings"})),
        line(json!({"command_id":"invalid", "command":"initiate"})),
        line(command("unknown")),
    ];
    for name in [
        "ping",
        "methods",
        "server_status",
        "settings",
        "reply",
        "cancel",
        "set_volume",
    ] {
        let mut value = command(name);
        value["extra"] = json!(true);
        match name {
            "reply" => {
                value["character"] = json!("Crazy");
                value["text"] = json!("must not run");
            }
            "cancel" => value["request_id"] = json!(Uuid::new_v4()),
            "set_volume" => value["value"] = json!(12),
            _ => {}
        }
        invalid.push(line(value));
    }
    for value in [
        json!(-1),
        json!(101),
        json!(256),
        json!(0.5),
        json!("50"),
        Value::Null,
    ] {
        let mut cmd = command("set_volume");
        cmd["value"] = value;
        invalid.push(line(cmd));
    }
    invalid.extend([
        line(reply(Uuid::new_v4(), "missing", "hello")),
        line(reply(Uuid::new_v4(), "Crazy", " \t")),
        line(
            json!({"command_id":Uuid::new_v4(), "command":"reply", "character":"Crazy", "text":4}),
        ),
        line(json!({"command_id":Uuid::new_v4(), "command":"cancel", "request_id":"bad"})),
    ]);
    for bytes in invalid {
        let before = SettingsApi::snapshot::<VoiceSettings>().unwrap();
        let timeline = fixture.app.timeline().len();
        client.bytes(&bytes).await;
        assert_eq!(client.next().await["type"], "error", "input: {bytes:?}");
        client.send(command("settings")).await;
        let current = client.next().await;
        assert_eq!(current["type"], "settings");
        assert_eq!(current["revision"], before.revision);
        assert_eq!(
            current["settings"]["voice"]["volume"],
            before.value.volume.get()
        );
        assert_eq!(fixture.app.timeline().len(), timeline);
        assert!(fixture.calls.try_recv().is_err());
    }
    for volume in [0, 100] {
        let mut set = command("set_volume");
        set["value"] = json!(volume);
        client.send(set).await;
        let applied = client.until(|v| v["type"] == "settings_applied").await;
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().revision,
            applied["revision"].as_u64().unwrap()
        );
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>()
                .unwrap()
                .value
                .volume
                .get(),
            volume
        );
    }
    let id = Uuid::new_v4();
    client.send(reply(id, "Kind", "valid after errors")).await;
    let accepted = client.accepted(id).await;
    let call = fixture.call().await;
    assert_eq!(call.context.character_id, CharacterId::Kind);
    assert!(call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "valid after errors")));
    call.finish("accepted");
    assert_eq!(
        client.terminal(uuid(&accepted["request_id"])).await["text"],
        "accepted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn fragmented_utf8_and_coalesced_lines_preserve_commands() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let id = Uuid::new_v4();
    let bytes = line(
        json!({"command_id":id,"command":"initiate","character":"Crazy","instruction":"Привет"}),
    );
    let split = bytes.iter().position(|byte| *byte >= 128).unwrap() + 1;
    client.bytes(&bytes[..split]).await;
    SettingsApi::edit::<VoiceSettings>(|voice| voice.volume = Volume::new(37).unwrap()).unwrap();
    let settings = client
        .until(|v| v["type"] == "settings" && v["settings"]["voice"]["volume"] == 37)
        .await;
    assert_eq!(settings["type"], "settings");
    assert_eq!(settings["settings"]["voice"]["volume"], 37);
    assert!(fixture.calls.try_recv().is_err());
    let mut rest = bytes[split..bytes.len() - 1].to_vec();
    rest.extend_from_slice(b"\r\n");
    rest.extend_from_slice(&line(command("settings")));
    rest.extend_from_slice(&line(reply(Uuid::new_v4(), "Crazy", "test")));
    client.bytes(&rest).await;
    let accepted = client.accepted(id).await;
    let request = uuid(&accepted["request_id"]);
    let call = fixture.call().await;
    assert_eq!(&*call.context.current_input, "Привет");
    assert_eq!(
        active(request).snapshot().metadata.source,
        RequestSource::Client(client.id)
    );
    let settings = client.until(|v| v["type"] == "settings").await;
    assert_eq!(settings["settings"]["voice"]["volume"], 37);
    let second = client.until(|v| v["type"] == "accepted").await;
    assert_ne!(accepted["request_id"], second["request_id"]);
    call.finish("first");
    client.terminal(request).await;
    let call = fixture.call().await;
    assert!(call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "test")));
    call.finish("second");
    client.terminal(uuid(&second["request_id"])).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn receipts_replay_terminal_results_and_evict_any_terminal_entry() {
    let mut fixture = Fixture::start(|server| server.receipt_capacity = positive(2)).await;
    let mut client = fixture.connect().await;
    let first = Uuid::new_v4();
    let first_command = reply(first, "Crazy", "long running");
    client.send(first_command.clone()).await;
    let original = client.accepted(first).await;
    let first_request = uuid(&original["request_id"]);
    let first_call = fixture.call().await;
    client.send(first_command.clone()).await;
    let replay = client.accepted(first).await;
    assert_eq!(replay["request_id"], original["request_id"]);
    assert_eq!(replay["replayed"], true);
    for changed in [
        reply(first, "Crazy", "changed"),
        reply(first, "Kind", "long running"),
        json!({"command_id": first, "command":"set_volume", "value":1}),
    ] {
        client.send(changed).await;
        client.error("different intent").await;
    }
    assert_eq!(
        SettingsApi::snapshot::<VoiceSettings>()
            .unwrap()
            .value
            .volume
            .get(),
        80
    );
    let second = Uuid::new_v4();
    let second_command = reply(second, "Kind", "short running");
    client.send(second_command.clone()).await;
    let accepted = client.accepted(second).await;
    let second_request = uuid(&accepted["request_id"]);
    let second_call = fixture.call().await;
    client.send(reply(Uuid::new_v4(), "Kind", "full")).await;
    client.error("too many tracked requests").await;
    assert!(fixture.calls.try_recv().is_err());
    second_call.finish("terminal result");
    let finished = client.terminal(second_request).await;
    assert_eq!(finished["outcome"], "success");
    client.send(reply(Uuid::new_v4(), "Kind", " ")).await;
    client.error("target event is not observable").await;
    client.send(second_command).await;
    let replay = client.accepted(second).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.terminal(second_request).await, finished);
    assert!(fixture.calls.try_recv().is_err());
    let third = Uuid::new_v4();
    client
        .send(reply(third, "Kind", "reuse terminal slot"))
        .await;
    let accepted = client.accepted(third).await;
    fixture.call().await.finish("third result");
    client.terminal(uuid(&accepted["request_id"])).await;
    client.send(first_command).await;
    assert_eq!(
        client.accepted(first).await["request_id"],
        original["request_id"]
    );
    client.send(cancel(first_request)).await;
    let cancelled = client.until(|v| v["type"] == "cancel").await;
    assert_eq!(cancelled["result"], "Requested");
    assert_eq!(client.terminal(first_request).await["state"], "cancelled");
    drop(first_call);
    fixture.shutdown().await;
}

#[tokio::test]
async fn cancellation_and_reconnect_scopes_are_per_connection() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut owner = fixture.connect().await;
    let mut other = fixture.connect().await;
    let owner_id = owner.id;
    assert_ne!(owner.id, other.id);
    let command_id = Uuid::new_v4();
    let original_command = reply(command_id, "Crazy", "survives disconnect");
    owner.send(original_command.clone()).await;
    let accepted = owner.accepted(command_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let handle = active(request_id);
    let call = fixture.call().await;
    other.send(cancel(request_id)).await;
    other.error("does not belong").await;
    assert!(!handle.snapshot().state.is_terminal());
    let other_command = Uuid::new_v4();
    other
        .send(reply(other_command, "Kind", "other client"))
        .await;
    let other_accepted = other.accepted(other_command).await;
    let other_id = uuid(&other_accepted["request_id"]);
    let other_call = fixture.call().await;
    owner.send(cancel(other_id)).await;
    owner.error("does not belong").await;
    drop(owner);
    server_state(fixture.app.server(), |s| s.connected_clients == 1).await;
    let mut reconnected = fixture.connect().await;
    assert_ne!(reconnected.id, owner_id);
    reconnected.send(cancel(request_id)).await;
    reconnected.error("does not belong").await;
    assert_eq!(
        handle.snapshot().metadata.source,
        RequestSource::Client(owner_id)
    );
    reconnected.send(original_command).await;
    let new_accepted = reconnected.accepted(command_id).await;
    assert_eq!(new_accepted["replayed"], false);
    assert_ne!(new_accepted["request_id"], accepted["request_id"]);
    let new_id = uuid(&new_accepted["request_id"]);
    let new_handle = active(new_id);
    assert_eq!(
        new_handle.snapshot().metadata.source,
        RequestSource::Client(reconnected.id)
    );
    call.finish("completed after disconnect");
    let terminal = bounded(handle.wait()).await.unwrap();
    assert!(
        matches!(&terminal.state, RequestState::Finished(RequestOutcome::Success(response)) if &*response.text == "completed after disconnect")
    );
    fixture.call().await.finish("new connection result");
    let final_frame = reconnected
        .until(|v| {
            if v["type"] == "request_status" {
                assert_ne!(
                    v["request_id"],
                    request_id.to_string(),
                    "old request observation leaked across reconnect"
                );
            }
            v["type"] == "request_status"
                && v["request_id"] == new_id.to_string()
                && v["state"] == "finished"
        })
        .await;
    assert_eq!(final_frame["text"], "new connection result");
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .state(CharacterId::Crazy)
            .await
            .unwrap()
            .accepted_replies,
        2
    );
    other.send(cancel(other_id)).await;
    assert_eq!(
        other.until(|v| v["type"] == "cancel").await["result"],
        "Requested"
    );
    assert_eq!(other.terminal(other_id).await["state"], "cancelled");
    drop(other_call);
    fixture.shutdown().await;
}

#[tokio::test]
async fn queue_full_rejection_does_not_reserve_a_command_id() {
    let mut fixture = Fixture::with_queue(1, |_| {}).await;
    let mut client = fixture.connect().await;
    let first = Uuid::new_v4();
    client.send(reply(first, "Crazy", "running")).await;
    let first_request = uuid(&client.accepted(first).await["request_id"]);
    let first_call = fixture.call().await;
    let second = Uuid::new_v4();
    client.send(reply(second, "Crazy", "queued")).await;
    let second_request = uuid(&client.accepted(second).await["request_id"]);
    assert!(matches!(
        active(second_request).snapshot().state,
        RequestState::Pending
    ));
    let third = Uuid::new_v4();
    let third_command = reply(third, "Crazy", "retry after queue full");
    let timeline = fixture.app.timeline().len();
    client.send(third_command.clone()).await;
    let rejected = client.until(|v| v["type"] == "error").await;
    assert_eq!(rejected["command_id"], third.to_string());
    assert_eq!(rejected["error"], "request queue is full");
    assert_eq!(rejected["code"], "queue_full");
    assert!(fixture.calls.try_recv().is_err());
    assert_eq!(fixture.app.timeline().len(), timeline);
    first_call.finish("first done");
    client.terminal(first_request).await;
    let second_call = fixture.call().await;
    assert!(second_call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "queued")));
    client.send(third_command.clone()).await;
    let accepted = client.accepted(third).await;
    assert_eq!(accepted["replayed"], false);
    let third_request = uuid(&accepted["request_id"]);
    second_call.finish("second done");
    client.terminal(second_request).await;
    let third_call = fixture.call().await;
    assert!(third_call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "retry after queue full")));
    third_call.finish("third done");
    let finished = client.terminal(third_request).await;
    client.send(third_command).await;
    assert_eq!(
        client.accepted(third).await["request_id"],
        accepted["request_id"]
    );
    assert_eq!(client.terminal(third_request).await, finished);
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .state(CharacterId::Crazy)
            .await
            .unwrap()
            .accepted_replies,
        3
    );
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn max_clients_rejects_overflow_and_recovers_freed_slots() {
    let fixture = Fixture::start(|s| s.max_clients = positive(1)).await;
    for count in 1..=8 {
        let mut client = fixture.connect().await;
        server_state(fixture.app.server(), |s| s.connected_clients == 1).await;
        let rejected = bounded(TcpStream::connect(fixture.app.server().address()))
            .await
            .unwrap();
        let (read, write) = rejected.into_split();
        let mut rejected = Client {
            read: BufReader::new(read),
            write,
            id: Uuid::nil(),
            initial_settings: Value::Null,
        };
        rejected.closed().await;
        assert_eq!(fixture.app.server().snapshot().accepted_connections, count);
        client.send(command("settings")).await;
        assert_eq!(client.next().await["type"], "settings");
        drop(client);
        server_state(fixture.app.server(), |s| s.connected_clients == 0).await;
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn invalid_frames_close_connections_and_shutdown_interrupts_partial_reads() {
    const LIMIT: usize = 256;
    let fixture = Fixture::start(|s| s.max_command_bytes = positive(LIMIT)).await;
    let mut client = fixture.connect().await;
    let mut exact = line(command("settings"));
    exact.pop();
    exact.resize(LIMIT - 1, b' ');
    exact.push(b'\n');
    client.bytes(&exact).await;
    assert_eq!(client.next().await["type"], "settings");
    drop(client);
    server_state(fixture.app.server(), |s| s.connected_clients == 0).await;
    for (bytes, incomplete, error) in [
        (vec![b'x'; LIMIT + 1], false, "command too long"),
        (vec![0xff, b'\n'], false, "command is not UTF-8"),
        (
            line(command("set_volume")).into_iter().take(10).collect(),
            true,
            "incomplete command",
        ),
    ] {
        let mut client = fixture.connect().await;
        client.bytes(&bytes).await;
        if incomplete {
            bounded(client.write.shutdown()).await.unwrap();
        }
        client.closed().await;
        server_state(fixture.app.server(), |s| {
            s.connected_clients == 0
                && s.last_error
                    .as_deref()
                    .is_some_and(|message| message.contains(error))
        })
        .await;
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>()
                .unwrap()
                .value
                .volume
                .get(),
            80
        );
        assert!(fixture.app.timeline().is_empty());
    }
    let mut idle = fixture.connect().await;
    let mut partial = fixture.connect().await;
    partial.bytes(b"{\"command_id\":").await;
    let server = fixture.app.server().clone();
    fixture.shutdown().await;
    assert!(!server.snapshot().running);
    assert_eq!(server.snapshot().connected_clients, 0);
    idle.closed().await;
    partial.closed().await;
    assert!(TcpStream::connect(server.address()).await.is_err());
}

#[tokio::test]
async fn game_link_push_resume_idempotency_and_session_ownership() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut receive = String::new();
    let hello = Uuid::new_v4();
    write
        .write_all(&line(json!({"command_id":hello,"command":"hello"})))
        .await
        .unwrap();
    reader.read_line(&mut receive).await.unwrap();
    let response: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(response["type"], "hello_ack");
    let session_id = uuid(&response["session_id"]);
    let token = uuid(&response["session_token"]);
    let issued = Uuid::new_v4();
    let command = json!({"command_id":issued,"command":"initiate","character":"Crazy","instruction":"from game"});
    write.write_all(&line(command.clone())).await.unwrap();
    receive.clear();
    reader.read_line(&mut receive).await.unwrap();
    let ack: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(ack["type"], "request_accepted");
    let request_id = uuid(&ack["request_id"]);
    assert_eq!(
        active(request_id).snapshot().metadata.source,
        RequestSource::Game(session_id)
    );
    let call = fixture.call().await;
    drop(reader);
    drop(write);

    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    write.write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session_id,"session_token":token}))).await.unwrap();
    receive.clear();
    reader.read_line(&mut receive).await.unwrap();
    let resumed: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(resumed["type"], "session_resumed");
    assert_eq!(resumed["requests"], 1);
    write.write_all(&line(command)).await.unwrap();
    let mut replay = None;
    for _ in 0..5 {
        receive.clear();
        bounded(reader.read_line(&mut receive)).await.unwrap();
        let event: Value = serde_json::from_str(&receive).unwrap();
        if event["type"] == "request_accepted" {
            replay = Some(event);
            break;
        }
    }
    let replay = replay.expect("command must replay after reconnect");
    assert_eq!(replay["replayed"], true);
    assert_eq!(uuid(&replay["request_id"]), request_id);
    assert!(fixture.calls.try_recv().is_err());
    call.finish("game result");
    let mut finished = false;
    for _ in 0..8 {
        receive.clear();
        bounded(reader.read_line(&mut receive)).await.unwrap();
        let event: Value = serde_json::from_str(&receive).unwrap();
        if event["type"] == "request_completed" {
            assert_eq!(event["request_id"], request_id.to_string());
            assert_eq!(event["text"], "game result");
            finished = true;
            break;
        }
    }
    assert!(finished);
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_rejects_unidentified_and_cross_session_requests() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}),
        ))
        .await
        .unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "hello_required");
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["type"], "hello_ack");
    let session_id = uuid(&response["session_id"]);
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"cancel","request_id":Uuid::new_v4()}),
        ))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "ownership_denied");
    drop(reader);
    drop(write);
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    write.write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session_id,"session_token":Uuid::new_v4()}))).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "session_denied");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_semantic_world_events_are_atomic_and_replay_safe() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["type"],
        "hello_ack"
    );
    let command_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let batch = json!({
        "command_id": command_id,
        "command": "publish_events",
        "events": [{
            "event_id": event_id, "kind": "unity_event", "occurred_at_unix_ms": 42,
            "actor": "Player", "observers": ["Crazy"],
            "payload": {"name": "DoorOpened"}
        }]
    });
    write.write_all(&line(batch.clone())).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(receipt["type"], "events_published");
    assert_eq!(receipt["event_ids"], json!([event_id]));
    write.write_all(&line(batch.clone())).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(receipt["replayed"], true);
    let mut conflicting = batch;
    conflicting["events"][0]["payload"]["name"] = json!("WindowOpened");
    write.write_all(&line(conflicting)).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let rejection: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(rejection["code"], "command_id_conflict");
    write
        .write_all(&line(json!({
            "command_id": Uuid::new_v4(), "command": "react", "character": "Crazy",
            "event_id": event_id, "instruction": "remember the door"
        })))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let accepted: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(accepted["type"], "request_accepted");
    let call = fixture.call().await;
    call.finish("door acknowledged");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_resume_replaces_old_tcp_connection() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (old_read, mut old_write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut old_reader = BufReader::new(old_read);
    let mut buffer = String::new();
    old_write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    bounded(old_reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    let (new_read, mut new_write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut new_reader = BufReader::new(new_read);
    new_write
        .write_all(&line(json!({
            "command_id":Uuid::new_v4(),"command":"resume",
            "session_id":receipt["session_id"],"session_token":receipt["session_token"]
        })))
        .await
        .unwrap();
    buffer.clear();
    bounded(new_reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["type"],
        "session_resumed"
    );
    buffer.clear();
    assert_eq!(bounded(old_reader.read_line(&mut buffer)).await.unwrap(), 0);
    new_write
        .write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"ping"})))
        .await
        .unwrap();
    let response = bounded(async {
        loop {
            buffer.clear();
            new_reader.read_line(&mut buffer).await.unwrap();
            let event: Value = serde_json::from_str(&buffer).unwrap();
            if event["type"] == "pong" {
                break event;
            }
        }
    })
    .await;
    assert_eq!(response["type"], "pong");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_enforces_frame_size_while_receiving() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write.write_all(&vec![b'X'; 100_000]).await.unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "command_too_large");
    buffer.clear();
    assert_eq!(bounded(reader.read_line(&mut buffer)).await.unwrap(), 0);
    fixture.shutdown().await;
}

struct GameClient {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}
impl GameClient {
    async fn connect(address: SocketAddr) -> Self {
        let (read, write) = bounded(TcpStream::connect(address))
            .await
            .unwrap()
            .into_split();
        Self {
            read: BufReader::new(read),
            write,
        }
    }
    async fn send(&mut self, command: Value) {
        bounded(self.write.write_all(&line(command))).await.unwrap();
    }
    async fn next(&mut self) -> Value {
        let mut frame = String::new();
        assert_ne!(bounded(self.read.read_line(&mut frame)).await.unwrap(), 0);
        serde_json::from_str(&frame).unwrap()
    }
    async fn until(&mut self, expected: &str) -> Value {
        bounded(async {
            loop {
                let frame = self.next().await;
                if frame["type"] == expected {
                    return frame;
                }
            }
        })
        .await
    }
    async fn hello(&mut self) -> (Uuid, Uuid) {
        self.send(json!({"command":"hello","command_id":Uuid::new_v4()}))
            .await;
        let answer = self.until("hello_ack").await;
        (uuid(&answer["session_id"]), uuid(&answer["session_token"]))
    }
}

#[tokio::test]
async fn game_link_captures_world_context_at_admission_and_restores_revision() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    let (session, token) = game.hello().await;
    let first = Uuid::new_v4();
    let context = json!({"revision":1,"scene":"Kitchen","world":{"room":"Kitchen","actors":["Crazy","Player"]},"capabilities":["look","walk"]});
    game.send(json!({"command_id":first,"command":"update_context","context":context}))
        .await;
    let ack = game.until("context_applied").await;
    assert_eq!(ack["revision"], 1);
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy","instruction":"look around"})).await;
    let accepted = game.until("request_accepted").await;
    let first_id = uuid(&accepted["request_id"]);
    let call = fixture.call().await;
    assert_eq!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Kitchen")
    );
    assert_eq!(call.context.runtime_context.as_ref().unwrap().revision, 1);
    assert_eq!(
        call.context.runtime_context.as_ref().unwrap().capabilities,
        vec!["look", "walk"]
    );
    game.send(json!({"command_id":Uuid::new_v4(),"command":"update_context","context":{"revision":2,"scene":"Garden","world":{},"capabilities":[]}})).await;
    assert_eq!(game.until("context_applied").await["revision"], 2);
    assert_eq!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Kitchen")
    );
    call.finish("first game response");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let second = fixture.call().await;
    assert_eq!(
        second
            .context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Garden")
    );
    second.finish("second game response");
    let cmd = Uuid::new_v4();
    game.send(json!({"command_id":cmd,"command":"update_context","context":{"revision":2,"scene":"Kitchen","world":{},"capabilities":[]}})).await;
    assert_eq!(game.until("error").await["code"], "context_rejected");
    game.send(json!({"command_id":first,"command":"update_context","context":context}))
        .await;
    assert_eq!(
        game.until("context_applied").await["command_id"],
        first.to_string()
    );
    drop(game);
    let mut resumed = GameClient::connect(address).await;
    resumed.send(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session,"session_token":token})).await;
    assert_eq!(
        resumed.until("session_resumed").await["session_id"],
        session.to_string()
    );
    let state = resumed.until("session_state").await;
    assert_eq!(state["context_revision"], 2);
    assert_ne!(first_id, Uuid::nil());
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_audio_chunks_delivery_ack_and_ptt_survive_resume() {
    let mut fixture = Fixture::with_voice(8, |_| {}, true).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    let (session, token) = game.hello().await;
    let capture = Uuid::new_v4();
    let ptt_cmd = Uuid::new_v4();
    game.send(json!({"command_id":ptt_cmd,"command":"report_ptt","capture_id":capture,"generation":4,"active":true})).await;
    assert_eq!(game.until("ptt_accepted").await["generation"], 4);
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_ptt","capture_id":capture,"generation":3,"active":false})).await;
    assert_eq!(game.until("error").await["code"], "ptt_rejected");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let request = uuid(&game.until("request_accepted").await["request_id"]);
    fixture.call().await.finish("audio output");
    let completed = game.until("request_completed").await;
    assert_eq!(completed["request_id"], request.to_string());
    assert_eq!(completed["audio_asset"]["format"], "wave");
    assert_eq!(completed["audio_asset"]["size_bytes"], 17);
    assert!(completed["audio"].is_null());
    let event = uuid(&completed["event_id"]);
    let read_id = Uuid::new_v4();
    game.send(json!({"command_id":read_id,"command":"read_audio_chunk","request_id":request,"offset":0,"length":128})).await;
    let chunk = game.until("audio_chunk").await;
    assert_eq!(chunk["base64"], "UklGRi1maXh0dXJlLXdhdmU=");
    let playback = Uuid::new_v4();
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_playback","request_id":request,"playback_id":playback,"phase":"started"})).await;
    assert_eq!(game.until("playback_accepted").await["phase"], "started");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_playback","request_id":request,"playback_id":playback,"phase":"completed"})).await;
    assert_eq!(game.until("playback_accepted").await["phase"], "completed");
    let ack_id = Uuid::new_v4();
    game.send(json!({"command_id":ack_id,"command":"acknowledge_result","request_id":request,"event_id":event,"applied":true})).await;
    assert_eq!(game.until("result_acknowledged").await["applied"], true);
    drop(game);
    let mut resumed = GameClient::connect(address).await;
    resumed.send(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session,"session_token":token})).await;
    resumed.until("session_resumed").await;
    let state = resumed.until("session_state").await;
    assert_eq!(state["ptt"]["capture_id"], capture.to_string());
    assert_eq!(state["deliveries"][0]["ack"]["applied"], true);
    assert_eq!(state["playback"][0]["report"]["phase"], "completed");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_restricts_audio_and_result_ack_to_session() {
    let mut fixture = Fixture::with_voice(8, |_| {}, true).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut first = GameClient::connect(address).await;
    first.hello().await;
    let mut second = GameClient::connect(address).await;
    second.hello().await;
    first
        .send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let request = uuid(&first.until("request_accepted").await["request_id"]);
    fixture.call().await.finish("secret");
    let frame = first.until("request_completed").await;
    let event = uuid(&frame["event_id"]);
    second.send(json!({"command_id":Uuid::new_v4(),"command":"read_audio_chunk","request_id":request,"offset":0,"length":1024})).await;
    assert_eq!(second.until("error").await["code"], "ownership_denied");
    second.send(json!({"command_id":Uuid::new_v4(),"command":"acknowledge_result","request_id":request,"event_id":event,"applied":true})).await;
    assert_eq!(second.until("error").await["code"], "ownership_denied");
    first.send(json!({"command_id":Uuid::new_v4(),"command":"read_audio_chunk","request_id":request,"offset":0,"length":20_000})).await;
    assert_eq!(first.until("error").await["code"], "invalid_chunk");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_pushes_scoped_game_settings_without_polling() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    game.hello().await;
    game.send(json!({"command_id":Uuid::new_v4(),"command":"get_game_settings"}))
        .await;
    let initial = game.until("game_settings").await;
    assert!(initial["voice"]["enabled"].is_boolean());
    let revision = initial["revision"].as_u64().unwrap();
    SettingsApi::edit::<VoiceSettings>(|voice| {
        voice.in_game = !voice.in_game;
    })
    .unwrap();
    let pushed = game.until("game_settings_updated").await;
    assert!(pushed["revision"].as_u64().unwrap() > revision);
    assert_ne!(pushed["voice"]["enabled"], initial["voice"]["enabled"]);
    assert!(pushed.get("api").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_never_mixes_runtime_contexts_between_game_sessions() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut first = GameClient::connect(address).await;
    first.hello().await;
    let mut second = GameClient::connect(address).await;
    second.hello().await;
    first.send(json!({"command_id":Uuid::new_v4(),"command":"update_context","context":{"revision":1,"scene":"PrivateKitchen","world":{},"capabilities":[]}})).await;
    first.until("context_applied").await;
    second
        .send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    second.until("request_accepted").await;
    let call = fixture.call().await;
    assert!(call.context.runtime_context.is_some());
    assert!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .is_none()
    );
    call.finish("no shared runtime scene");
    fixture.shutdown().await;
}

#[tokio::test]
async fn local_control_accepts_only_loopback_bind_addresses() {
    for address in ["0.0.0.0:0", "192.0.2.1:0", "[::]:0"] {
        let config = ServerConfig::local_control(address.parse().unwrap());
        let result = ServerLifecycle::bind(config).await;
        assert!(
            matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput),
            "LocalControl allowed {address}"
        );
    }

    let (lifecycle, handle) =
        ServerLifecycle::bind(ServerConfig::local_control("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
    assert!(handle.address().ip().is_loopback());
    drop(lifecycle);
}
