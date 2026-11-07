use std::{collections::HashSet, num::NonZeroUsize, time::Duration};

use neuromita::{
    UserInput,
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterError, CharacterId},
    games::{
        GameApi, GameError, GamePolicy,
        seabattle::{Coordinate, Orientation},
    },
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    request::{
        CancelResult, InitiateRequest, RequestDiscovery, RequestHandle, RequestId, RequestOutcome,
        RequestSource, RequestState,
    },
    timeline::ContextPosition,
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

static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const LAYOUT: [(&str, u8); 10] = [
    ("A1", 4),
    ("F1", 3),
    ("A3", 3),
    ("E3", 2),
    ("H3", 2),
    ("A5", 2),
    ("D5", 1),
    ("F5", 1),
    ("H5", 1),
    ("J5", 1),
];

const GREETING: &str = r#"{"segments":[{"text":"Сыграем!"}]}"#;
const PLACEMENT: &str = r#"{"segments":[{"text":"Расставляюсь.","commands":["PlaceShip,A1,4,H","PlaceShip,F1,3,H","PlaceShip,A3,3,H","PlaceShip,E3,2,H","PlaceShip,H3,2,H","PlaceShip,A5,2,H","PlaceShip,D5,1,H","PlaceShip,F5,1,H","PlaceShip,H5,1,H","PlaceShip,J5,1,H"]}]}"#;
const MOVE_D5: &str = r#"{"segments":[{"text":"Д5.","commands":["MakeMove,D5"]}]}"#;
const MOVE_J10: &str = r#"{"segments":[{"text":"Мимо.","commands":["MakeMove,J10"]}]}"#;

struct Call {
    body: Value,
    respond: oneshot::Sender<&'static str>,
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
            .expect("model call")
            .expect("model call channel")
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
    let size: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
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
    calls.send(Call { body, respond }).unwrap();
    let Ok(text) = response.await else {
        return;
    };
    let body = json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"finishReason":"STOP"}]}).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn preset(endpoint: &str) -> GenerationPreset {
    let settings = GenerationSettings::new(0.7, 1024, Duration::from_secs(10)).unwrap();
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(endpoint, "test-model", "test-key", settings).unwrap(),
    ))
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(4).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(64).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(64).unwrap(),
    }
}

