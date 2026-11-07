use std::{
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterContext, CharacterId},
    finetune::{
        AppendOutcome, ContentPart, ExportFormat, FineTuneCollector, FineTuneError, FineTuneSample,
        FineTuneSettings, FineTuneStore, MessageContent, MessageRole, OutcomeFilter, OutcomeRecord,
        Retention, SampleFilters, SampleId, SampleMessage, SampleOutcome, TextStage, TimeRange,
        UtcTimestamp,
        cli::{self, CliError},
    },
    generation::{
        GenerationError, GenerationInput, GenerationObserver, GenerationPreset, GenerationProvider,
        PreparedRequest, PromptMessage, PromptRole, ProviderFuture, ProviderKind, ProviderReply,
        ProviderTemplate, ProviderUsage, ResponseGenerator, StructuredResponse,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    request::{CancelResult, RequestOutcome, RequestState},
    timeline::ContextPosition,
    voice::{AudioArtifact, VoiceError, VoiceFuture, VoiceOptions, VoiceProvider},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

static APP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("neuromita-ft-{tag}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sample(timestamp: &str, character: &str, model: &str, response: &str) -> FineTuneSample {
    let mut sample = FineTuneSample::new(
        UtcTimestamp::parse(timestamp).unwrap(),
        character,
        Some(model.into()),
        vec![SampleMessage::text(MessageRole::User, "hello")],
        response,
    );
    sample.character_name = character.into();
    sample
}

fn append(
    store: &FineTuneStore,
    sample: &FineTuneSample,
    retention: Retention,
) -> Result<AppendOutcome, FineTuneError> {
    store.append_sample(sample, retention)
}

fn sample_files(store: &FineTuneStore) -> Vec<PathBuf> {
    let mut files = fs::read_dir(store.root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("samples_") && name.ends_with(".jsonl"))
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn read_samples(store: &FineTuneStore) -> Vec<FineTuneSample> {
    let mut samples = Vec::new();
    for path in sample_files(store) {
        for line in fs::read_to_string(path).unwrap().lines() {
            if let Ok(sample) = serde_json::from_str::<FineTuneSample>(line) {
                samples.push(sample);
            }
        }
    }
    samples.sort_by_key(|sample| sample.timestamp);
    samples
}

fn filter_range(from: &str, to: &str) -> SampleFilters {
    let (start, _) = UtcTimestamp::utc_date_bounds(from).unwrap();
    let (_, end) = UtcTimestamp::utc_date_bounds(to).unwrap();
    SampleFilters {
        range: TimeRange::new(Some(start), Some(end)).unwrap(),
        ..SampleFilters::default()
    }
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(8).unwrap(),
        discovery_buffer: NonZeroUsize::new(128).unwrap(),
        character_commands: NonZeroUsize::new(32).unwrap(),
        presenter_updates: NonZeroUsize::new(128).unwrap(),
        retained_finished: NonZeroUsize::new(50).unwrap(),
    }
}

struct GenerationCall {
    context: CharacterContext,
    reply: oneshot::Sender<GenerationFixture>,
}

#[derive(Debug)]
struct GenerationFixture {
    result: Result<StructuredResponse, GenerationError>,
    response: Option<ProviderReply>,
}

struct ControlledGeneration(mpsc::UnboundedSender<GenerationCall>);

impl GenerationProvider for ControlledGeneration {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            self.generate_observed(context, template, &mut NoopObserver)
                .await
        })
    }

    fn generate_observed<'a>(
        &'a self,
        context: GenerationInput,
        template: ProviderTemplate,
        observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            let model: Arc<str> = match &template {
                ProviderTemplate::Gemini(template) => template.model().into(),
                ProviderTemplate::OpenAi(template) => template.model().into(),
            };
            let user_text: Arc<str> = context.view.messages().last().map_or_else(
                || Arc::clone(&context.current_input),
                |message| Arc::from(message.content.as_str()),
            );
            observer.request_prepared(PreparedRequest {
                protocol: "google_gemini_default".into(),
                dialect: "gemini_generate_content".into(),
                provider: ProviderKind::Gemini,
                model,
                endpoint: "http://fixture.invalid/v1beta/models/fixture-model:generateContent"
                    .into(),
                temperature: Some(0.7),
                max_output_tokens: Some(128),
                prompt: vec![
                    PromptMessage {
                        role: PromptRole::System,
                        text: Arc::clone(&context.description),
                    },
                    PromptMessage {
                        role: PromptRole::User,
                        text: user_text,
                    },
                ]
                .into(),
                payload_json: r#"{"fixture":true}"#.into(),
                captured_at: std::time::SystemTime::now(),
            });
            let (reply, receive) = oneshot::channel();
            self.0
                .send(GenerationCall {
                    context: context.context,
                    reply,
                })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            let fixture = receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))?;
            if let Some(response) = fixture.response {
                observer.response_received(response);
            }
            if let Err(error) = &fixture.result {
                observer.failed(error);
            }
            fixture.result
        })
    }
}

struct NoopObserver;
impl GenerationObserver for NoopObserver {}

