use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, SystemTime},
};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationObserver, GenerationPreset, GenerationProvider, PreparedRequest,
        PromptMessage, PromptRole, ProviderFuture, ProviderKind, ProviderReply, ProviderTemplate,
        ProviderUsage, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{
        CancelResult, DiagnosticLookupError, DiagnosticScope, DiagnosticSection,
        DiagnosticSelection, FailureStage, RequestOutcome, RequestSource, RequestState,
    },
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

const SECRET: &str = "fixture-secret-key";
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
enum Scripted {
    Success {
        raw: String,
        usage: ProviderUsage,
    },
    Failure {
        error: GenerationError,
        reply: Option<ProviderReply>,
    },
}

struct ObservationCall {
    request: PreparedRequest,
    reply: oneshot::Sender<Scripted>,
}

struct ScriptedProvider(mpsc::UnboundedSender<ObservationCall>);

impl GenerationProvider for ScriptedProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(
        &self,
        _input: neuromita::generation::GenerationInput,
        _template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("unobserved call".into())) })
    }

    fn generate_observed<'a>(
        &'a self,
        _input: neuromita::generation::GenerationInput,
        _template: ProviderTemplate,
        observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            let prompt: Arc<[PromptMessage]> = vec![
                PromptMessage {
                    role: PromptRole::System,
                    text: "fixture system prompt".into(),
                },
                PromptMessage {
                    role: PromptRole::User,
                    text: "hello".into(),
                },
            ]
            .into();
            let request = PreparedRequest {
            protocol: "google_gemini_default".into(), dialect: "gemini_generate_content".into(),
                provider: ProviderKind::Gemini,
                model: "fixture-model".into(),
                endpoint: "https://example.test/v1beta".into(),
                temperature: Some(0.7),
                max_output_tokens: Some(128),
                prompt,
                payload_json: "{\"systemInstruction\":\"fixture system prompt\",\"url\":\"https://user:pass@example.test/v1?key=encoded-url-key\"}".into(),
                captured_at: SystemTime::now(),
            };
            observer.request_prepared(request.clone());
            let (reply, receive) = oneshot::channel();
            self.0
                .send(ObservationCall { request, reply })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            match receive
                .await
                .map_err(|_| GenerationError::Transport("fixture reply dropped".into()))?
            {
                Scripted::Success { raw, usage } => {
                    observer.response_received(ProviderReply {
                        status: Some(200),
                        raw_body: raw.as_str().into(),
                        answer_text: Some(raw.as_str().into()),
                        complete: true,
                        finish_reason: Some("STOP".into()),
                        usage: Some(usage),
                        elapsed: Duration::from_millis(3),
                    });
                    StructuredResponse::parse(&raw)
                }
                Scripted::Failure { error, reply } => {
                    if let Some(reply) = reply {
                        observer.response_received(reply);
                    }
                    observer.failed(&error);
                    Err(error)
                }
            }
        })
    }
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(8).unwrap(),
        discovery_buffer: NonZeroUsize::new(64).unwrap(),
        character_commands: NonZeroUsize::new(32).unwrap(),
        presenter_updates: NonZeroUsize::new(64).unwrap(),
        retained_finished: NonZeroUsize::new(50).unwrap(),
    }
}