async fn start_app(endpoint: &str, policy: GamePolicy) -> ApplicationLifecycle {
    let generator = ResponseGenerator::new([std::sync::Arc::new(GeminiProvider::new().unwrap())
        as std::sync::Arc<dyn GenerationProvider>])
    .unwrap();
    let mut config = ApplicationConfig::minimal(
        preset(endpoint),
        generator,
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.games = policy;
    ApplicationLifecycle::start(config).await.unwrap()
}

async fn place_player_fleet(character: CharacterId) -> Vec<neuromita::timeline::EventId> {
    let mut events = Vec::new();
    for (coord, length) in LAYOUT {
        let outcome = GameApi::place_ship(
            character,
            Coordinate::parse(coord).unwrap(),
            length,
            Orientation::Horizontal,
        )
        .await
        .unwrap();
        events.extend(outcome.receipt.events);
    }
    events
}

async fn wait_done(handle: RequestHandle) -> RequestState {
    let snapshot = timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("request finished")
        .unwrap();
    snapshot.state.clone()
}

fn assert_accepted(state: RequestState) {
    match state {
        RequestState::Finished(RequestOutcome::Success(_))
        | RequestState::Finished(RequestOutcome::VoiceGenerationFailed { .. }) => {}
        other => panic!("expected an accepted response, got {other:?}"),
    }
}

async fn follow_up(
    discovery: &mut RequestDiscovery,
    seen: &mut HashSet<RequestId>,
) -> RequestHandle {
    loop {
        let handle = timeout(Duration::from_secs(10), discovery.next())
            .await
            .expect("follow-up admitted")
            .unwrap();
        if seen.insert(handle.id()) {
            return handle;
        }
    }
}

fn opponent_section(prompt: &str) -> String {
    let start = prompt
        .find("Поле противника")
        .expect("opponent board in prompt");
    let rest = &prompt[start..];
    let end = ["\nТвои выстрелы", "\nПоследний выстрел", "\nКоманды:"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn model_commands_commit_privately_and_follow_the_real_turn() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, GamePolicy::default()).await;
    let mut discovery = ChatApi::observe_requests().unwrap();
    let mut seen = HashSet::new();

    let started = GameApi::start(CharacterId::Crazy, true).await.unwrap();
    assert_eq!(started.receipt.events.len(), 1);
    let session = started.receipt.key.unwrap().session;
    let greeting = started.reaction.expect("greeting submitted").unwrap();
    seen.insert(greeting.id());
    server.next().await.respond.send(GREETING).unwrap();
    assert_accepted(wait_done(greeting).await);

    let events = place_player_fleet(CharacterId::Crazy).await;
    let placement_handle = follow_up(&mut discovery, &mut seen).await;
    let placement = server.next().await;
    assert_eq!(
        placement.body["generationConfig"]["responseSchema"]["properties"]["segments"]["items"]["properties"]
            ["commands"]["type"],
        "array"
    );
    let prompt = placement.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|content| content["parts"][0]["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("[SEA BATTLE]"));
    let visible = opponent_section(&prompt);
    assert!(!visible.contains('S'), "opponent board leaked ships");
    placement.respond.send(PLACEMENT).unwrap();
    assert_accepted(wait_done(placement_handle).await);

    let view = GameApi::view(CharacterId::Crazy).await.unwrap();
    let player_board = view.view.as_ref().expect("active match");
    assert_eq!(player_board.phase.code(), "battle");
    assert_eq!(player_board.own_board[0], "SSSS.SSS..");
    assert_eq!(player_board.own_board[2], "SSS.SS.SS.");
    assert!(
        player_board
            .opponent_board
            .iter()
            .all(|row| !row.contains('S')),
        "the human view must not reveal Mita's fleet"
    );

    let miss = GameApi::fire(CharacterId::Crazy, Coordinate::parse("J10").unwrap())
        .await
        .unwrap();
    let mut all_events = events;
    all_events.extend(miss.receipt.events.clone());
    let shot_handle = miss.reaction.expect("turn request").unwrap();
    seen.insert(shot_handle.id());
    server.next().await.respond.send(MOVE_D5).unwrap();
    assert_accepted(wait_done(shot_handle).await);

    let follow_handle = follow_up(&mut discovery, &mut seen).await;
    seen.insert(follow_handle.id());
    server.next().await.respond.send(MOVE_J10).unwrap();
    assert_accepted(wait_done(follow_handle).await);

    let after = GameApi::view(CharacterId::Crazy).await.unwrap();
    let after_view = after.view.clone().expect("active match");
    assert_eq!(after_view.session, session);
    assert_eq!(after.key.unwrap().revision, after_view.revision);
    assert_eq!(
        all_events.iter().collect::<HashSet<_>>().len(),
        all_events.len()
    );

    let other = CharacterApi::get(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let other_history = format!("{:?}", other.history.events);
    assert!(!other_history.contains("SEA BATTLE"), "{other_history}");
    assert!(!other_history.contains("J10"), "{other_history}");

    let closed = GameApi::close(CharacterId::Crazy).await.unwrap();
    assert!(closed.receipt.view.is_none());
    let close_handle = closed.reaction.expect("close reaction").unwrap();
    seen.insert(close_handle.id());
    server.next().await.respond.send(GREETING).unwrap();
    assert_accepted(wait_done(close_handle).await);

    let restart = GameApi::start(CharacterId::Crazy, true).await.unwrap();
    assert_ne!(restart.receipt.key.unwrap().session, session);
    let restart_handle = restart.reaction.expect("restart greeting").unwrap();
    seen.insert(restart_handle.id());
    server.next().await.respond.send(GREETING).unwrap();
    assert_accepted(wait_done(restart_handle).await);
    let mut every = all_events;
    every.extend(restart.receipt.events);
    assert_eq!(every.iter().collect::<HashSet<_>>().len(), every.len());

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_turn_places_the_fleet_when_auto_reactions_are_off() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, GamePolicy::default()).await;
    let mut seen = HashSet::new();

    let started = GameApi::start(CharacterId::Crazy, false).await.unwrap();
    assert!(started.reaction.is_none());
    assert!(
        timeout(Duration::from_millis(200), server.calls.recv())
            .await
            .is_err()
    );

    let _ = place_player_fleet(CharacterId::Crazy).await;
    assert!(
        timeout(Duration::from_millis(200), server.calls.recv())
            .await
            .is_err()
    );

    let manual = GameApi::request_mita_turn(CharacterId::Crazy)
        .await
        .unwrap();
    let handle = manual.reaction.expect("manual placement request").unwrap();
    seen.insert(handle.id());
    let call = server.next().await;
    assert!(
        call.body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|content| content["parts"][0]["text"].as_str())
            .any(|text| text.contains("PlaceShip"))
    );
    call.respond.send(PLACEMENT).unwrap();
    assert_accepted(wait_done(handle).await);
    let view = GameApi::view(CharacterId::Crazy).await.unwrap();
    let board = view.view.expect("active match");
    assert_eq!(board.phase.code(), "battle");
    assert_eq!(board.own_board[0], "SSSS.SSS..");

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_model_turn_applies_no_move() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, GamePolicy::default()).await;
    let mut discovery = ChatApi::observe_requests().unwrap();
    let mut seen = HashSet::new();

    GameApi::start(CharacterId::Crazy, true).await.unwrap();
    let greeting = follow_up(&mut discovery, &mut seen).await;
    server.next().await.respond.send(GREETING).unwrap();
    assert_accepted(wait_done(greeting).await);
    place_player_fleet(CharacterId::Crazy).await;
    let placement = follow_up(&mut discovery, &mut seen).await;
    server.next().await.respond.send(PLACEMENT).unwrap();
    assert_accepted(wait_done(placement).await);
    GameApi::view(CharacterId::Crazy).await.unwrap();

    let shot = GameApi::fire(CharacterId::Crazy, Coordinate::parse("J10").unwrap())
        .await
        .unwrap();
    let before = shot.receipt.key.expect("match key");
    let handle = shot.reaction.expect("reaction submitted").unwrap();
    let _pending = server.next().await;
    assert!(matches!(
        handle.cancel(),
        CancelResult::Cancelled | CancelResult::Requested
    ));
    let _ = handle.wait().await.unwrap();
    let after = GameApi::view(CharacterId::Crazy).await.unwrap();
    assert_eq!(after.key.unwrap(), before);

    app.shutdown().await.unwrap();
    let _ = seen;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn historical_generation_before_match_remains_available_while_match_is_active() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, GamePolicy::default()).await;

    let input = UserInput::new("старое сообщение".into()).unwrap();
    ChatApi::publish_message(CharacterId::Crazy, input).unwrap();
    let old = app.at();
    let input = UserInput::new("новее".into()).unwrap();
    ChatApi::publish_message(CharacterId::Crazy, input).unwrap();

    GameApi::start(CharacterId::Crazy, false).await.unwrap();

    let handle = ChatApi::initiate(
        InitiateRequest::new(CharacterId::Crazy, "продолжи", RequestSource::Presenter)
            .at(ContextPosition::At(old)),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(GREETING).unwrap();
    assert_accepted(wait_done(handle).await);
    assert!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .get_context(old)
            .await
            .unwrap()
            .game
            .is_none()
    );
    assert!(
        GameApi::view(CharacterId::Crazy)
            .await
            .unwrap()
            .view
            .is_some()
    );

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_policy_blocks_every_start_path() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let server = GeminiServer::start().await;
    let app = start_app(
        &server.endpoint,
        GamePolicy {
            enabled: false,
            auto_reactions: true,
        },
    )
    .await;

    assert!(matches!(
        GameApi::start(CharacterId::Crazy, true).await,
        Err(GameError::Disabled)
    ));
    let direct = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .game(neuromita::games::UserGameCommand::Start {
            auto_reactions: true,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        direct,
        CharacterError::InvalidGame(GameError::Disabled)
    ));

    app.shutdown().await.unwrap();
    server.shutdown().await;
}
