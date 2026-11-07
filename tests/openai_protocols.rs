use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        openai::{OpenAiPresetDefinition, OpenAiProtocol, OpenAiProvider, OpenAiTemplate},
    },
    request::{RequestOutcome, RequestState},
};
use serde_json::{Value, json};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::oneshot,
};

#[tokio::test]
async fn all_http_profiles_execute_through_the_presenter_with_their_own_wire_format() {
    for protocol in [
        OpenAiProtocol::Compatible,
        OpenAiProtocol::Mistral,
        OpenAiProtocol::OpenRouter,
        OpenAiProtocol::Aiio,
        OpenAiProtocol::LmStudio,
        OpenAiProtocol::Responses,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&line);
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            tx.send((headers, body)).unwrap();
            let answer = r#"{"segments":[{"text":"fixture answer"}]}"#;
            let body = if protocol == OpenAiProtocol::Responses {
                json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}]})
            } else {
                json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":answer}}]})
            };
            let body = serde_json::to_string(&body).unwrap();
            reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let no_auth = protocol == OpenAiProtocol::LmStudio;
        let config = OpenAiPresetDefinition {
            protocol,
            endpoint,
            model: "vendor/model:free".into(),
            credential: (!no_auth).then(|| "FIXTURE_KEY".into()),
            temperature: 0.7,
            max_output_tokens: 128,
            timeout_ms: 2000,
            options: protocol.options(),
            transforms: protocol.transforms(),
        };
        let template = OpenAiTemplate::new(config, (!no_auth).then_some("fixture-secret")).unwrap();
        assert!(!format!("{template:?}").contains("fixture-secret"));
        let limits = RuntimeLimits {
            request_queue: NonZeroUsize::new(2).unwrap(),
            discovery_buffer: NonZeroUsize::new(8).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(8).unwrap(),
            retained_finished: NonZeroUsize::new(4).unwrap(),
        };
        let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
        let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
            GenerationPreset::new(ProviderTemplate::OpenAi(template)),
            ResponseGenerator::new([provider]).unwrap(),
            limits,
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let request = app
            .presenter()
            .conversation
            .reply(CharacterId::Crazy, "hello".into())
            .unwrap();
        let (headers, body) = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap();
        app.shutdown().await.unwrap();
        server.await.unwrap();
        assert!(
            matches!(snapshot.state, RequestState::Finished(RequestOutcome::Success(ref response)) if response.text.as_ref() == "fixture answer")
        );
        assert_eq!(body["model"], "vendor/model:free");
        assert_eq!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"),
            !no_auth
        );
        if protocol == OpenAiProtocol::Responses {
            assert!(body.get("messages").is_none());
            assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
            assert_eq!(body["text"]["format"]["type"], "json_schema");
            assert_eq!(body["max_output_tokens"], 128);
        } else {
            assert!(body.get("input").is_none());
            assert_eq!(body["max_tokens"], 128);
        }
        if protocol == OpenAiProtocol::OpenRouter {
            assert!(headers.contains("NeuroMita"));
            assert_eq!(body["response_format"]["type"], "json_object");
        }
        if protocol == OpenAiProtocol::Aiio {
            assert!(body.get("response_format").is_none());
            assert!(
                body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|m| m["role"] != "system")
            );
            assert!(
                body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("[SYSTEM CONTEXT]")
            );
        }
    }
}
