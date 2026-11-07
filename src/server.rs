use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedReadHalf},
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    character::CharacterId,
    identity::EventId,
    lifecycle::Lifecycle,
    request::{
        DiagnosticLookupError, DiagnosticScope, DiagnosticSection, InitiateRequest,
        ProcessingStage, ReactRequest, ReplyRequest, RequestHandle, RequestId, RequestOutcome,
        RequestSnapshot, RequestSource, RequestState, SubmitError,
    },
    runtime::{self, ApiBindings},
    settings::{SettingsError, VoiceSettings, Volume},
    timeline::conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
};

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub max_clients: NonZeroUsize,
    pub outbound_capacity: NonZeroUsize,
    pub max_command_bytes: NonZeroUsize,
    pub receipt_capacity: NonZeroUsize,
}

impl ServerConfig {
    pub fn local_control(bind: SocketAddr) -> Self {
        Self {
            bind,
            max_clients: NonZeroUsize::new(16).expect("positive"),
            outbound_capacity: NonZeroUsize::new(64).expect("positive"),
            max_command_bytes: NonZeroUsize::new(65536).expect("positive"),
            receipt_capacity: NonZeroUsize::new(128).expect("positive"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerSnapshot {
    pub running: bool,
    pub connected_clients: usize,
    pub accepted_connections: u64,
    pub last_error: Option<Arc<str>>,
}

struct ServerState {
    snapshot: Mutex<ServerSnapshot>,
    updates: watch::Sender<Arc<ServerSnapshot>>,
}
impl ServerState {
    fn change(&self, edit: impl FnOnce(&mut ServerSnapshot)) {
        let mut snapshot = self.snapshot.lock().expect("server state poisoned");
        edit(&mut snapshot);
        self.updates.send_replace(Arc::new(snapshot.clone()));
    }
}

#[derive(Clone)]
pub struct ServerHandle {
    address: SocketAddr,
    state: Arc<ServerState>,
}
impl ServerHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn snapshot(&self) -> Arc<ServerSnapshot> {
        Arc::clone(&self.state.updates.borrow())
    }
    pub fn observe(&self) -> watch::Receiver<Arc<ServerSnapshot>> {
        self.state.updates.subscribe()
    }
}

pub struct ServerLifecycle {
    listener: TcpListener,
    config: Arc<ServerConfig>,
    state: Arc<ServerState>,
}
impl ServerLifecycle {
    pub async fn bind(config: ServerConfig) -> io::Result<(Self, ServerHandle)> {
        if !config.bind.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LocalControl requires a loopback bind address",
            ));
        }
        let listener = TcpListener::bind(config.bind).await?;
        let address = listener.local_addr()?;
        let snapshot = ServerSnapshot {
            running: false,
            connected_clients: 0,
            accepted_connections: 0,
            last_error: None,
        };
        let (updates, _) = watch::channel(Arc::new(snapshot.clone()));
        let state = Arc::new(ServerState {
            snapshot: Mutex::new(snapshot),
            updates,
        });
        Ok((
            Self {
                listener,
                config: Arc::new(config),
                state: Arc::clone(&state),
            },
            ServerHandle { address, state },
        ))
    }
}

impl Lifecycle for ServerLifecycle {
    type Error = io::Error;
    async fn run(self, stop: CancellationToken) -> io::Result<()> {
        let api =
            runtime::current().map_err(|_| io::Error::other("application API unavailable"))?;
        self.run_with_api(stop, api).await
    }
}

impl ServerLifecycle {
    pub(crate) async fn run_with_api(
        self,
        stop: CancellationToken,
        api: Arc<ApiBindings>,
    ) -> io::Result<()> {
        let clients_stop = CancellationToken::new();
        let mut clients: JoinSet<io::Result<()>> = JoinSet::new();
        self.state.change(|state| state.running = true);
        let mut failure = None;
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                result = clients.join_next(), if !clients.is_empty() => {
                    match result {
                        Some(Ok(Err(error))) => self.state.change(|state| state.last_error = Some(Arc::from(error.to_string()))),
                        Some(Err(error)) => self.state.change(|state| state.last_error = Some(Arc::from(error.to_string()))),
                        _ => {}
                    }
                }
                connected = self.listener.accept() => {
                    match connected {
                        Ok((stream, _)) => {
                            if clients.len() >= self.config.max_clients.get() { drop(stream); continue; }
                            self.state.change(|state| { state.connected_clients += 1; state.accepted_connections += 1; });
                            let state = Arc::clone(&self.state);
                            let config = Arc::clone(&self.config);
                            let stop = clients_stop.clone();
                            let api = Arc::clone(&api);
                            let server = ServerHandle { address: self.listener.local_addr()?, state: Arc::clone(&self.state) };
                            clients.spawn(async move {
                                let _connected = Connected(state);
                                client(stream, config, stop, api, server).await
                            });
                        }
                        Err(error) => { failure = Some(error); break; }
                    }
                }
            }
        }
        clients_stop.cancel();
        while clients.join_next().await.is_some() {}
        self.state.change(|state| state.running = false);
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