async fn start() -> (
    ApplicationLifecycle,
    mpsc::UnboundedReceiver<ObservationCall>,
) {
    let (calls, receiver) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(ScriptedProvider(calls));
    let generator = ResponseGenerator::new([provider]).unwrap();
    let template = GeminiTemplate::google(
        "fixture-model",
        SECRET,
        GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        generator,
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    (app, receiver)
}

async fn next(receiver: &mut mpsc::UnboundedReceiver<ObservationCall>) -> ObservationCall {
    timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn terminal(
    handle: &neuromita::request::RequestHandle,
) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

fn scope() -> DiagnosticScope {
    DiagnosticScope::owner(CharacterId::Crazy, RequestSource::Presenter)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn success_captures_exact_artifacts_usage_and_redacted_exports() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "hello".into())
        .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.request.model.as_ref(), "fixture-model");
    assert_eq!(call.request.temperature, Some(0.7));
    call.reply
        .send(Scripted::Success {
            raw: json!({"segments":[{"text":"answer"}]}).to_string(),
            usage: ProviderUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                total_tokens: Some(18),
            },
        })
        .unwrap();
    assert!(matches!(
        terminal(&handle).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let access = ChatApi::diagnostics_for(scope()).unwrap();
    let record = access.request(handle.id()).unwrap();
    assert_eq!(record.selection, DiagnosticSelection::Exact);
    let view = record.trace.view();
    assert_eq!(view.input.text, "hello");
    assert_eq!(view.prompt.as_ref().unwrap().messages.len(), 1);
    assert_eq!(
        view.prompt
            .as_ref()
            .unwrap()
            .system_instruction
            .as_ref()
            .unwrap()
            .text,
        "fixture system prompt"
    );
    assert!(
        view.provider_request
            .as_ref()
            .unwrap()
            .body
            .text
            .contains("fixture system prompt")
    );
    assert!(view.provider_response.as_ref().unwrap().complete);
    assert_eq!(view.usage.actual.as_ref().unwrap().total_tokens, Some(18));
    assert!(view.usage.estimated.input_tokens > 0);
    assert!(view.usage.estimated.output_tokens > 0);
    assert!(
        !view
            .missing
            .iter()
            .any(|entry| entry.starts_with("actual_usage"))
    );

    let all = record.export(DiagnosticSection::All).unwrap();
    assert!(!all.contains("encoded-url-key"));
    assert!(!all.contains("user:pass"));
    assert!(!all.contains(SECRET));
    assert!(all.contains("[REDACTED]"));
    let usage = record.copy_text(DiagnosticSection::Usage).unwrap();
    assert!(usage.contains("estimated"));
    assert!(usage.contains("actual"));

    assert!(matches!(
        access.latest().unwrap().selection,
        DiagnosticSelection::Latest
    ));
    assert!(access.errors(8).unwrap().is_empty());
    assert!(matches!(
        access.request(neuromita::request::RequestId(uuid::Uuid::new_v4())),
        Err(DiagnosticLookupError::NotFound(_))
    ));
    let foreign = ChatApi::diagnostics(CharacterId::Kind).unwrap();
    assert!(matches!(
        foreign.request(handle.id()),
        Err(DiagnosticLookupError::OwnershipDenied(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_response_keeps_raw_body_and_error_history() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "malformed".into())
        .unwrap();
    let call = next(&mut calls).await;
    call.reply
        .send(Scripted::Failure {
            error: GenerationError::Response("invalid structured response v3".into()),
            reply: Some(ProviderReply {
                status: Some(200),
                raw_body: "{\"segments\":[]}".into(),
                answer_text: Some("{\"segments\":[]}".into()),
                complete: true,
                finish_reason: Some("STOP".into()),
                usage: None,
                elapsed: Duration::from_millis(4),
            }),
        })
        .unwrap();
    assert!(matches!(
        terminal(&handle).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    let access = ChatApi::diagnostics_for(scope()).unwrap();
    let record = access.request(handle.id()).unwrap();
    let view = record.trace.view();
    assert!(matches!(
        view.outcome,
        Some(neuromita::request::DiagnosticOutcome::Failed {
            stage: FailureStage::ProviderResponseMalformed,
            malformed: true,
            ..
        })
    ));
    assert!(
        view.provider_response
            .as_ref()
            .unwrap()
            .raw_body
            .text
            .contains("segments")
    );
    assert!(view.usage.actual.is_none());
    assert!(
        view.missing
            .iter()
            .any(|entry| entry.starts_with("actual_usage"))
    );
    let errors = access.errors(8).unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].malformed);
    assert_eq!(errors[0].request_id, handle.id().0);
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transport_partial_body_and_cancellation_are_visible() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let partial = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "partial".into())
        .unwrap();
    let call = next(&mut calls).await;
    call.reply
        .send(Scripted::Failure {
            error: GenerationError::Transport("connection reset".into()),
            reply: Some(ProviderReply {
                status: Some(200),
                raw_body: "{\"candidates\":[".into(),
                answer_text: None,
                complete: false,
                finish_reason: None,
                usage: None,
                elapsed: Duration::from_millis(9),
            }),
        })
        .unwrap();
    assert!(matches!(
        terminal(&partial).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(
            GenerationError::Transport(_)
        ))
    ));
    let access = ChatApi::diagnostics_for(scope()).unwrap();
    let view = access.request(partial.id()).unwrap().trace.view();
    assert!(!view.provider_response.as_ref().unwrap().complete);
    assert!(
        view.missing
            .iter()
            .any(|entry| entry.contains("incomplete body"))
    );

    let cancelled = app
        .presenter()
        .chat
        .reply(CharacterId::Crazy, "cancel".into())
        .unwrap();
    let waiting = next(&mut calls).await;
    assert_eq!(cancelled.cancel(), CancelResult::Requested);
    assert!(matches!(
        terminal(&cancelled).await.state,
        RequestState::Cancelled
    ));
    drop(waiting);

    let record = access.request(cancelled.id()).unwrap();
    let view = record.trace.view();
    assert!(matches!(
        view.outcome,
        Some(neuromita::request::DiagnosticOutcome::Cancelled)
    ));
    assert!(view.provider_request.is_some());
    assert!(view.prompt.is_some());
    assert!(view.provider_response.is_none());
    assert!(view.missing.iter().any(|entry| entry.contains("cancelled")));
    assert!(access.request(cancelled.id()).is_ok());
    app.shutdown().await.unwrap();
}

async fn frame(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).await.unwrap(),
                0,
                "connection closed"
            );
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if predicate(&value) {
                return value;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_diagnostics_are_scoped_to_the_observing_client() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let stream = TcpStream::connect(app.server().address()).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    frame(&mut reader, |value| value["type"] == "connected").await;
    write
        .write_all(
            format!(
                "{}\n",
                json!({"command_id": Uuid::new_v4(), "command": "initiate", "character": "Crazy", "instruction": "hello"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let accepted = frame(&mut reader, |value| value["type"] == "accepted").await;
    let request_id = Uuid::parse_str(accepted["request_id"].as_str().unwrap()).unwrap();
    let call = next(&mut calls).await;
    call.reply
        .send(Scripted::Success {
            raw: json!({"segments":[{"text":"answer"}]}).to_string(),
            usage: ProviderUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                total_tokens: Some(18),
            },
        })
        .unwrap();
    frame(&mut reader, |value| {
        value["type"] == "request_status" && value["state"] == "finished"
    })
    .await;
    write
        .write_all(
            format!(
                "{}\n",
                json!({"command_id": Uuid::new_v4(), "command": "diagnostics", "character": "Crazy", "request_id": request_id, "section": "usage"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let response = frame(&mut reader, |value| value["type"] == "diagnostics").await;
    assert_eq!(response["section"], "usage");
    assert_eq!(response["content"]["usage"]["actual"]["total_tokens"], 18);

    let foreign = TcpStream::connect(app.server().address()).await.unwrap();
    let (read, mut write) = foreign.into_split();
    let mut reader = BufReader::new(read);
    frame(&mut reader, |value| value["type"] == "connected").await;
    write
        .write_all(
            format!(
                "{}\n",
                json!({"command_id": Uuid::new_v4(), "command": "diagnostics", "character": "Crazy", "request_id": request_id})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let denied = frame(&mut reader, |value| value["type"] == "error").await;
    assert_eq!(denied["code"], "ownership_denied");
    app.shutdown().await.unwrap();
}
