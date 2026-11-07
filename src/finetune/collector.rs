use std::{path::Path, sync::Arc};

use serde_json::{Value, json};

use super::*;
use crate::{
    generation::{
        GenerationError, GenerationObserver, PreparedRequest, PromptRole, ProviderKind,
        ProviderReply, ProviderUsage,
    },
    request::{RequestId, RequestSource, TurnIntent},
    timeline::EventId,
};

#[derive(Debug, Clone)]
pub struct CaptureScope {
    pub request_id: RequestId,
    pub character_id: String,
    pub intent: TurnIntent,
    pub source: RequestSource,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureStatus {
    pub sample_id: Option<SampleId>,
    pub warning: Option<Arc<str>>,
}

/// Synchronous provider observer. A successful HTTP body is captured here
/// before provider decoding and application validation; persistence happens
/// afterwards and never replaces the provider result.
pub struct FineTuneCaptureObserver {
    enabled: bool,
    scope: CaptureScope,
    prepared: Option<PreparedRequest>,
    captured: Option<CapturedProviderResponse>,
    status: CaptureStatus,
}

impl FineTuneCaptureObserver {
    pub fn status(&self) -> &CaptureStatus {
        &self.status
    }

    pub fn captured(&self) -> Option<CapturedProviderResponse> {
        self.captured.clone()
    }

    fn record_warning(&mut self, warning: impl Into<Arc<str>>) {
        self.status.warning = Some(warning.into());
    }
}

impl GenerationObserver for FineTuneCaptureObserver {
    fn request_prepared(&mut self, request: PreparedRequest) {
        self.prepared = Some(request);
    }

    fn response_received(&mut self, response: ProviderReply) {
        if !self.enabled
            || self.captured.is_some()
            || !response.complete
            || !response
                .status
                .is_some_and(|status| (200..300).contains(&status))
        {
            return;
        }
        let Some(request) = self.prepared.take() else {
            self.record_warning("dataset capture omitted: provider request metadata unavailable");
            return;
        };
        self.captured = Some(CapturedProviderResponse {
            scope: self.scope.clone(),
            request,
            response,
        });
    }

    fn failed(&mut self, _error: &GenerationError) {}
}

#[derive(Clone)]
pub struct FineTuneCollector {
    store: FineTuneStore,
    enabled: bool,
    delivery: delivery::Delivery,
}

impl FineTuneCollector {
    pub fn from_settings(settings: &FineTuneSettings) -> Result<Self, FineTuneError> {
        settings.validate()?;
        let store = FineTuneStore::open(settings.data_dir.clone())?;
        let delivery = delivery::Delivery::start(store.clone(), settings.retention)?;
        Ok(Self {
            store,
            delivery,
            enabled: settings.enabled,
        })
    }

    /// Sample ids denote admitted observations, not a durability receipt.
    pub fn enqueue(
        &self,
        capture: CapturedProviderResponse,
    ) -> Result<CaptureStatus, FineTuneError> {
        if !self.enabled {
            return Ok(CaptureStatus::default());
        }
        let sample = build_sample(&capture.scope, &capture.request, &capture.response);
        let sample_id = sample.id;
        self.delivery.sample(sample)?;
        Ok(CaptureStatus {
            sample_id: Some(sample_id),
            warning: None,
        })
    }

    pub fn enqueue_outcome(
        &self,
        sample_id: SampleId,
        request_id: RequestId,
        accepted_event_id: Option<EventId>,
        outcome: SampleOutcome,
    ) -> Result<(), FineTuneError> {
        self.delivery.outcome(OutcomeRecord {
            sample_id,
            request_id: Some(request_id),
            accepted_event_id,
            outcome,
            recorded_at: UtcTimestamp::now(),
        })
    }

    pub(crate) fn stop_delivery(&self) {
        self.delivery.stop();
    }

    pub fn delivery_status(&self) -> DeliveryStatus {
        self.delivery.status()
    }

