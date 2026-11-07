//! Game-origin command and result channel. A session survives TCP reconnects for
//! the lifetime of the application; request state is owned by RequestHandle.
use crate::{
    character::CharacterId,
    game_context::GameRuntimeContext,
    identity::EventId,
    request::{
        InitiateRequest, ReactRequest, ReplyRequest, RequestHandle, RequestOutcome, RequestSource,
        RequestState,
    },
    runtime::ApiBindings,
    timeline::{
        conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
        domain::EventKind,
        ingress::{SemanticBatch, SemanticEvent},
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct GameLinkConfig {
    pub bind: SocketAddr,
    pub max_command_bytes: usize,
    pub outbound_capacity: usize,
    pub max_sessions: usize,
    pub max_requests_per_session: usize,
    pub disconnected_session_ttl: Duration,
    pub max_events_per_batch: usize,
    pub max_connections: usize,
    pub handshake_timeout: Duration,
}
impl GameLinkConfig {
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            max_command_bytes: 65536,
            outbound_capacity: 64,
            max_sessions: 16,
            max_requests_per_session: 4096,
            disconnected_session_ttl: Duration::from_secs(1800),
            max_events_per_batch: 64,
            max_connections: 32,
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone)]
pub struct GameLinkHandle {
    address: SocketAddr,
}
impl GameLinkHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

struct Session {
    token: Uuid,
    connection: Option<(Uuid, mpsc::Sender<Value>, CancellationToken)>,
    disconnected_at: Option<Instant>,
    receipts: HashMap<Uuid, (Value, Receipt)>,
    context: Arc<GameRuntimeContext>,
    playback: HashMap<Uuid, PlaybackReport>,
    deliveries: HashMap<Uuid, DeliveryAck>,
    ptt: Option<PttReport>,
}
#[derive(Clone)]
enum Receipt {
    Request(RequestHandle),
    Published(EventId),
    Events(Vec<EventId>),
    Ack(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaybackPhase {
    Started,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct PlaybackReport {
    playback_id: Uuid,
    phase: PlaybackPhase,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct PttReport {
    capture_id: Uuid,
    generation: u64,
    active: bool,
    cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DeliveryAck {
    event_id: Uuid,
    applied: bool,
    error: Option<String>,
}

#[derive(Default)]
struct Sessions(Mutex<HashMap<Uuid, Session>>);
impl Sessions {
    fn create(
        &self,
        connection: Uuid,
        outbound: mpsc::Sender<Value>,
        disconnect: CancellationToken,
        max: usize,
        ttl: Duration,
    ) -> Result<(Uuid, Uuid), &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        sessions.retain(|_, session| {
            let expired = session
                .disconnected_at
                .is_some_and(|at| at.elapsed() >= ttl);
            if expired {
                for (_, receipt) in session.receipts.values() {
                    if let Receipt::Request(handle) = receipt {
                        handle.cancel();
                    }
                }
            }
            !expired
        });
        if sessions.len() >= max {
            return Err("game session capacity reached");
        }
        let id = Uuid::new_v4();
        let token = Uuid::new_v4();
        sessions.insert(
            id,
            Session {
                token,
                connection: Some((connection, outbound, disconnect)),
                disconnected_at: None,
                receipts: HashMap::new(),
                context: Arc::new(GameRuntimeContext::default()),
                playback: HashMap::new(),
                deliveries: HashMap::new(),
                ptt: None,
            },
        );
        Ok((id, token))
    }
    fn resume(
        &self,
        id: Uuid,
        token: Uuid,
        connection: Uuid,
        outbound: mpsc::Sender<Value>,
        disconnect: CancellationToken,
        ttl: Duration,
    ) -> Result<Vec<RequestHandle>, &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("unknown session")?;
        if session.token != token {
            return Err("invalid session credential");
        }
        if session
            .disconnected_at
            .is_some_and(|at| at.elapsed() >= ttl)
        {
            return Err("session expired");
        }
        if let Some((_, _, old_disconnect)) = session.connection.take() {
            old_disconnect.cancel();
        }
        session.connection = Some((connection, outbound, disconnect));
        session.disconnected_at = None;
        Ok(session
            .receipts
            .values()
            .filter_map(|(_, receipt)| match receipt {
                Receipt::Request(handle) => Some(handle.clone()),
                _ => None,
            })
            .collect())
    }
    fn is_active(&self, id: Uuid, connection: Uuid) -> bool {
        self.0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)
            .and_then(|s| s.connection.as_ref())
            .is_some_and(|(active, _, _)| *active == connection)
    }
    fn can_record(&self, id: Uuid, max: usize) -> Result<(), &'static str> {
        let sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get(&id).ok_or("session not found")?;
        if session.receipts.len() >= max {
            return Err("session receipt capacity reached");
        }
        Ok(())
    }
    fn clear(&self, id: Uuid, connection: Uuid) {
        if let Some(session) = self.0.lock().expect("game sessions poisoned").get_mut(&id)
            && session
                .connection
                .as_ref()
                .is_some_and(|(active, _, _)| *active == connection)
        {
            session.connection = None;
            session.disconnected_at = Some(Instant::now());
        }
    }
    fn context(&self, id: Uuid) -> Result<Arc<GameRuntimeContext>, &'static str> {
        self.0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)
            .map(|s| Arc::clone(&s.context))
            .ok_or("session not found")
    }
    fn update_context(&self, id: Uuid, new: GameRuntimeContext) -> Result<(), &'static str> {
        new.validate()?;
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if new.revision <= session.context.revision {
            return Err("game context revision must increase");
        }
        session.context = Arc::new(new);
        Ok(())
    }
    fn report_ptt(&self, id: Uuid, report: PttReport) -> Result<(), &'static str> {
        if report.cancelled && report.active {
            return Err("cancelled capture cannot be active");
        }
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if let Some(previous) = session.ptt {
            if previous.capture_id == report.capture_id && report.generation < previous.generation {
                return Err("stale capture generation");
            }
            if previous.capture_id != report.capture_id && previous.active {
                return Err("previous capture is still active");
            }
        }
        session.ptt = Some(report);
        Ok(())
    }
    fn report_playback(
        &self,
        id: Uuid,
        request_id: Uuid,
        report: PlaybackReport,
    ) -> Result<(), &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if let Some(previous) = session.playback.get(&request_id) {
            if previous.playback_id != report.playback_id {
                return Err("request already has playback identity");
            }
            if previous.phase != report.phase && previous.phase != PlaybackPhase::Started {
                return Err("playback already reached terminal state");
            }
            if previous.phase != report.phase && report.phase == PlaybackPhase::Started {
                return Err("playback cannot restart");
            }
        } else if report.phase != PlaybackPhase::Started {
            return Err("playback must start first");
        }
        session.playback.insert(request_id, report);
        Ok(())
    }
    fn acknowledge_result(
        &self,
        id: Uuid,
        request_id: Uuid,
        ack: DeliveryAck,
    ) -> Result<(), &'static str> {
        if ack.error.as_ref().is_some_and(|reason| reason.len() > 1024)
            || (ack.applied && ack.error.is_some())
        {
            return Err("invalid delivery acknowledgment");
        }
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if session.deliveries.contains_key(&request_id) {
            return Err("request result was already acknowledged");
        }
        session.deliveries.insert(request_id, ack);
        Ok(())
    }
    fn snapshots(&self, id: Uuid) -> Value {
        let sessions = self.0.lock().expect("game sessions poisoned");
        let session = &sessions[&id];
        json!({
            "type":"session_state","session_id":id,
            "context_revision":session.context.revision,"ptt":session.ptt,
            "playback":session.playback.iter().map(|(request,report)|json!({"request_id":request,"report":report})).collect::<Vec<_>>(),
            "deliveries":session.deliveries.iter().map(|(request,ack)|json!({"request_id":request,"ack":ack})).collect::<Vec<_>>(),
        })
    }
    fn request(&self, id: Uuid, request_id: Uuid) -> Option<RequestHandle> {
        self.0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)?
            .receipts
            .values()
            .find_map(|(_, receipt)| match receipt {
                Receipt::Request(h) if h.id().0 == request_id => Some(h.clone()),
                _ => None,
            })
    }
    fn receipt(&self, id: Uuid, command: Uuid) -> Option<(Value, Receipt)> {
        self.0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)?
            .receipts
            .get(&command)
            .cloned()
    }
    fn record(
        &self,
        id: Uuid,
        command: Uuid,
        fingerprint: Value,
        result: Receipt,
        max: usize,
    ) -> Result<(), &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if session.receipts.len() >= max {
            return Err("session receipt capacity reached");
        }
        session.receipts.insert(command, (fingerprint, result));
        Ok(())
    }
    async fn broadcast(&self, frame: Value) {
        let sessions: Vec<Uuid> = self
            .0
            .lock()
            .expect("game sessions poisoned")
            .iter()
            .filter_map(|(id, session)| session.connection.as_ref().map(|_| *id))
            .collect();
        for session in sessions {
            self.push(session, frame.clone()).await;
        }
    }
    async fn push(&self, id: Uuid, frame: Value) {
        let delivery = self
            .0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)
            .and_then(|s| s.connection.as_ref())
            .map(|(_, output, disconnect)| (output.clone(), disconnect.clone()));
        if let Some((output, disconnect)) = delivery
            && output.try_send(frame).is_err()
        {
            disconnect.cancel();
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GameEventKind {
    UnityEvent,
    GameStarted,
    GameAction,
    GameEnded,
    CharacterJoined,
    CharacterLeft,
    PlayerInactive,
    ProactiveOpportunity,
}
impl From<GameEventKind> for EventKind {
    fn from(kind: GameEventKind) -> Self {
        match kind {
            GameEventKind::UnityEvent => Self::UnityEvent,
            GameEventKind::GameStarted => Self::GameStarted,
            GameEventKind::GameAction => Self::GameAction,
            GameEventKind::GameEnded => Self::GameEnded,
            GameEventKind::CharacterJoined => Self::CharacterJoined,
            GameEventKind::CharacterLeft => Self::CharacterLeft,
            GameEventKind::PlayerInactive => Self::PlayerInactive,
            GameEventKind::ProactiveOpportunity => Self::ProactiveOpportunity,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameWorldEvent {
    event_id: Uuid,
    kind: GameEventKind,
    occurred_at_unix_ms: u64,
    #[serde(default)]
    actor: Option<String>,
    observers: Vec<CharacterId>,
    payload: serde_json::Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum GameCommand {
    Hello {},
    Resume {
        session_id: Uuid,
        session_token: Uuid,
    },
    Ping {},
    GetGameSettings {},
    PublishMessage {
        event_id: Uuid,
        occurred_at_unix_ms: u64,
        content: Value,
        character: CharacterId,
    },
    PublishEvents {
        events: Vec<GameWorldEvent>,
    },
    Reply {
        character: CharacterId,
        message_id: Uuid,
    },
    React {
        character: CharacterId,
        event_id: Uuid,
        #[serde(default)]
        instruction: String,
    },
    Initiate {
        character: CharacterId,
        #[serde(default)]
        instruction: String,
    },
    Cancel {
        request_id: Uuid,
    },
    UpdateContext {
        context: GameRuntimeContext,
    },
    ReportPtt {
        capture_id: Uuid,
        generation: u64,
        active: bool,
        #[serde(default)]
        cancelled: bool,
    },
    ReportPlayback {
        request_id: Uuid,
        playback_id: Uuid,
        phase: PlaybackPhase,
    },
    ReadAudioChunk {
        request_id: Uuid,
        offset: usize,
        length: usize,
    },
    AcknowledgeResult {
        request_id: Uuid,
        event_id: Uuid,
        applied: bool,
        #[serde(default)]
        error: Option<String>,
    },
}
#[derive(Deserialize)]
struct Frame {
    command_id: Uuid,
    #[serde(flatten)]
    command: GameCommand,
}

fn game_settings_frame(
    snapshot: &crate::settings::SettingsSnapshot,
    command_id: Option<Uuid>,
    kind: &str,
) -> Value {
    let voice = &snapshot.values.voice;
    json!({"type":kind,"command_id":command_id,"revision":snapshot.revision,"voice":{"enabled":voice.in_game,"volume":voice.volume,"profile":voice.voice}})
}

fn error(command_id: Option<Uuid>, code: &str, reason: impl std::fmt::Display) -> Value {
    json!({"type":"error", "command_id":command_id, "code":code, "error":reason.to_string()})
}
fn request_frame(snapshot: &crate::request::RequestSnapshot, session_id: Uuid) -> Value {
    let mut frame = crate::server::status_with_audio(snapshot, false);
    frame["session_id"] = json!(session_id);
    frame["type"] = json!(match &snapshot.state {
        RequestState::Cancelled => "request_cancelled",
        RequestState::Finished(
            crate::request::RequestOutcome::TextGenerationFailed(_)
            | crate::request::RequestOutcome::InternalError(_),
        ) => "request_failed",
        RequestState::Finished(_) => "request_completed",
        _ => "request_updated",
    });
    if let RequestState::Finished(
        RequestOutcome::Success(response) | RequestOutcome::VoiceGenerationFailed { response, .. },
    ) = &snapshot.state
    {
        frame["segments"] = json!(response.segments.as_ref());
        if let Some(audio) = &response.audio {
            let hash = digest(&SHA256, &audio.bytes);
            let sha = hash
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();

            frame["audio_asset"] = json!({"request_id":snapshot.metadata.id.0,"format":"wave","size_bytes":audio.bytes.len(),"sha256":sha});
        }
    }
    frame
}

pub struct GameLinkLifecycle {
    listener: TcpListener,
    config: GameLinkConfig,
    sessions: Arc<Sessions>,
}
impl GameLinkLifecycle {
    pub async fn bind(config: GameLinkConfig) -> io::Result<(Self, GameLinkHandle)> {
        if !config.bind.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "GameLink requires a loopback bind address",
            ));
        }
        let listener = TcpListener::bind(config.bind).await?;
        let address = listener.local_addr()?;
        Ok((
            Self {
                listener,
                config,
                sessions: Arc::new(Sessions::default()),
            },
            GameLinkHandle { address },
        ))
    }
    pub(crate) async fn run_with_api(
        self,
        stop: CancellationToken,
        api: Arc<ApiBindings>,
    ) -> io::Result<()> {
        let mut settings_updates = api.settings.observe().map_err(io::Error::other)?;
        let mut connections = JoinSet::new();
        let permits = Arc::new(Semaphore::new(self.config.max_connections.max(1)));
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                changed = settings_updates.changed() => {
                    if changed.is_err() { break; }
                    let settings = settings_updates.borrow_and_update().clone();
                    self.sessions.broadcast(game_settings_frame(&settings, None, "game_settings_updated")).await;
                },
                result = connections.join_next(), if !connections.is_empty() => { let _ = result; },
                accepted = self.listener.accept() => {
                    let (stream,_) = accepted?;
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        drop(stream);
                        continue;
                    };
                    let sessions = Arc::clone(&self.sessions);
                    let api = Arc::clone(&api);
                    let config = self.config;
                    let connection_stop = stop.child_token();
                    connections.spawn(async move { let _permit = permit; let _ = client(stream,config,sessions,api,connection_stop).await; });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

async fn client(
    stream: TcpStream,
    config: GameLinkConfig,
    sessions: Arc<Sessions>,
    api: Arc<ApiBindings>,
    stop: CancellationToken,
) -> io::Result<()> {
    let connection = Uuid::new_v4();
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Value>(config.outbound_capacity.max(1));
    let connection_stop = stop.child_token();
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        loop {
            let frame = tokio::select! { biased; _ = writer_stop.cancelled() => break, value=rx.recv()=> match value {Some(value)=>value,None=>break} };
            let mut bytes = serde_json::to_vec(&frame).map_err(io::Error::other)?;
            bytes.push(b'\n');
            write.write_all(&bytes).await?;
        }
        Ok::<(), io::Error>(())
    });
    let mut reader = BufReader::new(read);
    let mut session_id = None;
    let deadline = tokio::time::Instant::now() + config.handshake_timeout;
    let result = async {
        loop {
            let line = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = connection_stop.cancelled() => break,
                _ = tokio::time::sleep_until(deadline), if session_id.is_none() => {
                    tx.send(error(None,"handshake_timeout","game session handshake timed out")).await.map_err(io::Error::other)?;
                    break;
                },
                incoming = read_bounded_line(&mut reader, config.max_command_bytes) => match incoming {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::InvalidData && e.to_string().contains("exceeds configured limit") => {
                        tx.send(error(None,"command_too_large",e)).await.map_err(io::Error::other)?;
                        break;
                    },
                    Err(e) => return Err(e),
                },
            };
            let frame: Frame = match serde_json::from_str(&line) {
                Ok(frame) => frame,
                Err(e) => { tx.send(error(None,"invalid_command",e)).await.map_err(io::Error::other)?; continue; }
            };
            let id = frame.command_id;
            match frame.command {
                GameCommand::Hello {} => {
                    if session_id.is_some() { tx.send(error(Some(id),"already_connected","session already established")).await.map_err(io::Error::other)?; continue; }
                    match sessions.create(connection,tx.clone(),connection_stop.clone(),config.max_sessions,config.disconnected_session_ttl) {
                        Ok((session,token)) => { session_id=Some(session); tx.send(json!({"type":"hello_ack","command_id":id,"session_id":session,"session_token":token})).await.map_err(io::Error::other)?; },
                        Err(e)=>tx.send(error(Some(id),"capacity",e)).await.map_err(io::Error::other)?,
                    }
                }
                GameCommand::Resume {session_id: target, session_token} => {
                    if session_id.is_some() { tx.send(error(Some(id),"already_connected","session already established")).await.map_err(io::Error::other)?; continue; }
                    match sessions.resume(target,session_token,connection,tx.clone(),connection_stop.clone(),config.disconnected_session_ttl) {
                        Err(e) => tx.send(error(Some(id),"session_denied",e)).await.map_err(io::Error::other)?,
                        Ok(handles) => {
                            session_id=Some(target);
                            tx.send(json!({"type":"session_resumed","command_id":id,"session_id":target,"requests":handles.len()})).await.map_err(io::Error::other)?;
                            tx.send(sessions.snapshots(target)).await.map_err(io::Error::other)?;
                            for handle in handles { tx.send(request_frame(&handle.snapshot(),target)).await.map_err(io::Error::other)?; }
                        }
                    }
                }
                GameCommand::Ping {} => { tx.send(json!({"type":"pong","command_id":id})).await.map_err(io::Error::other)?; }
                command => {
                    let Some(session) = session_id else { tx.send(error(Some(id),"hello_required","establish or resume game session first")).await.map_err(io::Error::other)?; continue; };
                    if !sessions.is_active(session, connection) {
                        tx.send(error(Some(id),"session_replaced","another connection owns this session")).await.map_err(io::Error::other)?;
                        break;
                    }
                    let fingerprint = serde_json::to_value(&command).map_err(io::Error::other)?;
                    if let Some((previous,receipt)) = sessions.receipt(session,id) {
                        if previous != fingerprint { tx.send(error(Some(id),"command_id_conflict","command ID has different payload")).await.map_err(io::Error::other)?; }
                        else { match receipt {
                            Receipt::Published(event)=> tx.send(json!({"type":"published","command_id":id,"event_id":event.0,"replayed":true})).await.map_err(io::Error::other)?,
                            Receipt::Events(events)=> tx.send(json!({"type":"events_published","command_id":id,"event_ids":events.iter().map(|e|e.0).collect::<Vec<_>>(),"replayed":true})).await.map_err(io::Error::other)?,
                            Receipt::Request(handle)=> { tx.send(json!({"type":"request_accepted","command_id":id,"request_id":handle.id().0,"replayed":true})).await.map_err(io::Error::other)?; tx.send(request_frame(&handle.snapshot(),session)).await.map_err(io::Error::other)?; },
                            Receipt::Ack(frame)=>tx.send(frame).await.map_err(io::Error::other)?,
                        }}
                        continue;
                    }
                    match command {
                        GameCommand::PublishMessage {event_id,occurred_at_unix_ms,content,character} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            let mut message = ConversationMessage::new(event_id.to_string(),ConversationSpeaker::Player,content,[character]);
                            message.id=EventId(event_id);
                            message.occurred_at_unix_ms=occurred_at_unix_ms;
                            message.audience=ConversationAudience::player_to(character);
                            match api.chat.publish_conversation_message(message) {
                                Err(e)=>tx.send(error(Some(id),"publish_failed",e)).await.map_err(io::Error::other)?,
                                Ok(event)=>match sessions.record(session,id,fingerprint,Receipt::Published(event),config.max_requests_per_session) {
                                    Err(e)=>tx.send(error(Some(id),"capacity",e)).await.map_err(io::Error::other)?,
                                    Ok(())=>tx.send(json!({"type":"published","command_id":id,"event_id":event.0,"replayed":false})).await.map_err(io::Error::other)?,
                                }
                            }
                        }
                        GameCommand::PublishEvents {events} => {
                            if events.is_empty() || events.len() > config.max_events_per_batch {
                                tx.send(error(Some(id),"invalid_events","invalid event batch size")).await.map_err(io::Error::other)?;
                                continue;
                            }
                            if events.iter().any(|e| e.observers.is_empty() || e.observers.len()>9 || e.actor.as_deref().is_some_and(|a|a.len()>256)) {
                                tx.send(error(Some(id),"invalid_events","invalid event observers or actor")).await.map_err(io::Error::other)?;
                                continue;
                            }
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            let batch = SemanticBatch::new(format!("game:{session}:{id}"), events.into_iter().map(|e| {
                                SemanticEvent {
                                    id: EventId(e.event_id), kind: e.kind.into(), actor: e.actor,
                                    observers: e.observers.into_iter().collect(), payload: e.payload,
                                    occurred_at_unix_ms: e.occurred_at_unix_ms,
                                }
                            }).collect());
                            let result = api.timeline.lock().map_err(|_| io::Error::other("timeline poisoned"))
                                .and_then(|mut timeline| {
                                    let selected = timeline.selection();
                                    timeline.ingest_semantic_events(selected,batch).map_err(io::Error::other)
                                });
                            match result {
                                Err(e)=> tx.send(error(Some(id),"publish_failed",e)).await.map_err(io::Error::other)?,
                                Ok(facts)=> {
                                    let ids = facts.into_iter().map(|f| f.draft.id).collect::<Vec<_>>();
                                    sessions.record(session,id,fingerprint,Receipt::Events(ids.clone()),config.max_requests_per_session).map_err(io::Error::other)?;
                                    tx.send(json!({"type":"events_published","command_id":id,"event_ids":ids.iter().map(|e|e.0).collect::<Vec<_>>(),"replayed":false})).await.map_err(io::Error::other)?;
                                }
                            }
                        }
                        GameCommand::Reply {character,message_id} | GameCommand::React {character,event_id:message_id,instruction:_} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            let source = RequestSource::Game(session);
                            let game_context = sessions.context(session).map_err(io::Error::other)?;
                            let result = match command {
                                GameCommand::Reply {..} => api.chat.send(ReplyRequest::new(character,EventId(message_id),source).with_game_context(Arc::clone(&game_context))),
                                GameCommand::React {instruction,..} => api.chat.send(ReactRequest::new(character,EventId(message_id),instruction,source).with_game_context(game_context)),
                                _=> unreachable!(),
                            };
                            accept_request(result,session,id,fingerprint,config.max_requests_per_session,&sessions,&tx).await?;
                        }
                        GameCommand::Initiate {character,instruction} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            accept_request(api.chat.send(InitiateRequest::new(character,instruction,RequestSource::Game(session)).with_game_context(sessions.context(session).map_err(io::Error::other)?)),session,id,fingerprint,config.max_requests_per_session,&sessions,&tx).await?;
                        }
                        GameCommand::Cancel {request_id} => {
                            let handle = sessions.0.lock().expect("game sessions poisoned").get(&session).and_then(|s| s.receipts.values().find_map(|(_,receipt)| match receipt {Receipt::Request(h) if h.id().0==request_id=>Some(h.clone()),_=>None}));
                            match handle {
                                Some(h)=>tx.send(json!({"type":"cancel_ack","command_id":id,"request_id":request_id,"result":format!("{:?}",h.cancel())})).await.map_err(io::Error::other)?,
                                None=>tx.send(error(Some(id),"ownership_denied","request is not owned by game session")).await.map_err(io::Error::other)?,
                            }
                        }
                        GameCommand::GetGameSettings {} => {
                            match api.settings.snapshot_all() {
                                Ok(snapshot) => tx.send(game_settings_frame(&snapshot, Some(id), "game_settings")).await.map_err(io::Error::other)?,
                                Err(reason) => tx.send(error(Some(id),"settings_unavailable",reason)).await.map_err(io::Error::other)?,
                            }
                        }
                        GameCommand::UpdateContext {context} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            let revision = context.revision;
                            match sessions.update_context(session,context) {
                                Err(reason)=>tx.send(error(Some(id),"context_rejected",reason)).await.map_err(io::Error::other)?,
                                Ok(())=> {
                                    let ack = json!({"type":"context_applied","command_id":id,"revision":revision});
                                    sessions.record(session,id,fingerprint,Receipt::Ack(ack.clone()),config.max_requests_per_session).map_err(io::Error::other)?;
                                    tx.send(ack).await.map_err(io::Error::other)?;
                                }
                            }
                        }
                        GameCommand::ReportPtt {capture_id,generation,active,cancelled} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            match sessions.report_ptt(session,PttReport {capture_id,generation,active,cancelled}) {
                                Err(reason)=>tx.send(error(Some(id),"ptt_rejected",reason)).await.map_err(io::Error::other)?,
                                Ok(())=> {
                                    let ack=json!({"type":"ptt_accepted","command_id":id,"capture_id":capture_id,"generation":generation});
                                    sessions.record(session,id,fingerprint,Receipt::Ack(ack.clone()),config.max_requests_per_session).map_err(io::Error::other)?;
                                    tx.send(ack).await.map_err(io::Error::other)?;
                                }
                            }
                        }
                        GameCommand::ReportPlayback {request_id,playback_id,phase} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            match sessions.request(session,request_id) {
                                None => tx.send(error(Some(id),"ownership_denied","unknown game request")).await.map_err(io::Error::other)?,
                                Some(handle) => {
                                    let snapshot=handle.snapshot();
                                    let audio_ready=matches!(&snapshot.state,RequestState::Finished(RequestOutcome::Success(response)) if response.audio.is_some());
                                    if !audio_ready { tx.send(error(Some(id),"audio_unavailable","request has no completed audio asset")).await.map_err(io::Error::other)?; continue; }
                                    match sessions.report_playback(session,request_id,PlaybackReport {playback_id,phase}) {
                                        Err(reason)=>tx.send(error(Some(id),"playback_rejected",reason)).await.map_err(io::Error::other)?,
                                        Ok(())=> {
                                            let ack=json!({"type":"playback_accepted","command_id":id,"request_id":request_id,"phase":phase});
                                            sessions.record(session,id,fingerprint,Receipt::Ack(ack.clone()),config.max_requests_per_session).map_err(io::Error::other)?;
                                            tx.send(ack).await.map_err(io::Error::other)?;
                                        }
                                    }
                                }
                            }
                        }
                        GameCommand::AcknowledgeResult {request_id,event_id,applied,error:reason} => {
                            if let Err(reason) = sessions.can_record(session,config.max_requests_per_session) {
                                tx.send(error(Some(id),"capacity",reason)).await.map_err(io::Error::other)?;
                                continue;
                            }
                            let Some(handle) = sessions.request(session,request_id) else {
                                tx.send(error(Some(id),"ownership_denied","unknown game request")).await.map_err(io::Error::other)?; continue;
                            };
                            let snapshot = handle.snapshot();
                            let accepted_event = match &snapshot.state {
                                RequestState::Finished(RequestOutcome::Success(response) | RequestOutcome::VoiceGenerationFailed { response, .. }) => Some(response.event_id.0),
                                _ => None,
                            };
                            if accepted_event != Some(event_id) {
                                tx.send(error(Some(id),"result_not_accepted","request has no matching accepted event")).await.map_err(io::Error::other)?; continue;
                            }
                            match sessions.acknowledge_result(session,request_id,DeliveryAck {event_id,applied,error:reason}) {
                                Err(reason)=>tx.send(error(Some(id),"delivery_rejected",reason)).await.map_err(io::Error::other)?,
                                Ok(())=>{
                                    let ack=json!({"type":"result_acknowledged","command_id":id,"request_id":request_id,"event_id":event_id,"applied":applied});
                                    sessions.record(session,id,fingerprint,Receipt::Ack(ack.clone()),config.max_requests_per_session).map_err(io::Error::other)?;
                                    tx.send(ack).await.map_err(io::Error::other)?;
                                }
                            }
                        }
                        GameCommand::ReadAudioChunk {request_id,offset,length} => {
                            if length == 0 || length > 16_384 { tx.send(error(Some(id),"invalid_chunk","chunk length must be 1..16384")).await.map_err(io::Error::other)?; continue; }
                            let Some(handle)=sessions.request(session,request_id) else {
                                tx.send(error(Some(id),"ownership_denied","unknown game request")).await.map_err(io::Error::other)?; continue;
                            };
                            let snapshot=handle.snapshot();
                            let RequestState::Finished(RequestOutcome::Success(response))=&snapshot.state else {
                                tx.send(error(Some(id),"audio_unavailable","request is not completed")).await.map_err(io::Error::other)?; continue;
                            };
                            let Some(audio)=&response.audio else {
                                tx.send(error(Some(id),"audio_unavailable","request has no audio")).await.map_err(io::Error::other)?; continue;
                            };
                            if offset > audio.bytes.len() {tx.send(error(Some(id),"invalid_chunk","offset exceeds asset size")).await.map_err(io::Error::other)?; continue;}
                            let end=offset.saturating_add(length).min(audio.bytes.len());
                            tx.send(json!({"type":"audio_chunk","command_id":id,"request_id":request_id,"offset":offset,"next_offset":end,"size_bytes":audio.bytes.len(),"base64":STANDARD.encode(&audio.bytes[offset..end])})).await.map_err(io::Error::other)?;
                        }
                        _=>unreachable!(),
                    }
                }
            }
        }
        Ok::<(),io::Error>(())
    }.await;
    if let Some(session) = session_id {
        sessions.clear(session, connection);
    }
    drop(tx);
    let _ = writer.await;
    result
}