struct Connected(Arc<ServerState>);
impl Drop for Connected {
    fn drop(&mut self) {
        self.0
            .change(|state| state.connected_clients = state.connected_clients.saturating_sub(1));
    }
}

macro_rules! commands {
    ($($variant:ident { $($(#[$attr:meta])* $field:ident : $ty:ty),* $(,)? }),* $(,)?) => {
        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
        enum Command { $($variant { $($(#[$attr])* $field: $ty),* }),* }

        #[derive(Clone, Copy, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Method { $($variant),* }

        impl Method { const ALL: &'static [Self] = &[$(Self::$variant),*]; }
    };
}

commands! {
    Ping {}, Methods {}, ServerStatus {},
    PublishMessage {
        event_id: Uuid,
        occurred_at_unix_ms: u64,
        speaker: ConversationSpeaker,
        content: Value,
        #[serde(flatten)]
        audience: WireAudience,
    },
    Reply {
        character: CharacterId,
        message_id: Uuid,
        #[serde(flatten)]
        audience: WireAudience,
    },
    React {
        character: CharacterId,
        event_id: Uuid,
        #[serde(default)]
        instruction: String,
        #[serde(flatten)]
        audience: WireAudience,
    },
    Initiate {
        character: CharacterId,
        #[serde(default)]
        instruction: String,
        #[serde(flatten)]
        audience: WireAudience,
    },
    Cancel {
        request_id: Uuid,
    },
    Diagnostics {
        character: CharacterId,
        request_id: Uuid,
        #[serde(default)]
        section: Option<String>,
    },
    DiagnosticsRecent {
        character: CharacterId,
        #[serde(default)]
        limit: Option<usize>,
    },
    DiagnosticsErrors {
        character: CharacterId,
        #[serde(default)]
        limit: Option<usize>,
    },
    Settings {},
    SetVolume {
        value: Volume,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    MalformedJson,
    InvalidCommand,
    UnknownCommand,
    InvalidValue,
    CommandIdConflict,
    QueueFull,
    ReceiptCapacity,
    OwnershipDenied,
    DiagnosticsUnavailable,
    DiagnosticsNotFound,
    DiagnosticsExpired,
    CoreUnavailable,
    SettingsConflict,
    SettingsStorage,
}

fn error_response(id: Option<Uuid>, code: ErrorCode, message: impl std::fmt::Display) -> Value {
    let mut value = json!({"type":"error", "code":code, "error":message.to_string()});
    if let Some(id) = id {
        value["command_id"] = json!(id);
    }
    value
}

fn decode(line: &str) -> Result<Envelope, Value> {
    match serde_json::from_str::<Envelope>(line) {
        Ok(envelope) => Ok(envelope),
        Err(error) => {
            let value = serde_json::from_str::<Value>(line);
            let id = serde_json::from_str::<CommandCorrelation>(line)
                .ok()
                .and_then(|v| v.command_id);
            let code = match &value {
                Err(_) => ErrorCode::MalformedJson,
                Ok(value)
                    if value
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            serde_json::from_value::<Method>(json!(name)).is_err()
                        }) =>
                {
                    ErrorCode::UnknownCommand
                }
                _ => ErrorCode::InvalidCommand,
            };
            Err(error_response(id, code, error))
        }
    }
}

#[derive(Deserialize)]
struct CommandCorrelation {
    command_id: Option<Uuid>,
}

fn settings_code(error: &SettingsError) -> ErrorCode {
    match error {
        SettingsError::Validation(_) => ErrorCode::InvalidValue,
        SettingsError::Conflict { .. } => ErrorCode::SettingsConflict,
        SettingsError::Storage(_) => ErrorCode::SettingsStorage,
        SettingsError::Uninitialized | SettingsError::Stopped => ErrorCode::CoreUnavailable,
    }
}

fn diagnostic_code(error: &DiagnosticLookupError) -> ErrorCode {
    match error {
        DiagnosticLookupError::Unavailable | DiagnosticLookupError::Stopped => {
            ErrorCode::DiagnosticsUnavailable
        }
        DiagnosticLookupError::NotFound(_) | DiagnosticLookupError::Empty => {
            ErrorCode::DiagnosticsNotFound
        }
        DiagnosticLookupError::Expired(_) => ErrorCode::DiagnosticsExpired,
        DiagnosticLookupError::OwnershipDenied(_) => ErrorCode::OwnershipDenied,
    }
}

fn submit_code(error: &SubmitError) -> ErrorCode {
    match error {
        SubmitError::InvalidTargetEvent => ErrorCode::InvalidValue,
        SubmitError::TimelineUnavailable => ErrorCode::CoreUnavailable,
        SubmitError::QueueFull => ErrorCode::QueueFull,
        SubmitError::Uninitialized
        | SubmitError::Stopped
        | SubmitError::QueueClosed
        | SubmitError::CharacterUnavailable => ErrorCode::CoreUnavailable,
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WireAudience {
    #[serde(default)]
    targets: Vec<ConversationSpeaker>,
    #[serde(default)]
    participants: Vec<ConversationSpeaker>,
    #[serde(default)]
    seen_by: Vec<CharacterId>,
}

impl WireAudience {
    fn into_audience(self, sender: Option<CharacterId>) -> ConversationAudience {
        let mut audience = ConversationAudience::default();
        audience.targets.extend(self.targets);
        audience.participants.extend(self.participants);
        audience.seen_by.extend(self.seen_by);
        if let Some(character) = sender {
            audience.seen_by.insert(character);
        }
        audience
    }
}

#[derive(Deserialize)]
struct Envelope {
    command_id: Uuid,
    #[serde(flatten)]
    command: Command,
}

struct Receipt {
    command_id: Uuid,
    fingerprint: String,
    result: ReceiptResult,
}

enum ReceiptResult {
    Generation(RequestHandle),
    Published(EventId),
}

async fn send(
    outbound: &mpsc::Sender<Value>,
    value: Value,
    stop: &CancellationToken,
) -> io::Result<()> {
    tokio::select! {
        biased;
        _ = stop.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "connection stopping")),
        sent = outbound.send(value) => sent.map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client writer closed")),
    }
}

async fn read_command(
    reader: &mut BufReader<OwnedReadHalf>,
    limit: usize,
) -> io::Result<Option<String>> {
    let mut result = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if result.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete command",
                ))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |index| index + 1);
        if result.len() + count > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "command too long",
            ));
        }
        result.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if newline.is_some() {
            let text = String::from_utf8(result)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "command is not UTF-8"))?;
            return Ok(Some(text.trim_end_matches(['\r', '\n']).to_owned()));
        }
    }
}

