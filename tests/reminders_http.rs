use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    reminders::{CreateReminder, DueSpec, ManualClock, ReminderConfig, RemindersApi, UtcInstant},
    request::{
        ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState, UserInput,
    },
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

const SECRET: &str = "секрет-только-для-Миты";
const PLAIN: &str = r#"{"segments":[{"text":"Хорошо."}]}"#;
const ADD: &str = r#"{"segments":[{"text":"Поставила напоминание."}],"tool_call":{"name":"reminder","args":{"action":"add","text":"секрет-только-для-Миты","due":"через 2 часа"}}}"#;
const LIST: &str = r#"{"segments":[{"text":"Проверяю напоминания."}],"tool_call":{"name":"reminder","args":{"action":"list"}}}"#;
const UPDATE_DELETE: &str = r#"{"segments":[{"text":"Обновляю и удаляю."}],"reminder_delete":["2"],"tool_call":{"name":"reminder","args":{"action":"update","n":1,"text":"first updated","due":"через 3 часа"}}}"#;

struct Call {
    path: String,
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
            .expect("provider call")
            .expect("provider call channel")
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
    let Ok(text) = response.await else {
        return;
    };
    let body = json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": text}]},
            "finishReason": "STOP"
        }]
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        request_queue: NonZeroUsize::new(4).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(32).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(32).unwrap(),
    }
}

async fn start_app(endpoint: &str, clock: Arc<ManualClock>) -> ApplicationLifecycle {
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        preset(endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let mut reminders = ReminderConfig::system();
    reminders.clock = clock;
    reminders.start_scheduler = false;
    config.reminders = reminders;
    ApplicationLifecycle::start(config).await.unwrap()
}

fn request(character: CharacterId, text: &str) -> RequestHandle {
    let event =
        ChatApi::publish_message(character, UserInput::new(text.to_owned()).unwrap()).unwrap();
    ChatApi::reply(ReplyRequest::new(
        character,
        event,
        RequestSource::Presenter,
    ))
    .unwrap()
}

async fn finish(handle: RequestHandle) -> RequestState {
    timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("request finished")
        .unwrap()
        .state
        .clone()
}

fn assert_accepted(state: RequestState) {
    match state {
        RequestState::Finished(RequestOutcome::Success(_))
        | RequestState::Finished(RequestOutcome::VoiceGenerationFailed { .. }) => {}
        other => panic!("expected accepted response, got {other:?}"),
    }
}

fn prompt_text(body: &Value) -> String {
    body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn now() -> UtcInstant {
    UtcInstant::parse_rfc3339("2026-11-03T09:00:00Z").unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_add_uses_fixture_schema_persists_due_and_stays_private_from_neighbor() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let clock = Arc::new(ManualClock::new(now()));
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, Arc::clone(&clock)).await;

    let first = request(CharacterId::Crazy, "поставь напоминание");
    let call = server.next().await;
    assert!(call.path.contains(":generateContent?key=test-key "));
    let schema = &call.body["generationConfig"]["responseSchema"]["properties"];
    for field in ["reminder_add", "timer_add", "reminder_delete", "tool_call"] {
        assert!(schema.get(field).is_some(), "missing schema field {field}");
    }
    assert_eq!(
        schema["tool_call"]["properties"]["args"]["properties"]["action"]["enum"],
        json!(["list", "add", "timer", "delete", "update"])
    );
    call.respond.send(ADD).unwrap();
    assert_accepted(finish(first).await);

    let reminders = RemindersApi::list(CharacterId::Crazy).await.unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].text, SECRET);
    assert_eq!(
        reminders[0].due_utc,
        clock
            .current()
            .checked_add(Duration::from_secs(2 * 60 * 60))
            .unwrap()
    );

    let own = request(CharacterId::Crazy, "покажи pending");
    let own_call = server.next().await;
    let own_prompt = prompt_text(&own_call.body);
    assert!(own_prompt.contains("[PENDING REMINDERS]"));
    assert!(own_prompt.contains(SECRET));
    own_call.respond.send(PLAIN).unwrap();
    assert_accepted(finish(own).await);

    let neighbor = request(CharacterId::Kind, "чужой prompt");
    let neighbor_call = server.next().await;
    let neighbor_prompt = prompt_text(&neighbor_call.body);
    assert!(!neighbor_prompt.contains(SECRET));
    assert!(!neighbor_prompt.contains("[PENDING REMINDERS]"));
    neighbor_call.respond.send(PLAIN).unwrap();
    assert_accepted(finish(neighbor).await);

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_list_result_is_really_present_in_the_next_prompt() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let clock = Arc::new(ManualClock::new(now()));
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, Arc::clone(&clock)).await;

    RemindersApi::create(CreateReminder::new(
        CharacterId::Crazy,
        "list-secret",
        DueSpec::after(Duration::from_secs(3600)).unwrap(),
    ))
    .await
    .unwrap();

    let listed = request(CharacterId::Crazy, "проверь список");
    let call = server.next().await;
    call.respond.send(LIST).unwrap();
    assert_accepted(finish(listed).await);

    let next = request(CharacterId::Crazy, "после list");
    let next_call = server.next().await;
    let prompt = prompt_text(&next_call.body);
    assert!(prompt.contains("Активные напоминания: 1"), "{prompt}");
    assert!(prompt.contains("list-secret"), "{prompt}");
    next_call.respond.send(PLAIN).unwrap();
    assert_accepted(finish(next).await);

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_update_and_delete_commit_together() {
    let _guard = CORE_TEST_MUTEX.lock().await;
    let clock = Arc::new(ManualClock::new(now()));
    let mut server = GeminiServer::start().await;
    let app = start_app(&server.endpoint, Arc::clone(&clock)).await;

    for text in ["first", "second"] {
        RemindersApi::create(CreateReminder::new(
            CharacterId::Crazy,
            text,
            DueSpec::after(Duration::from_secs(3600)).unwrap(),
        ))
        .await
        .unwrap();
    }

    let changed = request(CharacterId::Crazy, "обнови и удали");
    let call = server.next().await;
    call.respond.send(UPDATE_DELETE).unwrap();
    assert_accepted(finish(changed).await);
    let reminders = RemindersApi::list(CharacterId::Crazy).await.unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].number, 1);
    assert_eq!(reminders[0].text, "first updated");
    assert_eq!(
        reminders[0].due_utc,
        clock
            .current()
            .checked_add(Duration::from_secs(3 * 60 * 60))
            .unwrap()
    );

    app.shutdown().await.unwrap();
    server.shutdown().await;
}