async fn accept_request(
    result: Result<RequestHandle, crate::request::SubmitError>,
    session: Uuid,
    id: Uuid,
    fingerprint: Value,
    capacity: usize,
    sessions: &Arc<Sessions>,
    tx: &mpsc::Sender<Value>,
) -> io::Result<()> {
    let handle = match result {
        Ok(h) => h,
        Err(e) => {
            tx.send(error(Some(id), "request_rejected", e))
                .await
                .map_err(io::Error::other)?;
            return Ok(());
        }
    };
    if let Err(e) = sessions.record(
        session,
        id,
        fingerprint,
        Receipt::Request(handle.clone()),
        capacity,
    ) {
        handle.cancel();
        tx.send(error(Some(id), "capacity", e))
            .await
            .map_err(io::Error::other)?;
        return Ok(());
    }
    tx.send(json!({"type":"request_accepted","command_id":id,"request_id":handle.id().0,"replayed":false})).await.map_err(io::Error::other)?;
    let sessions = Arc::clone(sessions);
    tokio::spawn(async move {
        let mut observer = handle.observe();
        while let Ok(Some(snapshot)) = observer.next().await {
            sessions
                .push(session, request_frame(&snapshot, session))
                .await;
        }
    });
    Ok(())
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete command frame",
            ));
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |i| i + 1);
        if line.len().saturating_add(count) > limit.saturating_add(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "command exceeds configured limit",
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return String::from_utf8(line)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}