async fn client(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    parent_stop: CancellationToken,
    api: Arc<ApiBindings>,
    server: ServerHandle,
) -> io::Result<()> {
    let stop = parent_stop.child_token();
    let client_id = Uuid::new_v4();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let (outbound, mut messages) = mpsc::channel::<Value>(config.outbound_capacity.get());
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        let _writer_guard = writer_stop.clone().drop_guard();
        loop {
            let message = tokio::select! { biased; _ = writer_stop.cancelled() => break, message = messages.recv() => match message { Some(message) => message, None => break } };
            let mut bytes = serde_json::to_vec(&message).map_err(io::Error::other)?;
            bytes.push(b'\n');
            tokio::select! { biased; _ = writer_stop.cancelled() => break, result = write.write_all(&bytes) => result? }
        }
        Ok::<(), io::Error>(())
    });
    let mut observers = JoinSet::new();
    let mut receipts: VecDeque<Receipt> = VecDeque::new();
    let result = async {
        send(&outbound, json!({"type":"connected","client_id":client_id}), &stop).await?;
        let mut settings = api.settings.observe().map_err(io::Error::other)?;
        let settings_send = outbound.clone();
        let settings_stop = stop.clone();
        observers.spawn(async move {
            loop {
                let snapshot = Arc::clone(&settings.borrow_and_update());
                let message = json!({"type":"settings","revision":snapshot.revision,"settings":snapshot.values});
                if send(&settings_send, message, &settings_stop).await.is_err() { break; }
                tokio::select! { biased; _ = settings_stop.cancelled() => break, changed = settings.changed() => if changed.is_err() { break; } }
            }
        });
        loop {
            while let Some(result) = observers.try_join_next() {
                result.map_err(io::Error::other)?;
            }
            let line = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                line = read_command(&mut reader, config.max_command_bytes.get()) => match line? { Some(line) => line, None => break },
            };
            let envelope = match decode(&line) {
                Ok(envelope) => envelope,
                Err(error) => { send(&outbound, error, &stop).await?; continue; }
            };
            let fingerprint = serde_json::to_string(&envelope.command).map_err(io::Error::other)?;
            if let Some(receipt) = receipts.iter().find(|receipt| receipt.command_id == envelope.command_id) {
                if receipt.fingerprint != fingerprint {
                    send(&outbound, error_response(Some(envelope.command_id), ErrorCode::CommandIdConflict, "command ID reused for different intent"), &stop).await?;
                } else {
                    match &receipt.result {
                        ReceiptResult::Generation(handle) => {
                            send(&outbound, json!({"type":"accepted","command_id":envelope.command_id,"request_id":handle.id().0,"replayed":true}), &stop).await?;
                            send(&outbound, status(&handle.snapshot()), &stop).await?;
                        }
                        ReceiptResult::Published(event) => {
                            send(&outbound, json!({"type":"published","command_id":envelope.command_id,"event_id":event.0,"replayed":true}), &stop).await?;
                        }
                    }
                }
                continue;
            }
            let track = matches!(envelope.command, Command::PublishMessage { .. } | Command::Reply { .. } | Command::React { .. } | Command::Initiate { .. });
            let eviction = if track && receipts.len() >= config.receipt_capacity.get() {
                match receipts.iter().position(|receipt| match &receipt.result {
                    ReceiptResult::Published(_) => true,
                    ReceiptResult::Generation(handle) => handle.snapshot().state.is_terminal(),
                }) {
                    Some(index) => Some(index),
                    None => { send(&outbound, error_response(Some(envelope.command_id), ErrorCode::ReceiptCapacity, "too many tracked requests"), &stop).await?; continue; }
                }
            } else { None };
            match envelope.command {
                Command::Ping {} => {
                    send(&outbound, json!({"type":"pong", "command_id":envelope.command_id}), &stop).await?;
                }
                Command::Methods {} => {
                    send(&outbound, json!({"type":"methods", "command_id":envelope.command_id, "methods":Method::ALL}), &stop).await?;
                }
                Command::ServerStatus {} => {
                    let snapshot = server.snapshot();
                    send(&outbound, json!({"type":"server_status", "command_id":envelope.command_id,
                        "address":server.address(), "running":snapshot.running,
                        "connected_clients":snapshot.connected_clients, "accepted_connections":snapshot.accepted_connections,
                        "has_error":snapshot.last_error.is_some(),
                        "limits":{"max_clients":config.max_clients, "outbound_capacity":config.outbound_capacity,
                            "max_command_bytes":config.max_command_bytes, "receipt_capacity":config.receipt_capacity}}), &stop).await?;
                }
                Command::PublishMessage { event_id, occurred_at_unix_ms, speaker, content, audience } => {
                    let mut message = ConversationMessage::new(event_id.to_string(), speaker, content, []);
                    message.id = EventId(event_id);
                    message.occurred_at_unix_ms = occurred_at_unix_ms;
                    message.audience = audience.into_audience(None);
                    match api.chat.publish_conversation_message(message) {
                        Ok(event) => {
                            if let Some(index) = eviction { receipts.remove(index); }
                            send(&outbound, json!({"type":"published","command_id":envelope.command_id,"event_id":event.0,"replayed":false}), &stop).await?;
                            receipts.push_back(Receipt { command_id: envelope.command_id, fingerprint, result: ReceiptResult::Published(event) });
                        }
                        Err(error) => send(&outbound, error_response(Some(envelope.command_id), submit_code(&error), error), &stop).await?,
                    }
                }
                Command::Reply { .. } | Command::React { .. } | Command::Initiate { .. } => {
                    let source = RequestSource::Client(client_id);
                    let handle = match envelope.command {
                        Command::Reply { character, message_id, audience } => {
                            api.chat.send(ReplyRequest::new(character, EventId(message_id), source).with_audience(audience.into_audience(Some(character)))).map_err(|error| (submit_code(&error), error.to_string()))
                        }
                        Command::React { character, event_id, instruction, audience } => {
                            api.chat.send(ReactRequest::new(character, EventId(event_id), instruction, source).with_audience(audience.into_audience(Some(character)))).map_err(|error| (submit_code(&error), error.to_string()))
                        }
                        Command::Initiate { character, instruction, audience } => {
                            api.chat.send(InitiateRequest::new(character, instruction, source).with_audience(audience.into_audience(Some(character)))).map_err(|error| (submit_code(&error), error.to_string()))
                        }
                        _ => unreachable!(),
                    };
                    let handle = match handle {
                        Ok(handle) => handle,
                        Err((code, error)) => { send(&outbound, error_response(Some(envelope.command_id), code, error), &stop).await?; continue; }
                    };
                    if let Some(index) = eviction { receipts.remove(index); }
                    send(&outbound, json!({"type":"accepted","command_id":envelope.command_id,"request_id":handle.id().0,"replayed":false}), &stop).await?;
                    receipts.push_back(Receipt { command_id: envelope.command_id, fingerprint, result: ReceiptResult::Generation(handle.clone()) });
                    let outbound = outbound.clone();
                    let stop = stop.clone();
                    observers.spawn(async move {
                        let mut observer = handle.observe();
                        loop {
                            let snapshot = tokio::select! { biased; _ = stop.cancelled() => break, snapshot = observer.next() => match snapshot { Ok(Some(snapshot)) => snapshot, _ => break } };
                            if send(&outbound, status(&snapshot), &stop).await.is_err() { break; }
                        }
                    });
                }
                Command::Cancel { request_id } => {
                    let response = match receipts.iter().find_map(|receipt| match &receipt.result { ReceiptResult::Generation(handle) if handle.id() == RequestId(request_id) => Some(handle), _ => None }) {
                        Some(handle) => json!({"type":"cancel","command_id":envelope.command_id,"request_id":request_id,"result":format!("{:?}", handle.cancel())}),
                        None => error_response(Some(envelope.command_id), ErrorCode::OwnershipDenied, "request does not belong to this connection"),
                    };
                    send(&outbound, response, &stop).await?;
                }
                Command::Diagnostics { character, request_id, section } => {
                    let scope = DiagnosticScope::owner(character, RequestSource::Client(client_id));
                    let response = match api.chat.diagnostics(scope) {
                        Err(error) => error_response(Some(envelope.command_id), submit_code(&error), error),
                        Ok(access) => {
                            let section = match section.as_deref() {
                                None => Ok(DiagnosticSection::All),
                                Some(value) => DiagnosticSection::parse(value).ok_or_else(|| {
                                    error_response(Some(envelope.command_id), ErrorCode::InvalidValue, "unknown diagnostic section")
                                }),
                            };
                            match section {
                                Err(error) => error,
                                Ok(section) => match access.request(RequestId(request_id)) {
                                    Err(error) => error_response(Some(envelope.command_id), diagnostic_code(&error), error),
                                    Ok(record) => match record.export(section) {
                                        Err(error) => error_response(Some(envelope.command_id), ErrorCode::InvalidValue, error),
                                        Ok(text) => {
                                            let content = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                                            json!({"type":"diagnostics", "command_id":envelope.command_id,
                                                "request_id":request_id, "section":section.to_string(),
                                                "content":content})
                                        }
                                    },
                                },
                            }
                        }
                    };
                    send(&outbound, response, &stop).await?;
                }
                Command::DiagnosticsRecent { character, limit } => {
                    let scope = DiagnosticScope::owner(character, RequestSource::Client(client_id));
                    let response = match api.chat.diagnostics(scope) {
                        Err(error) => error_response(Some(envelope.command_id), submit_code(&error), error),
                        Ok(access) => match access.recent(limit.unwrap_or(16).clamp(1, 64)) {
                            Err(error) => error_response(Some(envelope.command_id), diagnostic_code(&error), error),
                            Ok(rows) => json!({"type":"diagnostics_recent", "command_id":envelope.command_id,
                                "character":character, "requests":rows}),
                        },
                    };
                    send(&outbound, response, &stop).await?;
                }
                Command::DiagnosticsErrors { character, limit } => {
                    let scope = DiagnosticScope::owner(character, RequestSource::Client(client_id));
                    let response = match api.chat.diagnostics(scope) {
                        Err(error) => error_response(Some(envelope.command_id), submit_code(&error), error),
                        Ok(access) => match access.errors(limit.unwrap_or(16).clamp(1, 64)) {
                            Err(error) => error_response(Some(envelope.command_id), diagnostic_code(&error), error),
                            Ok(errors) => json!({"type":"diagnostics_errors", "command_id":envelope.command_id,
                                "character":character, "errors":errors}),
                        },
                    };
                    send(&outbound, response, &stop).await?;
                }
                Command::Settings {} => {
                    let response = match api.settings.snapshot_all() {
                        Ok(snapshot) => json!({"type":"settings","command_id":envelope.command_id,"revision":snapshot.revision,"settings":snapshot.values}),
                        Err(error) => error_response(Some(envelope.command_id), settings_code(&error), error),
                    };
                    send(&outbound, response, &stop).await?;
                }
                Command::SetVolume { value } => {
                    let result = api.settings.edit::<VoiceSettings>(|voice| voice.volume = value);
                    let response = match result { Ok(snapshot) => json!({"type":"settings_applied","command_id":envelope.command_id,"revision":snapshot.revision}), Err(error) => error_response(Some(envelope.command_id), settings_code(&error), error) };
                    send(&outbound, response, &stop).await?;
                }
            }
        }
        Ok(())
    }.await;
    stop.cancel();
    observers.abort_all();
    while observers.join_next().await.is_some() {}
    drop(outbound);
    let written = writer.await.map_err(io::Error::other)?;
    result.and(written)
}