fn fixture_ok(text: &str, total_tokens: u64) -> GenerationFixture {
    let structured = StructuredResponse::spoken(text);
    let answer = serde_json::to_string(&structured).unwrap();
    GenerationFixture {
        result: Ok(structured),
        response: Some(ProviderReply {
            status: Some(200),
            answer_text: Some(answer.as_str().into()),
            raw_body: json!({
                "candidates": [{"content":{"parts":[{"text": answer}]},"finishReason":"STOP"}],
                "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": total_tokens - 3, "totalTokenCount": total_tokens}
            })
            .to_string()
            .into(),
            complete: true,
            finish_reason: Some("STOP".into()),
            usage: Some(ProviderUsage {
                input_tokens: Some(3),
                output_tokens: Some(total_tokens - 3),
                total_tokens: Some(total_tokens),
            }),
            elapsed: Duration::from_millis(1),
        }),
    }
}

fn fixture_http_error(status: u16) -> GenerationFixture {
    GenerationFixture {
        result: Err(GenerationError::Http(status)),
        response: Some(ProviderReply {
            status: Some(status),
            raw_body: "fixture error".into(),
            answer_text: None,
            complete: true,
            finish_reason: None,
            usage: None,
            elapsed: Duration::from_millis(1),
        }),
    }
}

struct VoiceCall {
    _reply: oneshot::Sender<Result<AudioArtifact, VoiceError>>,
}

struct ControlledVoice(mpsc::UnboundedSender<VoiceCall>);

impl VoiceProvider for ControlledVoice {
    fn synthesize(&self, _text: Arc<str>, _options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(VoiceCall { _reply: reply })
                .map_err(|_| VoiceError::Unavailable)?;
            receive.await.map_err(|_| VoiceError::Unavailable)?
        })
    }
}

fn generator(sender: mpsc::UnboundedSender<GenerationCall>) -> ResponseGenerator {
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledGeneration(sender));
    ResponseGenerator::new([provider]).unwrap()
}

fn preset() -> GenerationPreset {
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture-model",
            "fixture-secret",
            GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ))
}