    /// Waits only up to the supplied budget. With close=true, admission stops,
    /// then any remaining queued work is discarded after the deadline.
    pub async fn drain(&self, budget: std::time::Duration, close: bool) -> DeliveryStatus {
        self.delivery.drain(budget, close).await
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn store(&self) -> &FineTuneStore {
        &self.store
    }

    pub fn data_dir(&self) -> &Path {
        self.store.root()
    }

    pub fn observer(&self, scope: CaptureScope) -> FineTuneCaptureObserver {
        FineTuneCaptureObserver {
            enabled: self.enabled,
            scope,
            prepared: None,
            captured: None,
            status: CaptureStatus::default(),
        }
    }

    pub fn rate(&self, sample_id: SampleId, rating: Rating) -> Result<RatingUpdate, FineTuneError> {
        self.store.set_rating(sample_id, rating)
    }
}

#[derive(Debug, Clone)]
pub struct CapturedProviderResponse {
    pub scope: CaptureScope,
    pub request: PreparedRequest,
    pub response: ProviderReply,
}

fn build_sample(
    scope: &CaptureScope,
    request: &PreparedRequest,
    response: &ProviderReply,
) -> FineTuneSample {
    let response_text = response
        .answer_text
        .as_deref()
        .unwrap_or_default()
        .to_owned();
    let messages = request
        .prompt
        .iter()
        .map(|message| {
            SampleMessage::text(
                match message.role {
                    PromptRole::System => MessageRole::System,
                    PromptRole::User => MessageRole::User,
                    PromptRole::Assistant => MessageRole::Assistant,
                },
                message.text.as_ref(),
            )
        })
        .collect();
    let model = Some(request.model.to_string());
    let mut sample = FineTuneSample::new(
        UtcTimestamp::now(),
        scope.character_id.clone(),
        model,
        messages,
        response_text.clone(),
    );
    sample.id = SampleId::for_request(scope.request_id);
    sample.character_name = scope.character_id.clone();
    sample.provider_name = Some(
        match request.provider {
            ProviderKind::Gemini => "gemini",
            ProviderKind::OpenAi => "openai",
        }
        .into(),
    );
    sample.protocol_id = Some(request.protocol.to_string());
    sample.dialect_id = Some(request.dialect.to_string());

    sample.temperature = request.temperature.map(f64::from);
    sample.max_tokens = request.max_output_tokens.map(u64::from);
    sample.request_payload = Some(request.payload_json.to_string());
    sample.provider_response = Some(response.raw_body.to_string());
    sample.raw_response = Some(response_text);
    sample.request_id = Some(scope.request_id);
    sample.intent = Some(match scope.intent {
        TurnIntent::Reply(_) => "reply".into(),
        TurnIntent::React(_) => "react".into(),
        TurnIntent::Initiate => "initiate".into(),
    });
    sample.source = Some(match scope.source {
        RequestSource::Presenter => "presenter".into(),
        RequestSource::Scheduler => "scheduler".into(),
        RequestSource::Client(id) => format!("client:{id}"),
        RequestSource::Game(id) => format!("game:{id}"),
    });
    sample.usage = response.usage.map(usage_payload);
    sample.usage_source = response
        .usage
        .is_some()
        .then_some("provider_response_usage_metadata".into());
    sample
}

fn usage_payload(usage: ProviderUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_uses_normalized_answer_without_parsing_provider_wire_json() {
        let scope = CaptureScope {
            request_id: RequestId::new(),
            character_id: "mita".into(),
            intent: TurnIntent::Initiate,
            source: RequestSource::Presenter,
        };
        let request = PreparedRequest {
            protocol: "google_gemini_default".into(),
            dialect: "gemini_generate_content".into(),
            provider: ProviderKind::Gemini,
            model: "fixture".into(),
            endpoint: "https://example.test".into(),
            temperature: Some(1.0),
            max_output_tokens: Some(128),
            prompt: Vec::new().into(),
            payload_json: "opaque request".into(),
            captured_at: std::time::SystemTime::now(),
        };
        let response = ProviderReply {
            status: Some(200),
            raw_body: "opaque provider response".into(),
            answer_text: Some("normalized answer".into()),
            complete: true,
            finish_reason: None,
            usage: None,
            elapsed: std::time::Duration::ZERO,
        };
        let sample = build_sample(&scope, &request, &response);
        assert_eq!(sample.response, "normalized answer");
        assert_eq!(
            sample.provider_response.as_deref(),
            Some("opaque provider response")
        );
    }
}