pub fn status(snapshot: &RequestSnapshot) -> Value {
    status_with_audio(snapshot, true)
}

pub(crate) fn status_with_audio(snapshot: &RequestSnapshot, include_audio: bool) -> Value {
    let mut message = json!({"type":"request_status","request_id":snapshot.metadata.id.0,"character":snapshot.metadata.character_id.to_string(),"revision":snapshot.revision,"can_cancel":snapshot.can_cancel()});
    message["dataset_sample_id"] = json!(snapshot.dataset.sample_id);
    if let Some(warning) = &snapshot.dataset.warning {
        message["dataset_warning"] = json!(warning);
    }
    match &snapshot.state {
        RequestState::Pending => message["state"] = json!("pending"),
        RequestState::Processing(stage) => {
            message["state"] = json!("processing");
            match stage {
                ProcessingStage::GeneratingText => message["stage"] = json!("text"),
                ProcessingStage::TextReady { .. } => message["stage"] = json!("text_ready"),
                ProcessingStage::GeneratingVoice { .. } => message["stage"] = json!("voice"),
                ProcessingStage::Accepting { .. } => message["stage"] = json!("accepting"),
            }
            if let Some(text) = snapshot.provisional_text() {
                message["text"] = json!(text);
                message["provisional"] = json!(true);
            }
        }
        RequestState::Cancelled => message["state"] = json!("cancelled"),
        RequestState::Finished(outcome) => {
            message["state"] = json!("finished");
            match outcome {
                RequestOutcome::Success(response) => {
                    message["outcome"] = json!("success");
                    message["text"] = json!(response.text);
                    message["event_id"] = json!(response.event_id);
                    if include_audio && let Some(audio) = &response.audio {
                        message["audio"] = json!({"format":"wave","bytes":audio.bytes.as_ref()});
                    }
                }
                RequestOutcome::VoiceGenerationFailed { response, error } => {
                    message["outcome"] = json!("voice_error");
                    message["text"] = json!(response.text);
                    message["event_id"] = json!(response.event_id);
                    message["error"] = json!(error.to_string());
                }
                RequestOutcome::TextGenerationFailed(error) => {
                    message["outcome"] = json!("text_error");
                    message["error"] = json!(error.to_string());
                }
                RequestOutcome::InternalError(error) => {
                    message["outcome"] = json!("internal_error");
                    message["error"] = json!(error.message);
                    message["partial_text"] = json!(error.partial_text);
                }
            }
        }
    }
    message
}