async fn next<T>(receive: &mut mpsc::UnboundedReceiver<T>) -> T {
    timeout(Duration::from_secs(15), receive.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn done(
    handle: &neuromita::request::RequestHandle,
) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(15), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

async fn flush(app: &ApplicationLifecycle) {
    let status = app
        .finetune()
        .unwrap()
        .drain(Duration::from_secs(5), false)
        .await;
    assert_eq!(status.pending, 0, "{status:?}");
    assert_eq!(status.errors, 0, "{status:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_capture_is_delivered_independently_and_records_outcomes() {
    let _serial = APP_LOCK.lock().await;
    let root = TempDir::new("lifecycle");
    let data_dir = root.path().join("data");
    let settings = FineTuneSettings::new(data_dir, true, Retention::limited(50).unwrap()).unwrap();
    let (generated, mut generations) = mpsc::unbounded_channel();
    let (voiced, mut voices) = mpsc::unbounded_channel();
    let mut config = ApplicationConfig::minimal(
        preset(),
        generator(generated),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.finetune = Some(settings.clone());
    config.voice = Arc::new(ControlledVoice(voiced));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let presenter = app.presenter();
    let origin = app.at();

    let first = presenter
        .chat
        .reply(CharacterId::Crazy, "first request".into())
        .unwrap();
    let call = next(&mut generations).await;
    assert_eq!(
        call.context
            .view
            .messages()
            .last()
            .unwrap()
            .content
            .as_str(),
        "first request"
    );
    call.reply
        .send(fixture_ok("first provider text", 9))
        .unwrap();
    assert!(matches!(
        done(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let store = app.finetune().unwrap().store();
    flush(&app).await;
    let samples = read_samples(store);
    assert_eq!(samples.len(), 1);
    let captured = &samples[0];
    assert_eq!(captured.request_id, Some(first.id()));
    let captured_text = StructuredResponse::parse(captured.raw_response.as_deref().unwrap())
        .unwrap()
        .full_text();
    assert_eq!(captured_text.as_ref(), "first provider text");
    assert_eq!(captured.response, captured.raw_response.clone().unwrap());
    assert_eq!(captured.cleaned_response, None);
    assert_eq!(captured.text_stage, TextStage::ProviderRaw);
    assert_eq!(captured.usage.as_ref().unwrap()["total_tokens"], 9);
    assert_eq!(
        captured.usage_source.as_deref(),
        Some("provider_response_usage_metadata")
    );
    assert_eq!(captured.messages[0].role, MessageRole::System);
    assert!(captured.messages[0].content.flatten().text.contains("Мита"));
    assert_eq!(
        captured.messages.last().unwrap().content.flatten().text,
        "first request"
    );
    assert_eq!(first.snapshot().dataset.sample_id, Some(captured.id));
    assert_eq!(
        store.list(&SampleFilters::default(), 10).unwrap().rows[0].outcome,
        Some(SampleOutcome::Accepted)
    );

    let rating = store
        .set_rating(captured.id, neuromita::finetune::Rating::Positive)
        .unwrap();
    assert_eq!(rating.previous, None);
    let stats = store.stats().unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.positive, 1);
    assert_eq!(stats.accepted, 1);
    assert_eq!(read_samples(store)[0].response, captured.response);

    let failed = presenter
        .chat
        .reply(CharacterId::Crazy, "provider failure".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_http_error(500))
        .unwrap();
    assert!(matches!(
        done(&failed).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    flush(&app).await;
    assert_eq!(store.stats().unwrap().total, 1);

    let empty = presenter
        .chat
        .reply(CharacterId::Crazy, "empty provider text".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("   ", 9))
        .unwrap();
    assert!(matches!(
        done(&empty).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    flush(&app).await;
    assert_eq!(store.stats().unwrap().total, 2);

    let second = presenter
        .chat
        .reply(CharacterId::Crazy, "second request".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("second provider text", 9))
        .unwrap();
    assert!(matches!(
        done(&second).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let stale = presenter
        .chat
        .reply_at(
            CharacterId::Crazy,
            "stale context".into(),
            ContextPosition::At(origin),
        )
        .unwrap();
    assert!(matches!(
        done(&stale).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert!(generations.try_recv().is_err());
    flush(&app).await;
    assert_eq!(store.stats().unwrap().total, 3);

    let rejected = presenter
        .chat
        .reply(CharacterId::Crazy, "unresolvable memory effect".into())
        .unwrap();
    let mut invalid_effect = fixture_ok("provider response with invalid effect", 9);
    let structured = invalid_effect.result.as_mut().unwrap();
    structured.memory_delete = Some(vec![uuid::Uuid::new_v4().to_string()]);
    // Reflect the actual provider answer before acceptance rejects the effect.
    let raw = serde_json::to_string(structured).unwrap();
    invalid_effect.response.as_mut().unwrap().raw_body = gemini_body(&raw, 9).into();
    next(&mut generations)
        .await
        .reply
        .send(invalid_effect)
        .unwrap();
    assert!(matches!(
        done(&rejected).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    flush(&app).await;
    let all = store.list(&SampleFilters::default(), 20).unwrap();
    assert_eq!(all.matched, 4);
    let empty_sample = all
        .rows
        .iter()
        .find(|sample| sample.request_id == Some(empty.id()))
        .unwrap();
    assert_eq!(empty_sample.outcome, Some(SampleOutcome::AcceptanceFailed));
    let stale_sample = all
        .rows
        .iter()
        .find(|sample| sample.request_id == Some(rejected.id()))
        .unwrap();
    assert_eq!(stale_sample.outcome, Some(SampleOutcome::AcceptanceFailed));
    let accepted_only = store
        .scan(&SampleFilters {
            outcome: OutcomeFilter::Accepted,
            ..SampleFilters::default()
        })
        .unwrap();
    let not_accepted = store
        .scan(&SampleFilters {
            outcome: OutcomeFilter::NotAccepted,
            ..SampleFilters::default()
        })
        .unwrap();
    assert_eq!(accepted_only.matched, 2);
    assert_eq!(not_accepted.matched, 2);

    presenter.settings.voice.in_launcher.set(true).unwrap();
    let cancelled = presenter
        .chat
        .reply(CharacterId::Crazy, "cancel after provider".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("cancel provider text", 9))
        .unwrap();
    let speech = next(&mut voices).await;
    assert_eq!(cancelled.cancel(), CancelResult::Requested);
    drop(speech);
    assert!(matches!(
        done(&cancelled).await.state,
        RequestState::Cancelled
    ));
    flush(&app).await;
    let cancelled_sample = store
        .list(&SampleFilters::default(), 20)
        .unwrap()
        .rows
        .into_iter()
        .find(|sample| sample.request_id == Some(cancelled.id()))
        .unwrap();
    assert_eq!(
        cancelled_sample.outcome,
        Some(SampleOutcome::CancelledBeforeAccept)
    );
    presenter.settings.voice.in_launcher.set(false).unwrap();

    let partial = presenter
        .chat
        .reply(CharacterId::Crazy, "partial transport body".into())
        .unwrap();
    let mut partial_fixture = fixture_ok("partial provider text", 9);
    partial_fixture.response.as_mut().unwrap().complete = false;
    next(&mut generations)
        .await
        .reply
        .send(partial_fixture)
        .unwrap();
    let partial_done = done(&partial).await;
    assert!(matches!(
        partial_done.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert_eq!(partial_done.dataset.sample_id, None);
    flush(&app).await;
    assert_eq!(store.stats().unwrap().total, 5);

    let saved_data = root.path().join("saved-data");
    fs::rename(&settings.data_dir, &saved_data).unwrap();
    fs::write(&settings.data_dir, b"blocks dataset writes").unwrap();
    let storage_failed = presenter
        .chat
        .reply(CharacterId::Crazy, "dataset storage unavailable".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("accepted despite dataset error", 9))
        .unwrap();
    let storage_done = done(&storage_failed).await;
    assert!(matches!(
        storage_done.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(storage_done.dataset.sample_id.is_some());
    let delivery = app
        .finetune()
        .unwrap()
        .drain(Duration::from_secs(2), false)
        .await;
    assert_eq!(delivery.pending, 0);
    assert!(delivery.errors > 0);
    assert!(delivery.last_error.is_some());
    fs::remove_file(&settings.data_dir).unwrap();
    fs::rename(&saved_data, &settings.data_dir).unwrap();

    let total_before_disabled = store.stats().unwrap().total;
    assert_eq!(total_before_disabled, 5);
    app.shutdown().await.unwrap();
    let reopened = FineTuneStore::open(settings.data_dir.clone()).unwrap();
    assert_eq!(reopened.stats().unwrap().total, 5);
    assert_eq!(reopened.stats().unwrap().positive, 1);

    let disabled_root = TempDir::new("disabled");
    let disabled = FineTuneSettings::new(
        disabled_root.path().join("data"),
        false,
        Retention::Unlimited,
    )
    .unwrap();
    let (disabled_generated, mut disabled_generations) = mpsc::unbounded_channel();
    let mut disabled_config = ApplicationConfig::minimal(
        preset(),
        generator(disabled_generated),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    disabled_config.finetune = Some(disabled.clone());
    let disabled_app = ApplicationLifecycle::start(disabled_config).await.unwrap();
    let disabled_request = disabled_app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "disabled collection".into())
        .unwrap();
    next(&mut disabled_generations)
        .await
        .reply
        .send(fixture_ok("not captured", 9))
        .unwrap();
    assert!(matches!(
        done(&disabled_request).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let disabled_collector = FineTuneCollector::from_settings(&disabled).unwrap();
    assert_eq!(disabled_collector.store().stats().unwrap().total, 0);
    disabled_app.shutdown().await.unwrap();
}

struct WireCall {
    path: String,
    body: Value,
    respond: oneshot::Sender<String>,
}

async fn read_wire_request(stream: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        assert!(bytes.len() < 64 * 1024);
    };
    let header = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let path = header.lines().next().unwrap().to_owned();
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < boundary + length {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    (
        path,
        serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap(),
    )
}

fn gemini_body(answer: &str, total_tokens: u64) -> String {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": answer}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 4,
            "candidatesTokenCount": total_tokens - 4,
            "totalTokenCount": total_tokens
        }
    })
    .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_wire_request_and_prevalidation_response_are_captured() {
    let _serial = APP_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1beta/", listener.local_addr().unwrap());
    let (send, mut calls) = mpsc::unbounded_channel::<WireCall>();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, body) = read_wire_request(&mut stream).await;
            let (respond, response) = oneshot::channel();
            send.send(WireCall {
                path,
                body,
                respond,
            })
            .unwrap();
            let raw = response.await.unwrap();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                raw.len(),
                raw
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });

    let root = TempDir::new("wire");
    let settings =
        FineTuneSettings::new(root.path().join("data"), true, Retention::Unlimited).unwrap();
    let template = GeminiTemplate::new(
        &endpoint,
        "fixture-model",
        "fixture-secret",
        GenerationSettings::new(0.4, 256, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(template));
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.finetune = Some(settings);
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let store = app.finetune().unwrap().store().clone();

    let accepted = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "actual wire request".into())
        .unwrap();
    let call = next(&mut calls).await;
    assert!(call.path.contains(":generateContent?key=fixture-secret "));
    assert_eq!(
        call.body["contents"].as_array().unwrap().last().unwrap()["parts"][0]["text"],
        "actual wire request"
    );
    assert_eq!(call.body["generationConfig"]["temperature"], 0.4);
    let accepted_raw = gemini_body(
        &json!({"segments":[{"text":"wire answer"}]}).to_string(),
        12,
    );
    call.respond.send(accepted_raw.clone()).unwrap();
    let accepted_done = done(&accepted).await;
    assert!(matches!(
        accepted_done.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let rejected = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "invalid structured answer".into())
        .unwrap();
    let call = next(&mut calls).await;
    let rejected_raw = gemini_body("not-json", 8);
    call.respond.send(rejected_raw.clone()).unwrap();
    let rejected_done = done(&rejected).await;
    assert!(matches!(
        rejected_done.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));

    flush(&app).await;
    let rows = store.list(&SampleFilters::default(), 10).unwrap().rows;
    assert_eq!(rows.len(), 2);
    let accepted_sample = rows
        .iter()
        .find(|row| row.request_id == Some(accepted.id()))
        .unwrap();
    assert_eq!(accepted_sample.outcome, Some(SampleOutcome::Accepted));
    assert_eq!(accepted_done.dataset.sample_id, Some(accepted_sample.id));
    let rejected_sample = rows
        .iter()
        .find(|row| row.request_id == Some(rejected.id()))
        .unwrap();
    assert_eq!(
        rejected_sample.outcome,
        Some(SampleOutcome::AcceptanceFailed)
    );

    let samples = read_samples(&store);
    let captured = samples
        .iter()
        .find(|sample| sample.request_id == Some(accepted.id()))
        .unwrap();
    assert_eq!(
        captured.provider_response.as_deref(),
        Some(accepted_raw.as_str())
    );
    let payload: Value =
        serde_json::from_str(captured.request_payload.as_deref().unwrap()).unwrap();
    assert_eq!(
        payload["contents"].as_array().unwrap().last().unwrap()["parts"][0]["text"],
        "actual wire request"
    );
    assert!((captured.temperature.unwrap() - 0.4).abs() < 1e-6);
    assert_eq!(captured.max_tokens, Some(256));
    assert_eq!(captured.usage.as_ref().unwrap()["total_tokens"], 12);
    assert_eq!(
        captured.messages.last().unwrap().content.flatten().text,
        "actual wire request"
    );
    assert!(
        !captured
            .request_payload
            .as_deref()
            .unwrap()
            .contains("fixture-secret")
    );
    let malformed = samples
        .iter()
        .find(|sample| sample.request_id == Some(rejected.id()))
        .unwrap();
    assert_eq!(malformed.raw_response.as_deref(), Some("not-json"));
    assert_eq!(
        malformed.provider_response.as_deref(),
        Some(rejected_raw.as_str())
    );

    let diagnostics =
        neuromita::api::ChatApi::diagnostics_for(neuromita::request::DiagnosticScope::owner(
            CharacterId::Crazy,
            neuromita::request::RequestSource::Presenter,
        ))
        .unwrap();
    let trace = diagnostics.request(accepted.id()).unwrap().trace;
    let view = trace.view();
    assert_eq!(
        view.provider_request.as_ref().unwrap().body.text,
        captured.request_payload.as_ref().unwrap().as_str()
    );
    assert_eq!(
        view.provider_response.as_ref().unwrap().raw_body.text,
        accepted_raw
    );
    assert_eq!(view.usage.actual.as_ref().unwrap().total_tokens, Some(12));
    assert!(!trace.to_json_pretty().unwrap().contains("fixture-secret"));
    let malformed_trace = diagnostics.request(rejected.id()).unwrap().trace;
    assert!(malformed_trace.view().provider_error.is_some());
    assert_eq!(
        malformed_trace
            .view()
            .provider_response
            .unwrap()
            .raw_body
            .text,
        rejected_raw
    );

    app.shutdown().await.unwrap();
    server.await.unwrap();
}

#[test]
fn retention_keeps_latest_and_prunes_correlated_outcomes() {
    let root = TempDir::new("retention");
    let store = FineTuneStore::open(root.path().join("moved")).unwrap();
    let first = sample("2026-01-01T00:00:00Z", "mita", "model-a", "first");
    let second = sample(
        "2026-01-02T00:00:00.999999999Z",
        "kind-mita",
        "model-b",
        "second",
    );
    let third = sample("2026-02-01T00:00:00Z", "mita", "model-a", "third");
    for sample in [&first, &second, &third] {
        append(&store, sample, Retention::Unlimited).unwrap();
    }
    for sample in [&first, &second, &third] {
        store
            .append_outcome(&OutcomeRecord {
                sample_id: sample.id,
                request_id: None,
                accepted_event_id: None,
                outcome: SampleOutcome::Accepted,
                recorded_at: sample.timestamp,
            })
            .unwrap();
    }
    let outcome = store
        .enforce_retention(Retention::limited(2).unwrap())
        .unwrap();
    assert_eq!(outcome.removed_samples, 1);
    assert_eq!(outcome.remaining_samples, 2);
    assert_eq!(outcome.removed_outcomes, 1);
    let ids = store
        .list(&SampleFilters::default(), 10)
        .unwrap()
        .rows
        .into_iter()
        .map(|sample| sample.id)
        .collect::<Vec<_>>();
    assert!(!ids.contains(&first.id));
    assert!(ids.contains(&second.id));
    assert!(ids.contains(&third.id));
    let stats = store.stats().unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.accepted, 2);
}

#[test]
fn retention_cap_keeps_latest_fifty_across_month_files() {
    let root = TempDir::new("cap50");
    let store = FineTuneStore::open(root.path().join("data")).unwrap();
    let mut ids = Vec::new();
    for index in 0..51 {
        let month = if index < 25 { "2026-01" } else { "2026-02" };
        let day = index % 25 + 1;
        let sample = sample(
            &format!("{month}-{day:02}T00:00:{:02}Z", index % 60),
            "mita",
            "model",
            "answer",
        );
        ids.push(sample.id);
        append(&store, &sample, Retention::limited(50).unwrap()).unwrap();
    }
    let retained = store
        .list(&SampleFilters::default(), 100)
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 50);
    assert!(!retained.contains(&ids[0]));
    assert!(retained.contains(&ids[50]));
}

#[test]
fn oversized_append_is_rejected_without_changing_existing_bytes() {
    let root = TempDir::new("oversized");
    let store = FineTuneStore::open(root.path().join("data")).unwrap();
    let first = sample("2026-01-01T00:00:00Z", "mita", "model", "kept");
    append(&store, &first, Retention::Unlimited).unwrap();
    let path = store
        .root()
        .join(format!("samples_{}.jsonl", first.timestamp.month_key()));
    let before = fs::read(&path).unwrap();
    let mut oversized = sample("2026-01-02T00:00:00Z", "mita", "model", "large");
    oversized.response = "x".repeat(33 * 1024 * 1024);
    assert!(matches!(
        append(&store, &oversized, Retention::Unlimited),
        Err(FineTuneError::Io { .. })
    ));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn rating_and_retention_preserve_unreadable_and_incomplete_lines() {
    let root = TempDir::new("corrupt");
    let store = FineTuneStore::open(root.path().join("data")).unwrap();
    let first = sample("2026-01-01T00:00:00Z", "mita", "model", "first");
    append(&store, &first, Retention::Unlimited).unwrap();
    let path = store
        .root()
        .join(format!("samples_{}.jsonl", first.timestamp.month_key()));
    let broken = b"{\"incomplete\":\xff";
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    use std::io::Write;
    file.write_all(broken).unwrap();
    drop(file);
    store
        .set_rating(first.id, neuromita::finetune::Rating::Positive)
        .unwrap();
    let after_rating = fs::read(&path).unwrap();
    assert!(after_rating.ends_with(broken));
    assert!(!after_rating.is_empty());
    let before_append = after_rating.clone();
    let stats = store.stats().unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.unreadable_lines, 1);

    let second = sample("2026-01-02T00:00:00Z", "mita", "model", "second");
    assert!(matches!(
        append(&store, &second, Retention::Unlimited),
        Err(FineTuneError::IncompleteTail(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), before_append);

    let third = sample("2026-02-01T00:00:00Z", "mita", "model", "third");
    append(&store, &third, Retention::Unlimited).unwrap();
    let outcome = store
        .enforce_retention(Retention::limited(1).unwrap())
        .unwrap();
    assert_eq!(outcome.removed_samples, 1);
    assert_eq!(outcome.preserved_unreadable_lines, 1);
    let january = store
        .root()
        .join(format!("samples_{}.jsonl", first.timestamp.month_key()));
    assert_eq!(fs::read(january).unwrap(), broken);
    let stats = store.stats().unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.unreadable_lines, 1);
    assert!(matches!(
        store.set_rating(SampleId::new(), neuromita::finetune::Rating::Negative),
        Err(FineTuneError::SampleNotFound(_))
    ));
    let recovered = root.path().join("recovered.jsonl");
    let report = store
        .export(
            &SampleFilters::default(),
            ExportFormat::RawJsonl,
            &recovered,
        )
        .unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(report.unreadable_lines, 1);
    assert!(fs::read_to_string(recovered).unwrap().contains("third"));
}

#[test]
fn utc_period_filters_are_inclusive_and_offsets_are_normalized() {
    let root = TempDir::new("time");
    let store = FineTuneStore::open(root.path().join("moved")).unwrap();
    let start = sample("2026-01-01T00:00:00Z", "mita", "model-a", "start");
    let end = sample(
        "2026-01-01T23:59:59.999999999Z",
        "kind-mita",
        "model-b",
        "end",
    );
    let next_day = sample("2026-01-02T00:00:00Z", "mita", "model-b", "next");
    let offset = sample("2026-01-01T02:30:00+03:00", "mita", "model-a", "offset");
    for sample in [&start, &end, &next_day, &offset] {
        append(&store, sample, Retention::Unlimited).unwrap();
    }
    let summary = store
        .scan(&filter_range("2026-01-01", "2026-01-01"))
        .unwrap();
    assert_eq!(summary.matched, 2);
    let from_only = SampleFilters {
        range: TimeRange::new(
            Some(UtcTimestamp::utc_date_bounds("2026-01-01").unwrap().0),
            None,
        )
        .unwrap(),
        ..SampleFilters::default()
    };
    assert_eq!(store.scan(&from_only).unwrap().matched, 3);
    let offset_utc = UtcTimestamp::parse("2025-12-31T23:30:00Z").unwrap();
    assert_eq!(
        UtcTimestamp::parse("2026-01-01T02:30:00+03:00").unwrap(),
        offset_utc
    );
    assert!(UtcTimestamp::parse("2024-02-29T00:00:00Z").is_ok());
    assert!(UtcTimestamp::parse("2023-02-29T00:00:00Z").is_err());
    assert!(UtcTimestamp::parse("2026-01-01T00:00:00+24:00").is_err());
    assert!(UtcTimestamp::parse("0000-01-01T00:00:00Z").is_err());
    assert!(UtcTimestamp::parse("9999-12-31T23:59:59Z").is_ok());
    assert!(UtcTimestamp::parse("10000-01-01T00:00:00Z").is_err());
    assert!(UtcTimestamp::parse("2026-01-01T00:00:00Z\u{fffd}").is_err());
    assert!(UtcTimestamp::from_unix(i64::MIN, 0).is_err());
    assert!(UtcTimestamp::from_unix(i64::MAX, 0).is_err());
    assert!(TimeRange::new(Some(end.timestamp), Some(start.timestamp)).is_err());

    let character = store
        .scan(&SampleFilters {
            characters: vec!["kind-mita".into()],
            ..SampleFilters::default()
        })
        .unwrap();
    assert_eq!(character.matched, 1);
    let model = store
        .scan(&SampleFilters {
            models: vec!["model-b".into()],
            ..SampleFilters::default()
        })
        .unwrap();
    assert_eq!(model.matched, 2);
}

#[test]
fn export_keeps_raw_multimodal_data_and_reports_text_loss() {
    let root = TempDir::new("export");
    let store = FineTuneStore::open(root.path().join("data")).unwrap();
    let mut sample = sample("2026-01-01T00:00:00Z", "mita", "model", "answer");
    sample.messages = vec![
        SampleMessage::text(MessageRole::System, "system"),
        SampleMessage {
            role: MessageRole::User,
            content: MessageContent::Parts(vec![
                ContentPart::Text("look".into()),
                ContentPart::Other(json!({"type": "image_url", "image_url": {"url": "x"}})),
                ContentPart::Text("here".into()),
            ]),
        },
        SampleMessage::text(MessageRole::Assistant, "prior"),
        SampleMessage::text(MessageRole::Tool, "tool payload"),
    ];
    append(&store, &sample, Retention::Unlimited).unwrap();
    store
        .append_outcome(&OutcomeRecord {
            sample_id: sample.id,
            request_id: None,
            accepted_event_id: None,
            outcome: SampleOutcome::Accepted,
            recorded_at: sample.timestamp,
        })
        .unwrap();
    let preview = store.scan(&SampleFilters::default()).unwrap();
    assert_eq!(preview.matched, 1);
    assert_eq!(preview.multimodal_samples, 1);
    assert_eq!(preview.dropped_non_text_parts, 1);

    let sharegpt = root.path().join("sharegpt.jsonl");
    let report = store
        .export(&SampleFilters::default(), ExportFormat::ShareGpt, &sharegpt)
        .unwrap();
    assert_eq!(report.written, 1);
    let value: Value = serde_json::from_str(fs::read_to_string(&sharegpt).unwrap().trim()).unwrap();
    let conversations = value["conversations"].as_array().unwrap();
    assert_eq!(conversations[1]["from"], "human");
    assert_eq!(conversations[1]["value"], "look here");
    assert_eq!(conversations.last().unwrap()["from"], "gpt");
    assert_eq!(conversations.last().unwrap()["value"], "answer");
    assert_eq!(conversations.len(), 4);

    let chatml = root.path().join("chatml.jsonl");
    store
        .export(&SampleFilters::default(), ExportFormat::ChatMl, &chatml)
        .unwrap();
    let value: Value = serde_json::from_str(fs::read_to_string(&chatml).unwrap().trim()).unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "look here");
    assert_eq!(messages.last().unwrap()["role"], "assistant");
    assert_eq!(messages.len(), 4);

    let raw = root.path().join("raw.jsonl");
    store
        .export(&SampleFilters::default(), ExportFormat::RawJsonl, &raw)
        .unwrap();
    let value: Value = serde_json::from_str(fs::read_to_string(&raw).unwrap().trim()).unwrap();
    assert_eq!(value["messages"][1]["content"].as_array().unwrap().len(), 3);
    assert_eq!(value["accepted_outcome"], "accepted");
    assert_eq!(value["raw_response"], "answer");

    let existing = root.path().join("existing.jsonl");
    fs::write(&existing, b"keep").unwrap();
    assert!(matches!(
        store.export(&SampleFilters::default(), ExportFormat::RawJsonl, &existing),
        Err(FineTuneError::DestinationExists(_))
    ));
    assert_eq!(fs::read(existing).unwrap(), b"keep");
    assert!(matches!(
        store.export(
            &SampleFilters::default(),
            ExportFormat::RawJsonl,
            &store.root().join("samples_209901.jsonl")
        ),
        Err(FineTuneError::ManagedOutput(_))
    ));
    assert!(matches!(
        store.export(
            &SampleFilters::default(),
            ExportFormat::RawJsonl,
            &store.root().join("export.jsonl")
        ),
        Err(FineTuneError::ManagedOutput(_))
    ));
    assert!(matches!(
        store.export(
            &SampleFilters::default(),
            ExportFormat::RawJsonl,
            &root.path().join("missing").join("file.jsonl")
        ),
        Err(FineTuneError::NotDirectory(_))
    ));
}

#[test]
fn settings_and_cli_support_without_a_provider() {
    let root = TempDir::new("settings");
    let config = root.path().join("finetune.json");
    let data = root.path().join("data");
    cli::run(vec![
        "init".into(),
        "--config".into(),
        config.display().to_string(),
        "--dir".into(),
        data.display().to_string(),
        "--enabled".into(),
        "on".into(),
        "--max".into(),
        "10".into(),
    ])
    .unwrap();
    assert!(FineTuneSettings::load(&config).unwrap().enabled);
    assert!(matches!(
        cli::run(vec![
            "init".into(),
            "--config".into(),
            config.display().to_string(),
            "--dir".into(),
            data.display().to_string(),
            "--enabled".into(),
            "on".into(),
            "--max".into(),
            "10".into(),
        ]),
        Err(CliError::Usage(_))
    ));
    cli::run(vec![
        "config".into(),
        "--config".into(),
        config.display().to_string(),
        "enable".into(),
        "off".into(),
    ])
    .unwrap();
    assert!(!FineTuneSettings::load(&config).unwrap().enabled);
    cli::run(vec![
        "config".into(),
        "--config".into(),
        config.display().to_string(),
        "max".into(),
        "unlimited".into(),
    ])
    .unwrap();
    assert_eq!(
        FineTuneSettings::load(&config).unwrap().retention,
        Retention::Unlimited
    );
    let moved = root.path().join("moved");
    cli::run(vec![
        "config".into(),
        "--config".into(),
        config.display().to_string(),
        "dir".into(),
        moved.display().to_string(),
    ])
    .unwrap();
    assert_eq!(FineTuneSettings::load(&config).unwrap().data_dir, moved);

    let corrupt = root.path().join("corrupt.json");
    fs::write(&corrupt, b"{broken").unwrap();
    let before = fs::read(&corrupt).unwrap();
    assert!(FineTuneSettings::load(&corrupt).is_err());
    assert!(
        cli::run(vec![
            "config".into(),
            "--config".into(),
            corrupt.display().to_string(),
            "enable".into(),
            "on".into(),
        ])
        .is_err()
    );
    assert_eq!(fs::read(&corrupt).unwrap(), before);

    let store = FineTuneStore::open(moved.clone()).unwrap();
    append(
        &store,
        &sample("2026-01-01T00:00:00Z", "mita", "model", "data"),
        Retention::Unlimited,
    )
    .unwrap();
    assert!(matches!(
        cli::run(vec![
            "clear".into(),
            "--config".into(),
            config.display().to_string(),
        ]),
        Err(CliError::FineTune(FineTuneError::ConfirmationRequired))
    ));
    assert!(!sample_files(&store).is_empty());
    cli::run(vec![
        "clear".into(),
        "--config".into(),
        config.display().to_string(),
        "--yes".into(),
    ])
    .unwrap();
    assert!(sample_files(&store).is_empty());
    assert!(config.exists());

    let restarted = FineTuneSettings::load(&config).unwrap();
    assert!(!restarted.enabled);
    assert_eq!(restarted.retention, Retention::Unlimited);
    assert_eq!(restarted.data_dir, moved);

    let app_data = root.path().join("app-data");
    fs::create_dir_all(&app_data).unwrap();
    let app_dataset = FineTuneSettings::new(
        root.path().join("app-dataset"),
        true,
        Retention::limited(7).unwrap(),
    )
    .unwrap();
    FineTuneSettings::create_new(&app_data.join("finetune.json"), &app_dataset).unwrap();
    let (unused_generation, _unused_receiver) = mpsc::unbounded_channel();
    let persistent = ApplicationConfig::persistent(
        preset(),
        generator(unused_generation),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
        &app_data,
    )
    .unwrap();
    assert_eq!(persistent.finetune.as_ref(), Some(&app_dataset));

    let output = Command::new(env!("CARGO_BIN_EXE_neuromita"))
        .args(["dataset", "help"])
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dataset CLI required a provider or failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn producer_and_external_cli_contention_does_not_lose_writes() {
    let root = TempDir::new("contention");
    let config = root.path().join("finetune.json");
    let data = root.path().join("data");
    cli::run(vec![
        "init".into(),
        "--config".into(),
        config.display().to_string(),
        "--dir".into(),
        data.display().to_string(),
        "--enabled".into(),
        "on".into(),
        "--unlimited".into(),
    ])
    .unwrap();
    let initial_store = FineTuneStore::open(data.clone()).unwrap();
    let mut initial = Vec::new();
    for index in 0..10 {
        let sample = sample(
            &format!("2026-01-01T00:00:0{index}Z"),
            "mita",
            "model",
            "initial",
        );
        append(&initial_store, &sample, Retention::Unlimited).unwrap();
        initial.push(sample.id);
    }

    let producer_data = data.clone();
    let producer = std::thread::spawn(move || {
        let store = FineTuneStore::open(producer_data).unwrap();
        for index in 0..20 {
            let sample = sample(
                &format!("2026-02-01T00:00:{:02}Z", index % 60),
                "mita",
                "model",
                "producer",
            );
            store.append_sample(&sample, Retention::Unlimited).unwrap();
        }
    });

    let binary = env!("CARGO_BIN_EXE_neuromita");
    let rater_config = config.clone();
    let rater = std::thread::spawn(move || {
        for id in initial {
            let output = Command::new(binary)
                .args([
                    "dataset",
                    "rate",
                    "--config",
                    rater_config.to_str().unwrap(),
                    "--sample",
                    &id.to_string(),
                    "--value",
                    "1",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "unexpected CLI failure: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    });
    producer.join().unwrap();
    rater.join().unwrap();
    let stats = initial_store.stats().unwrap();
    assert_eq!(stats.total, 30);
    assert_eq!(stats.positive, 10);
    assert_eq!(stats.unreadable_lines, 0);
    assert_eq!(
        stats.not_accepted + stats.accepted + stats.outcome_unknown,
        30
    );
    assert!(data.join(".neuromita-finetune.lock").exists());
}

#[test]
fn cli_export_preview_does_not_create_a_file() {
    let root = TempDir::new("cli-export");
    let config = root.path().join("finetune.json");
    let data = root.path().join("data");
    cli::run(vec![
        "init".into(),
        "--config".into(),
        config.display().to_string(),
        "--dir".into(),
        data.display().to_string(),
        "--enabled".into(),
        "on".into(),
        "--unlimited".into(),
    ])
    .unwrap();
    let store = FineTuneStore::open(data).unwrap();
    append(
        &store,
        &sample("2026-01-01T00:00:00Z", "mita", "model", "preview"),
        Retention::Unlimited,
    )
    .unwrap();
    cli::run(vec![
        "export".into(),
        "--config".into(),
        config.display().to_string(),
        "--format".into(),
        "sharegpt".into(),
        "--preview".into(),
    ])
    .unwrap();
    assert!(!root.path().join("preview.jsonl").exists());
    assert_eq!(store.stats().unwrap().total, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_storage_does_not_block_completion_cancel_or_shutdown() {
    let _serial = APP_LOCK.lock().await;
    let root = TempDir::new("stalled");
    let settings =
        FineTuneSettings::new(root.path().join("data"), true, Retention::Unlimited).unwrap();
    let (generated, mut generations) = mpsc::unbounded_channel();
    let (voiced, mut voices) = mpsc::unbounded_channel();
    let mut config = ApplicationConfig::minimal(
        preset(),
        generator(generated),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.finetune = Some(settings.clone());
    config.voice = Arc::new(ControlledVoice(voiced));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let collector = app.finetune().unwrap().clone();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(settings.data_dir.join(".neuromita-finetune.lock"))
        .unwrap();
    lock.lock().unwrap();
    let request = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "storage stalled".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("answer", 9))
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_millis(150), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(collector.delivery_status().pending > 0);

    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(true)
        .unwrap();
    let cancelled = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "cancel while writer stalled".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("cancelled", 9))
        .unwrap();
    let speech = next(&mut voices).await;
    assert_eq!(cancelled.cancel(), CancelResult::Requested);
    assert!(matches!(
        timeout(Duration::from_millis(150), cancelled.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Cancelled
    ));
    drop(speech);

    let shutdown = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "shutdown while writer stalled".into())
        .unwrap();
    next(&mut generations)
        .await
        .reply
        .send(fixture_ok("shutdown", 9))
        .unwrap();
    let speech = next(&mut voices).await;
    timeout(Duration::from_secs(2), app.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(shutdown.snapshot().state, RequestState::Cancelled));
    assert_eq!(
        shutdown.snapshot().cancel_reason,
        Some(neuromita::request::CancelReason::Shutdown)
    );
    drop(speech);
    let status = collector.drain(Duration::from_secs(1), true).await;
    assert!(status.closed);
    assert_eq!(status.pending, 0);
    assert!(status.errors + status.dropped > 0);
    drop(lock);
}
