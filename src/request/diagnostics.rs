use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Weak},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use super::{
    EffectPhase, ProcessingStage, RequestId, RequestMetadata, RequestOutcome, RequestSource,
    RequestState, RequestTransition, TurnIntent,
};
use crate::{
    character::CharacterContext,
    generation::{
        GenerationError, PreparedRequest, PromptRole, ProviderKind, ProviderReply, ProviderUsage,
    },
    identity::CharacterId,
};

pub const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;
const ESTIMATE_METHOD: &str = "local ceil(chars/4); not provider usage";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsLimits {
    pub retained_traces: usize,
    pub retained_errors: usize,
    pub retained_expired: usize,
    pub max_text_bytes: usize,
    pub max_body_bytes: usize,
    pub max_messages: usize,
    pub max_transitions: usize,
}

impl Default for DiagnosticsLimits {
    fn default() -> Self {
        Self {
            retained_traces: 64,
            retained_errors: 32,
            retained_expired: 128,
            max_text_bytes: 16 * 1024,
            max_body_bytes: 32 * 1024,
            max_messages: 64,
            max_transitions: 32,
        }
    }
}

impl DiagnosticsLimits {
    pub fn with_retention(mut self, traces: usize, errors: usize) -> Self {
        self.retained_traces = traces.max(1);
        self.retained_errors = errors.max(1);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundedText {
    pub text: String,
    pub bytes: usize,
    pub truncated: bool,
}

impl BoundedText {
    fn capture(redactor: &mut DiagnosticRedactor, value: &str, limit: usize) -> Self {
        let value = redactor.redact(value);
        let bytes = value.len();
        let text = truncate_utf8(&value, limit).to_owned();
        Self {
            truncated: text.len() < value.len(),
            text,
            bytes,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RedactionSummary {
    pub credential_occurrences: usize,
    pub auth_url_occurrences: usize,
    pub policy: String,
}

#[derive(Clone, Default)]
struct DiagnosticRedactor {
    secrets: Vec<String>,
    redactions: RedactionSummary,
}

impl fmt::Debug for DiagnosticRedactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiagnosticRedactor")
            .field("configured_secrets", &self.secrets.len())
            .field("redactions", &self.redactions)
            .finish()
    }
}

impl DiagnosticRedactor {
    fn configure(&mut self, values: impl IntoIterator<Item = String>) {
        for value in values {
            if value.is_empty() {
                continue;
            }
            for variant in [
                value.clone(),
                percent_encode(&value, false),
                percent_encode(&value, true),
                escaped_json_variant(&value),
            ] {
                if !self.secrets.contains(&variant) {
                    self.secrets.push(variant);
                }
            }
        }
        self.secrets
            .sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        self.redactions.policy = "provider credentials, URL userinfo, query and fragment values, encoded credential variants and foreign owners are redacted or hidden".into();
    }

    fn redact(&mut self, value: &str) -> String {
        let mut text = value.to_owned();
        for secret in &self.secrets {
            let count = text.matches(secret.as_str()).count();
            if count > 0 {
                self.redactions.credential_occurrences += count;
                text = text.replace(secret.as_str(), "[REDACTED]");
            }
        }
        let text = redact_parameters(&mut self.redactions, &text);
        redact_urls(&mut self.redactions, &text)
    }
}

fn percent_encode(value: &str, form: bool) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else if form && byte == b' ' {
            encoded.push('+');
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn escaped_json_variant(value: &str) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""));
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

fn redact_parameters(summary: &mut RedactionSummary, value: &str) -> String {
    const NAMES: [&str; 9] = [
        "authorization",
        "access_token",
        "refresh_token",
        "api_key",
        "api-key",
        "password",
        "secret",
        "token",
        "key",
    ];
    let lower = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    let lower_bytes = lower.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut last = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if let Some((name_end, value_start, value_end)) =
            parameter_at(&lower, lower_bytes, bytes, cursor, &NAMES)
        {
            output.push_str(&value[last..value_start]);
            output.push_str("[REDACTED]");
            summary.credential_occurrences += 1;
            last = value_end;
            cursor = value_end.max(name_end);
            continue;
        }
        cursor += next_char_len(value, cursor);
    }
    output.push_str(&value[last..]);
    output
}

fn parameter_at(
    lower: &str,
    lower_bytes: &[u8],
    bytes: &[u8],
    cursor: usize,
    names: &[&str],
) -> Option<(usize, usize, usize)> {
    let before_ok = cursor == 0
        || lower_bytes[cursor - 1].is_ascii_whitespace()
        || matches!(
            lower_bytes[cursor - 1],
            b'"' | b'\'' | b'{' | b'(' | b'[' | b'&' | b'?' | b',' | b';'
        );
    if !before_ok {
        return None;
    }
    let name = names
        .iter()
        .find(|name| lower[cursor..].starts_with(**name))?;
    let mut separator = cursor + name.len();
    while separator < bytes.len() && bytes[separator].is_ascii_whitespace() {
        separator += 1;
    }
    if separator < bytes.len() && matches!(bytes[separator], b'"' | b'\'') {
        separator += 1;
    }
    if separator >= bytes.len() || !matches!(bytes[separator], b'=' | b':') {
        return None;
    }
    let mut value_start = separator + 1;
    while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
        value_start += 1;
    }
    let quote = (value_start < bytes.len() && matches!(bytes[value_start], b'"' | b'\''))
        .then(|| bytes[value_start]);
    if quote.is_some() {
        value_start += 1;
    }
    let mut value_end = value_start;
    match quote {
        Some(quote) => {
            while value_end < bytes.len() {
                match bytes[value_end] {
                    b'\\' => value_end = (value_end + 2).min(bytes.len()),
                    byte if byte == quote => break,
                    b'\n' | b'\r' => break,
                    _ => value_end += 1,
                }
            }
        }
        None if *name == "authorization" => {
            while value_end < bytes.len() && !matches!(bytes[value_end], b'\n' | b'\r' | b'"') {
                value_end += 1;
            }
        }
        None => {
            while value_end < bytes.len()
                && !bytes[value_end].is_ascii_whitespace()
                && !matches!(
                    bytes[value_end],
                    b'"' | b'\'' | b'&' | b',' | b';' | b'}' | b']'
                )
            {
                value_end += 1;
            }
        }
    }
    (value_end > value_start).then_some((cursor + name.len(), value_start, value_end))
}

fn next_char_len(value: &str, cursor: usize) -> usize {
    value[cursor..].chars().next().map_or(1, char::len_utf8)
}

fn redact_urls(summary: &mut RedactionSummary, value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(position) = find_scheme(rest) {
        output.push_str(&rest[..position]);
        let token_source = &rest[position..];
        let end = token_source
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | ',')
            })
            .unwrap_or(token_source.len());
        let (token, redacted) = sanitize_url(&token_source[..end]);
        if redacted {
            summary.auth_url_occurrences += 1;
        }
        output.push_str(&token);
        rest = &token_source[end..];
    }
    output.push_str(rest);
    output
}

fn find_scheme(value: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    ["https://", "http://"]
        .into_iter()
        .filter_map(|scheme| lower.find(scheme))
        .min()
}

fn sanitize_url(token: &str) -> (String, bool) {
    let Some(scheme_end) = token.find("://") else {
        return (token.to_owned(), false);
    };
    let (scheme, rest) = token.split_at(scheme_end + 3);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let mut redacted = false;
    let authority = if let Some((_, host)) = authority.rsplit_once('@') {
        redacted = true;
        format!("[REDACTED]@{host}")
    } else {
        authority.to_owned()
    };
    let tail = if let Some((path, _)) = tail.split_once('?') {
        redacted = true;
        format!("{path}?[REDACTED]")
    } else if let Some((path, _)) = tail.split_once('#') {
        redacted = true;
        format!("{path}#[REDACTED]")
    } else {
        tail.to_owned()
    };
    (format!("{scheme}{authority}{tail}"), redacted)
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosisMetadata {
    pub id: Uuid,
    pub character: CharacterId,
    pub source: RequestSource,
    pub intent: TurnIntent,
}

impl From<&RequestMetadata> for DiagnosisMetadata {
    fn from(metadata: &RequestMetadata) -> Self {
        Self {
            id: metadata.id.0,
            character: metadata.character_id,
            source: metadata.source,
            intent: metadata.intent,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceMark {
    pub name: String,
    pub offset_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TraceTiming {
    pub started_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub marks: Vec<TraceMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticState {
    Pending,
    Processing { stage: String },
    Finished { outcome: String },
    Cancelled,
}

impl From<&RequestState> for DiagnosticState {
    fn from(state: &RequestState) -> Self {
        match state {
            RequestState::Pending => Self::Pending,
            RequestState::Processing(ProcessingStage::GeneratingText) => Self::Processing {
                stage: "generating_text".into(),
            },
            RequestState::Processing(ProcessingStage::TextReady { .. }) => Self::Processing {
                stage: "text_ready".into(),
            },
            RequestState::Processing(ProcessingStage::GeneratingVoice { .. }) => Self::Processing {
                stage: "generating_voice".into(),
            },
            RequestState::Processing(ProcessingStage::Accepting { .. }) => Self::Processing {
                stage: "accepting".into(),
            },
            RequestState::Cancelled => Self::Cancelled,
            RequestState::Finished(outcome) => Self::Finished {
                outcome: outcome_name(outcome).into(),
            },
        }
    }
}

fn outcome_name(outcome: &RequestOutcome) -> &'static str {
    match outcome {
        RequestOutcome::Success(_) => "success",
        RequestOutcome::TextGenerationFailed(_) => "text_generation_failed",
        RequestOutcome::VoiceGenerationFailed { .. } => "voice_generation_failed",
        RequestOutcome::InternalError(_) => "internal_error",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticTransition {
    pub revision: u64,
    pub phase: EffectPhase,
    pub state: DiagnosticState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Queue,
    PromptPreparation,
    ProviderConfiguration,
    ProviderTransport,
    ProviderHttp,
    ProviderResponse,
    ProviderResponseMalformed,
    VoiceSynthesis,
    Acceptance,
    Internal,
}

impl FailureStage {
    fn from_generation(error: &GenerationError, malformed: bool) -> Self {
        match error {
            GenerationError::Unavailable(_) | GenerationError::Duplicate(_) => {
                Self::ProviderConfiguration
            }
            GenerationError::Transport(_) => Self::ProviderTransport,
            GenerationError::Http(_) => Self::ProviderHttp,
            GenerationError::Response(_) if malformed => Self::ProviderResponseMalformed,
            GenerationError::Response(_) => Self::ProviderResponse,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenEstimatePart {
    pub index: usize,
    pub role: String,
    pub chars: usize,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenEstimate {
    pub method: String,
    pub input_chars: usize,
    pub output_chars: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub per_message: Vec<TokenEstimatePart>,
}

impl TokenEstimate {
    fn tokens(chars: usize) -> u64 {
        (chars as u64).div_ceil(4)
    }

    fn set_input(&mut self, parts: impl IntoIterator<Item = (String, usize)>) {
        self.method = ESTIMATE_METHOD.into();
        self.per_message.clear();
        self.input_chars = 0;
        for (index, (role, chars)) in parts.into_iter().enumerate() {
            self.input_chars += chars;
            self.per_message.push(TokenEstimatePart {
                index,
                role,
                chars,
                estimated_tokens: Self::tokens(chars),
            });
        }
        self.input_tokens = Self::tokens(self.input_chars);
        self.recalculate();
    }

    fn set_output(&mut self, chars: usize) {
        self.method = ESTIMATE_METHOD.into();
        self.output_chars = chars;
        self.output_tokens = Self::tokens(chars);
        self.recalculate();
    }

    fn recalculate(&mut self) {
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ActualUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl From<&ProviderUsage> for ActualUsage {
    fn from(usage: &ProviderUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTrace {
    pub estimated: TokenEstimate,
    pub actual: Option<ActualUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptMessageTrace {
    pub index: usize,
    pub role: String,
    pub text: BoundedText,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PromptTrace {
    pub system_instruction: Option<BoundedText>,
    pub messages: Vec<PromptMessageTrace>,
    pub messages_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextSummary {
    pub branch_id: Uuid,
    pub through: u64,
    pub history_events: usize,
    pub memories: usize,
    pub memory_islands: usize,
    pub accepted_replies: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderRequestTrace {
    pub provider: String,
    pub protocol: String,
    pub dialect: String,
    pub model: BoundedText,
    pub endpoint: BoundedText,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub body: BoundedText,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderResponseTrace {
    pub status: Option<u16>,
    pub raw_body: BoundedText,
    pub complete: bool,
    pub finish_reason: Option<BoundedText>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticOutcome {
    Success {
        event_id: Uuid,
    },
    Cancelled,
    Failed {
        stage: FailureStage,
        message: BoundedText,
        malformed: bool,
        partial_text: Option<BoundedText>,
    },
    PartialError {
        stage: FailureStage,
        message: BoundedText,
        accepted_text: BoundedText,
    },
}

impl DiagnosticOutcome {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::PartialError { .. })
    }
}

#[derive(Debug, Clone)]
pub struct RequestTrace {
    pub metadata: RequestMetadata,
    pub guidance: Arc<str>,
    pub preset_description: String,
    pub context: Option<ContextSummary>,
    pub generated_text: Option<Arc<str>>,
    pub transitions: Vec<RequestTransition>,
    pub schema_version: u32,
    pub input: BoundedText,
    pub context_summary: Option<ContextSummary>,
    pub prompt: Option<PromptTrace>,
    pub provider_request: Option<ProviderRequestTrace>,
    pub provider_response: Option<ProviderResponseTrace>,
    pub provider_error: Option<BoundedText>,
    pub usage: UsageTrace,
    pub outcome: Option<DiagnosticOutcome>,
    pub timing: TraceTiming,
    pub redaction: RedactionSummary,
    pub complete: bool,
    started: Instant,
    prompt_started: Option<Instant>,
    provider_started: Option<Instant>,
    limits: DiagnosticsLimits,
    redactor: DiagnosticRedactor,
}

impl RequestTrace {
    pub(crate) fn new(
        metadata: RequestMetadata,
        guidance: &str,
        limits: DiagnosticsLimits,
    ) -> Self {
        let started = Instant::now();
        let mut redactor = DiagnosticRedactor::default();
        redactor.configure([]);
        let input = BoundedText::capture(&mut redactor, guidance, limits.max_text_bytes);
        let guidance = Arc::from(input.text.clone());
        Self {
            metadata,
            guidance,
            preset_description: String::new(),
            context: None,
            generated_text: None,
            transitions: vec![RequestTransition {
                revision: 0,
                phase: EffectPhase::Working,
                state: DiagnosticState::Pending,
            }],
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            input,
            context_summary: None,
            prompt: None,
            provider_request: None,
            provider_response: None,
            provider_error: None,
            usage: UsageTrace::default(),
            outcome: None,
            timing: TraceTiming {
                started_at_unix_ms: unix_ms(),
                elapsed_ms: 0,
                marks: vec![TraceMark {
                    name: "enqueued".into(),
                    offset_ms: 0,
                }],
            },
            redaction: redactor.redactions.clone(),
            complete: false,
            started,
            prompt_started: None,
            provider_started: None,
            limits,
            redactor,
        }
    }

    pub(crate) fn configure_redaction(&mut self, secrets: impl IntoIterator<Item = String>) {
        self.redactor.configure(secrets);
        let guidance = self.redactor.redact(&self.guidance);
        self.guidance = Arc::from(guidance);
        self.preset_description = self.redactor.redact(&self.preset_description);
        self.input = BoundedText::capture(
            &mut self.redactor,
            &self.input.text,
            self.limits.max_text_bytes,
        );
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn set_preset_description(&mut self, description: String) {
        self.preset_description = self.redactor.redact(&description);
        self.preset_description =
            truncate_utf8(&self.preset_description, self.limits.max_text_bytes).to_owned();
        self.redaction = self.redactor.redactions.clone();
    }

    fn mark(&mut self, name: &str) {
        let offset_ms = self.started.elapsed().as_millis() as u64;
        self.timing.elapsed_ms = offset_ms;
        self.timing.marks.push(TraceMark {
            name: name.into(),
            offset_ms,
        });
    }

    pub(crate) fn mark_started(&mut self) {
        self.mark("started");
        self.prompt_started = Some(Instant::now());
    }

    pub(crate) fn prepared(&mut self, context: &CharacterContext) {
        self.mark("prepared");
        self.context_summary = Some(ContextSummary {
            branch_id: context.anchor.branch_id,
            through: context.anchor.through,
            history_events: context.history.events.len(),
            memories: context.memories.len(),
            memory_islands: context.memory_islands.len(),
            accepted_replies: context.state.accepted_replies,
        });
        let current = context.current_input.as_ref();
        let ingress = if current.is_empty() {
            ingress_text(context).unwrap_or_default()
        } else {
            current
        };
        if !ingress.is_empty() {
            self.input =
                BoundedText::capture(&mut self.redactor, ingress, self.limits.max_text_bytes);
        }
        self.context = self.context_summary.clone();
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn provider_requested(&mut self, request: PreparedRequest) {
        self.mark("provider_request");
        self.provider_started = Some(Instant::now());
        let limit = self.limits.max_text_bytes;
        let body_limit = self.limits.max_body_bytes;
        let system_instruction = request
            .prompt
            .iter()
            .find(|message| matches!(message.role, PromptRole::System))
            .map(|message| BoundedText::capture(&mut self.redactor, &message.text, limit));
        let mut prompt = PromptTrace {
            system_instruction,
            messages: Vec::new(),
            messages_truncated: false,
        };
        let mut parts = Vec::new();
        for message in request
            .prompt
            .iter()
            .filter(|message| !matches!(message.role, PromptRole::System))
        {
            if prompt.messages.len() >= self.limits.max_messages {
                prompt.messages_truncated = true;
                break;
            }
            let role = match message.role {
                PromptRole::User => "user",
                PromptRole::Assistant => "assistant",
                PromptRole::System => continue,
            };
            let text = BoundedText::capture(&mut self.redactor, &message.text, limit);
            parts.push((role.to_owned(), text.text.chars().count()));
            prompt.messages.push(PromptMessageTrace {
                index: prompt.messages.len(),
                role: role.into(),
                text,
            });
        }
        if let Some(system) = &prompt.system_instruction {
            parts.insert(0, ("system".into(), system.text.chars().count()));
        }
        self.usage.estimated.set_input(parts);
        self.prompt = Some(prompt);
        self.provider_request = Some(ProviderRequestTrace {
            provider: provider_name(request.provider),
            protocol: request.protocol.to_string(),
            dialect: request.dialect.to_string(),
            model: BoundedText::capture(&mut self.redactor, &request.model, limit),
            endpoint: BoundedText::capture(&mut self.redactor, &request.endpoint, limit),
            temperature: request.temperature,
            max_output_tokens: request.max_output_tokens,
            body: BoundedText::capture(&mut self.redactor, &request.payload_json, body_limit),
        });
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn provider_replied(&mut self, reply: ProviderReply) {
        self.mark("provider_response");
        let limit = self.limits.max_text_bytes;
        self.provider_response = Some(ProviderResponseTrace {
            status: reply.status,
            raw_body: BoundedText::capture(
                &mut self.redactor,
                &reply.raw_body,
                self.limits.max_body_bytes,
            ),
            complete: reply.complete,
            finish_reason: reply
                .finish_reason
                .as_ref()
                .map(|reason| BoundedText::capture(&mut self.redactor, reason, limit)),
            elapsed_ms: reply.elapsed.as_millis() as u64,
        });
        self.usage.actual = reply.usage.as_ref().map(ActualUsage::from);
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn provider_failed(&mut self, error: &GenerationError) {
        self.mark("provider_failed");
        self.provider_error = Some(BoundedText::capture(
            &mut self.redactor,
            &error.to_string(),
            self.limits.max_text_bytes,
        ));
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn text_ready(&mut self, text: &str) {
        self.mark("text_ready");
        self.usage.estimated.set_output(text.chars().count());
        let text = BoundedText::capture(&mut self.redactor, text, self.limits.max_text_bytes);
        self.generated_text = Some(Arc::from(text.text));
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn voice_started(&mut self) {
        self.mark("voice_started");
    }

    pub(crate) fn voice_finished(&mut self) {
        self.mark("voice_finished");
    }

    pub(crate) fn accepting(&mut self) {
        self.mark("accepting");
    }

    pub(crate) fn outcome(&mut self, outcome: &RequestOutcome) {
        let malformed = self.provider_response.is_some()
            && matches!(
                outcome,
                RequestOutcome::TextGenerationFailed(GenerationError::Response(_))
            );
        self.outcome = Some(match outcome {
            RequestOutcome::Success(response) => DiagnosticOutcome::Success {
                event_id: response.event_id.0,
            },
            RequestOutcome::TextGenerationFailed(error) => DiagnosticOutcome::Failed {
                stage: FailureStage::from_generation(error, malformed),
                message: BoundedText::capture(
                    &mut self.redactor,
                    &error.to_string(),
                    self.limits.max_text_bytes,
                ),
                malformed,
                partial_text: self.generated_text.as_ref().map(|text| {
                    BoundedText::capture(&mut self.redactor, text, self.limits.max_text_bytes)
                }),
            },
            RequestOutcome::VoiceGenerationFailed { response, error } => {
                DiagnosticOutcome::PartialError {
                    stage: FailureStage::VoiceSynthesis,
                    message: BoundedText::capture(
                        &mut self.redactor,
                        &error.to_string(),
                        self.limits.max_text_bytes,
                    ),
                    accepted_text: BoundedText::capture(
                        &mut self.redactor,
                        &response.text,
                        self.limits.max_text_bytes,
                    ),
                }
            }
            RequestOutcome::InternalError(error) => DiagnosticOutcome::Failed {
                stage: FailureStage::Internal,
                message: BoundedText::capture(
                    &mut self.redactor,
                    &error.message,
                    self.limits.max_text_bytes,
                ),
                malformed: false,
                partial_text: error.partial_text.as_ref().map(|text| {
                    BoundedText::capture(&mut self.redactor, text, self.limits.max_text_bytes)
                }),
            },
        });
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn cancelled(&mut self) {
        self.outcome = Some(DiagnosticOutcome::Cancelled);
        self.redaction = self.redactor.redactions.clone();
    }

    pub(crate) fn finished(&mut self) {
        self.mark("finished");
        self.timing.elapsed_ms = self.started.elapsed().as_millis() as u64;
        self.complete = true;
    }

    pub(crate) fn push_transition(
        &mut self,
        revision: u64,
        phase: EffectPhase,
        state: &RequestState,
    ) {
        self.transitions.push(RequestTransition {
            revision,
            phase,
            state: DiagnosticState::from(state),
        });
        let limit = self.limits.max_transitions;
        if self.transitions.len() > limit {
            self.transitions.remove(1.min(self.transitions.len() - 1));
        }
    }

    pub fn error_summary(&self) -> Option<DiagnosticErrorSummary> {
        let (stage, message, malformed, partial_text) = match self.outcome.as_ref()? {
            DiagnosticOutcome::Failed {
                stage,
                message,
                malformed,
                partial_text,
            } => (*stage, message.clone(), *malformed, partial_text.clone()),
            DiagnosticOutcome::PartialError {
                stage,
                message,
                accepted_text,
            } => (*stage, message.clone(), false, Some(accepted_text.clone())),
            DiagnosticOutcome::Success { .. } | DiagnosticOutcome::Cancelled => return None,
        };
        Some(DiagnosticErrorSummary {
            request_id: self.metadata.id.0,
            character: self.metadata.character_id,
            source: self.metadata.source,
            stage,
            malformed,
            message,
            partial_text,
            at_unix_ms: self.timing.started_at_unix_ms,
        })
    }

    pub fn summary(&self) -> RequestTraceSummary {
        RequestTraceSummary {
            request_id: self.metadata.id.0,
            character: self.metadata.character_id,
            source: self.metadata.source,
            intent: self.metadata.intent,
            complete: self.complete,
            outcome: self.outcome.as_ref().map(|outcome| match outcome {
                DiagnosticOutcome::Success { .. } => "success",
                DiagnosticOutcome::Cancelled => "cancelled",
                DiagnosticOutcome::Failed { .. } => "failed",
                DiagnosticOutcome::PartialError { .. } => "partial_error",
            }),
            started_at_unix_ms: self.timing.started_at_unix_ms,
            elapsed_ms: self.timing.elapsed_ms,
        }
    }

    pub fn view(&self) -> DiagnosticView {
        DiagnosticView {
            schema_version: self.schema_version,
            request: DiagnosisMetadata::from(&self.metadata),
            input: self.input.clone(),
            preset_description: self.preset_description.clone(),
            context: self.context_summary.clone(),
            prompt: self.prompt.clone(),
            provider_request: self.provider_request.clone(),
            provider_response: self.provider_response.clone(),
            provider_error: self.provider_error.clone(),
            usage: self.usage.clone(),
            outcome: self.outcome.clone(),
            transitions: self
                .transitions
                .iter()
                .map(|transition| DiagnosticTransition {
                    revision: transition.revision,
                    phase: transition.phase,
                    state: transition.state.clone(),
                })
                .collect(),
            timing: self.timing.clone(),
            redaction: self.redaction.clone(),
            complete: self.complete,
            missing: self.missing_artifacts(),
        }
    }

    fn missing_artifacts(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if self.prompt.is_none() {
            missing.push(match self.outcome.as_ref() {
                Some(DiagnosticOutcome::Cancelled) => {
                    "prompt: not prepared before cancellation".into()
                }
                _ => "prompt: provider was not invoked".into(),
            });
        }
        if self.provider_request.is_none() {
            missing.push("provider_request: not sent".into());
        }
        if self.provider_response.is_none() {
            missing.push(match self.outcome.as_ref() {
                Some(DiagnosticOutcome::Cancelled) => {
                    "provider_response: cancelled before a body arrived".into()
                }
                Some(DiagnosticOutcome::Failed {
                    stage: FailureStage::ProviderTransport,
                    ..
                }) => "provider_response: transport failed".into(),
                _ => "provider_response: no body received".into(),
            });
        }
        if self.usage.actual.is_none() {
            missing.push("actual_usage: provider did not report usage".into());
        }
        if self
            .provider_response
            .as_ref()
            .is_some_and(|response| !response.complete)
        {
            missing.push("provider_response: incomplete body (partial prefix retained)".into());
        }
        missing
    }

    pub fn error_history_entry(&self) -> Option<DiagnosticErrorSummary> {
        self.error_summary()
    }

    pub fn section_json(&self, section: DiagnosticSection) -> Result<String, serde_json::Error> {
        let view = self.view();
        let value = serde_json::to_value(&view)?;
        let selected = match section {
            DiagnosticSection::All => value,
            DiagnosticSection::Ingress => serde_json::json!({
                "request": view.request,
                "input": view.input,
                "preset": view.preset_description,
            }),
            DiagnosticSection::Prompt => serde_json::json!({
                "context": view.context,
                "prompt": view.prompt,
            }),
            DiagnosticSection::Request => serde_json::json!({
                "provider_request": view.provider_request,
            }),
            DiagnosticSection::Response => serde_json::json!({
                "provider_response": view.provider_response,
                "provider_error": view.provider_error,
            }),
            DiagnosticSection::Usage => serde_json::json!({
                "usage": view.usage,
            }),
            DiagnosticSection::Error => serde_json::json!({
                "outcome": view.outcome,
                "missing": view.missing,
                "redaction": view.redaction,
            }),
            DiagnosticSection::Lifecycle => serde_json::json!({
                "timing": view.timing,
                "transitions": view.transitions,
                "complete": view.complete,
            }),
        };
        serde_json::to_string_pretty(&selected)
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.view())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticView {
    pub schema_version: u32,
    pub request: DiagnosisMetadata,
    pub input: BoundedText,
    pub preset_description: String,
    pub context: Option<ContextSummary>,
    pub prompt: Option<PromptTrace>,
    pub provider_request: Option<ProviderRequestTrace>,
    pub provider_response: Option<ProviderResponseTrace>,
    pub provider_error: Option<BoundedText>,
    pub usage: UsageTrace,
    pub outcome: Option<DiagnosticOutcome>,
    pub transitions: Vec<DiagnosticTransition>,
    pub timing: TraceTiming,
    pub redaction: RedactionSummary,
    pub complete: bool,
    pub missing: Vec<String>,
}

fn provider_name(kind: ProviderKind) -> String {
    match kind {
        ProviderKind::Gemini => "gemini".into(),
        ProviderKind::OpenAi => "openai".into(),
    }
}

fn ingress_text(context: &CharacterContext) -> Option<&str> {
    let event_id = match context.focus.intent {
        TurnIntent::Reply(id) | TurnIntent::React(id) => id,
        TurnIntent::Initiate => return None,
    };
    context
        .history
        .events
        .iter()
        .find(|event| event.id == event_id)
        .map(|event| match &event.content {
            crate::timeline::EventContent::PlayerMessage { text }
            | crate::timeline::EventContent::Observation { text }
            | crate::timeline::EventContent::Reaction { text, .. }
            | crate::timeline::EventContent::Initiation { text }
            | crate::timeline::EventContent::CharacterReply { text, .. } => text.as_ref(),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSection {
    All,
    Ingress,
    Prompt,
    Request,
    Response,
    Usage,
    Error,
    Lifecycle,
}

impl DiagnosticSection {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "all" => Self::All,
            "ingress" => Self::Ingress,
            "prompt" => Self::Prompt,
            "request" => Self::Request,
            "response" => Self::Response,
            "usage" => Self::Usage,
            "error" => Self::Error,
            "lifecycle" => Self::Lifecycle,
            _ => return None,
        })
    }
}

impl fmt::Display for DiagnosticSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::All => "all",
            Self::Ingress => "ingress",
            Self::Prompt => "prompt",
            Self::Request => "request",
            Self::Response => "response",
            Self::Usage => "usage",
            Self::Error => "error",
            Self::Lifecycle => "lifecycle",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticErrorSummary {
    pub request_id: Uuid,
    pub character: CharacterId,
    pub source: RequestSource,
    pub stage: FailureStage,
    pub malformed: bool,
    pub message: BoundedText,
    pub partial_text: Option<BoundedText>,
    pub at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestTraceSummary {
    pub request_id: Uuid,
    pub character: CharacterId,
    pub source: RequestSource,
    pub intent: TurnIntent,
    pub complete: bool,
    pub outcome: Option<&'static str>,
    pub started_at_unix_ms: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSelection {
    Exact,
    Latest,
}

#[derive(Debug, Clone)]
pub struct DiagnosticRecord {
    pub trace: Arc<RequestTrace>,
    pub selection: DiagnosticSelection,
}

impl DiagnosticRecord {
    pub fn export(&self, section: DiagnosticSection) -> Result<String, serde_json::Error> {
        self.trace.section_json(section)
    }

    pub fn copy_text(&self, section: DiagnosticSection) -> Result<String, serde_json::Error> {
        self.export(section)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticScope {
    character: CharacterId,
    source: Option<RequestSource>,
}

impl DiagnosticScope {
    pub fn character(character: CharacterId) -> Self {
        Self {
            character,
            source: None,
        }
    }

    pub fn owner(character: CharacterId, source: RequestSource) -> Self {
        Self {
            character,
            source: Some(source),
        }
    }

    pub fn character_id(&self) -> CharacterId {
        self.character
    }

    pub fn source(&self) -> Option<RequestSource> {
        self.source
    }

    pub(crate) fn allows(&self, character: CharacterId, source: RequestSource) -> bool {
        character == self.character && self.source.is_none_or(|allowed| allowed == source)
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DiagnosticLookupError {
    #[error("diagnostics are unavailable")]
    Unavailable,
    #[error("diagnostic trace {0} was not found")]
    NotFound(Uuid),
    #[error("diagnostic trace {0} expired from the bounded retention window")]
    Expired(Uuid),
    #[error("diagnostic trace {0} belongs to another owner")]
    OwnershipDenied(Uuid),
    #[error("no diagnostic trace matches this scope")]
    Empty,
    #[error("diagnostics are stopped")]
    Stopped,
}

pub(crate) struct DiagnosticsStore {
    traces: VecDeque<Arc<RequestTrace>>,
    errors: VecDeque<DiagnosticErrorSummary>,
    expired: VecDeque<ExpiredTrace>,
    limits: DiagnosticsLimits,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExpiredTrace {
    pub id: RequestId,
    pub character: CharacterId,
    pub source: RequestSource,
}

impl DiagnosticsStore {
    pub(crate) fn new(limits: DiagnosticsLimits) -> Self {
        Self {
            traces: VecDeque::new(),
            errors: VecDeque::new(),
            expired: VecDeque::new(),
            limits,
        }
    }

    pub(crate) fn limits(&self) -> DiagnosticsLimits {
        self.limits
    }

    pub(crate) fn complete(&mut self, trace: Arc<RequestTrace>) {
        if let Some(error) = trace.error_history_entry() {
            self.errors.push_back(error);
            while self.errors.len() > self.limits.retained_errors.max(1) {
                self.errors.pop_front();
            }
        }
        self.traces.push_back(trace);
        while self.traces.len() > self.limits.retained_traces.max(1) {
            if let Some(evicted) = self.traces.pop_front() {
                self.expired.push_back(ExpiredTrace {
                    id: evicted.metadata.id,
                    character: evicted.metadata.character_id,
                    source: evicted.metadata.source,
                });
                while self.expired.len() > self.limits.retained_expired.max(1) {
                    self.expired.pop_front();
                }
            }
        }
    }

    pub(crate) fn find(&self, id: RequestId) -> DiagnosticsLookup {
        if let Some(trace) = self.traces.iter().find(|trace| trace.metadata.id == id) {
            return DiagnosticsLookup::Trace(Arc::clone(trace));
        }
        if let Some(expired) = self.expired.iter().find(|trace| trace.id == id) {
            return DiagnosticsLookup::Expired {
                character: expired.character,
                source: expired.source,
            };
        }
        DiagnosticsLookup::Missing
    }

    pub(crate) fn latest(&self, scope: DiagnosticScope) -> Option<Arc<RequestTrace>> {
        self.traces
            .iter()
            .rev()
            .find(|trace| scope.allows(trace.metadata.character_id, trace.metadata.source))
            .cloned()
    }

    pub(crate) fn recent(&self, scope: DiagnosticScope, limit: usize) -> Vec<RequestTraceSummary> {
        self.traces
            .iter()
            .rev()
            .filter(|trace| scope.allows(trace.metadata.character_id, trace.metadata.source))
            .take(limit.max(1))
            .map(|trace| trace.summary())
            .collect()
    }

    pub(crate) fn errors(
        &self,
        scope: DiagnosticScope,
        limit: usize,
    ) -> Vec<DiagnosticErrorSummary> {
        self.errors
            .iter()
            .rev()
            .filter(|error| scope.allows(error.character, error.source))
            .take(limit.max(1))
            .cloned()
            .collect()
    }
}

pub(crate) enum DiagnosticsLookup {
    Trace(Arc<RequestTrace>),
    Expired {
        character: CharacterId,
        source: RequestSource,
    },
    Missing,
}

#[derive(Clone)]
pub struct DiagnosticsAccess {
    inner: Weak<super::registry::RegistryInner>,
    scope: DiagnosticScope,
}

impl DiagnosticsAccess {
    pub(crate) fn new(inner: Weak<super::registry::RegistryInner>, scope: DiagnosticScope) -> Self {
        Self { inner, scope }
    }

    pub fn scope(&self) -> DiagnosticScope {
        self.scope
    }

    pub fn limits(&self) -> Result<DiagnosticsLimits, DiagnosticLookupError> {
        self.inner
            .upgrade()
            .map(|inner| inner.diagnostics_limits())
            .ok_or(DiagnosticLookupError::Unavailable)
    }

    pub fn request(&self, id: RequestId) -> Result<DiagnosticRecord, DiagnosticLookupError> {
        let inner = self
            .inner
            .upgrade()
            .ok_or(DiagnosticLookupError::Unavailable)?;
        match inner.diagnostic(id) {
            DiagnosticsLookup::Trace(trace) => {
                if !self
                    .scope
                    .allows(trace.metadata.character_id, trace.metadata.source)
                {
                    return Err(DiagnosticLookupError::OwnershipDenied(id.0));
                }
                Ok(DiagnosticRecord {
                    trace,
                    selection: DiagnosticSelection::Exact,
                })
            }
            DiagnosticsLookup::Expired { character, source } => {
                if !self.scope.allows(character, source) {
                    return Err(DiagnosticLookupError::OwnershipDenied(id.0));
                }
                Err(DiagnosticLookupError::Expired(id.0))
            }
            DiagnosticsLookup::Missing => Err(DiagnosticLookupError::NotFound(id.0)),
        }
    }

    pub fn latest(&self) -> Result<DiagnosticRecord, DiagnosticLookupError> {
        let inner = self
            .inner
            .upgrade()
            .ok_or(DiagnosticLookupError::Unavailable)?;
        if let Some(trace) = inner.diagnostic_latest(self.scope) {
            return Ok(DiagnosticRecord {
                trace,
                selection: DiagnosticSelection::Latest,
            });
        }
        Err(DiagnosticLookupError::Empty)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<RequestTraceSummary>, DiagnosticLookupError> {
        self.inner
            .upgrade()
            .map(|inner| inner.diagnostic_recent(self.scope, limit))
            .ok_or(DiagnosticLookupError::Unavailable)
    }

    pub fn errors(
        &self,
        limit: usize,
    ) -> Result<Vec<DiagnosticErrorSummary>, DiagnosticLookupError> {
        self.inner
            .upgrade()
            .map(|inner| inner.diagnostic_errors(self.scope, limit))
            .ok_or(DiagnosticLookupError::Unavailable)
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(character: CharacterId) -> RequestMetadata {
        RequestMetadata {
            id: RequestId::new(),
            character_id: character,
            source: RequestSource::Presenter,
            intent: TurnIntent::Initiate,
        }
    }

    #[test]
    fn credentials_encoded_in_urls_and_parameters_are_redacted() {
        let mut redactor = DiagnosticRedactor::default();
        redactor.configure(["s3cr+et&key".to_owned()]);
        let text = "https://user:pass@example.test/v1?key=s3cr%2Bet%26key&x=1#frag key=s3cr%2Bet%26key s3cr+et&key";
        let redacted = redactor.redact(text);
        assert!(!redacted.contains("s3cr+et&key"));
        assert!(!redacted.contains("s3cr%2Bet%26key"));
        assert!(!redacted.contains("user:pass"));
        assert!(!redacted.contains("x=1"), "{redacted}");
        assert!(redacted.contains("[REDACTED]"));
        assert!(redactor.redactions.credential_occurrences > 0);
        assert!(redactor.redactions.auth_url_occurrences > 0);
    }

    #[test]
    fn mixed_parameter_order_and_fragments_are_redacted_left_to_right() {
        let mut redactor = DiagnosticRedactor::default();
        let text = "token=alpha api_key=bravo key=charlie https://host/#secret-fragment";
        let redacted = redactor.redact(text);
        for secret in ["alpha", "bravo", "charlie", "secret-fragment"] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert_eq!(redacted.matches("[REDACTED]").count(), 4);
        assert!(redactor.redactions.credential_occurrences >= 3);
        assert!(redactor.redactions.auth_url_occurrences >= 1);
    }

    #[test]
    fn parameter_scan_is_linear_for_large_bodies() {
        let mut redactor = DiagnosticRedactor::default();
        let mut body = "token=first ".to_owned();
        body.push_str(&"ordinary text ".repeat(32_000));
        body.push_str("api_key=last");
        let redacted = redactor.redact(&body);
        assert!(!redacted.contains("first"));
        assert!(!redacted.contains("last"));
        assert_eq!(redactor.redactions.credential_occurrences, 2);
    }

    #[test]
    fn late_redaction_configuration_rewrites_ingress_and_preset() {
        let mut trace = RequestTrace::new(
            metadata(CharacterId::Crazy),
            "raw s3cr3t value",
            DiagnosticsLimits::default(),
        );
        trace.set_preset_description("preset raw s3cr3t value".into());
        assert!(trace.guidance.contains("s3cr3t"));
        trace.configure_redaction(["s3cr3t".to_owned()]);
        assert!(!trace.guidance.contains("s3cr3t"));
        assert!(!trace.input.text.contains("s3cr3t"));
        assert!(!trace.preset_description.contains("s3cr3t"));
    }

    #[test]
    fn text_limits_apply_to_ingress_output_and_preset_description() {
        let limits = DiagnosticsLimits {
            max_text_bytes: 8,
            ..DiagnosticsLimits::default()
        };
        let mut trace = RequestTrace::new(
            metadata(CharacterId::Crazy),
            "x".repeat(100).as_str(),
            limits,
        );
        trace.set_preset_description("p".repeat(100));
        trace.text_ready(&"y".repeat(100));
        assert!(trace.input.truncated);
        assert!(trace.guidance.len() <= 8);
        assert!(trace.preset_description.len() <= 8);
        assert!(trace.generated_text.as_ref().unwrap().len() <= 8);
    }

    #[test]
    fn quoted_spaced_bearer_and_escaped_credentials_are_fully_redacted() {
        let mut redactor = DiagnosticRedactor::default();
        redactor.configure(["s3cr\"et".to_owned()]);
        let text = concat!(
            "Authorization: Bearer unknown-token\n",
            "{\"password\":\"two words\"} ",
            "{\"password\":\"pa\\\"ss word\"} ",
            "body s3cr\\\"et"
        );
        let redacted = redactor.redact(text);
        for secret in ["unknown-token", "two", "words", "ss word", "s3cr"] {
            assert!(!redacted.contains(secret), "{secret} leaked: {redacted}");
        }
        assert!(redactor.redactions.credential_occurrences >= 3);
    }

    #[test]
    fn trace_debug_never_contains_configured_credentials() {
        let mut trace = RequestTrace::new(
            metadata(CharacterId::Crazy),
            "ingress",
            DiagnosticsLimits::default(),
        );
        trace.configure_redaction(["debug-secret-value".to_owned()]);
        assert!(!format!("{trace:?}").contains("debug-secret-value"));
        assert!(format!("{trace:?}").contains("configured_secrets"));
    }

    #[test]
    fn successful_transitions_are_summaries_without_text_or_audio() {
        use crate::{
            character::{AcceptedResponse, CharacterState},
            identity::EventId,
            timeline::ContextAnchor,
            voice::{AudioArtifact, AudioFormat},
        };

        let mut trace = RequestTrace::new(
            metadata(CharacterId::Crazy),
            "ingress",
            DiagnosticsLimits::default(),
        );
        trace.configure_redaction(["credential-value".to_owned()]);
        trace.text_ready("credential-value response");
        let audio_bytes: Arc<[u8]> = vec![91u8; 16].into();
        let response = AcceptedResponse {
            game_invite: None,
            text: Arc::from("credential-value response"),
            segments: Vec::new().into(),
            effects: Vec::new().into(),
            state_after: CharacterState {
                accepted_replies: 1,
                revision: 1,
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
            event_id: EventId::new(),
            context: ContextAnchor {
                branch_id: Uuid::new_v4(),
                through: 1,
            },
            audio: Some(AudioArtifact {
                format: AudioFormat::Wave,
                bytes: Arc::clone(&audio_bytes),
            }),
        };
        trace.push_transition(
            1,
            EffectPhase::Applied,
            &RequestState::Finished(RequestOutcome::Success(response)),
        );
        assert_eq!(
            trace.transitions.last().unwrap().state,
            DiagnosticState::Finished {
                outcome: "success".into()
            }
        );
        let debug = format!("{trace:?}");
        assert!(!debug.contains("credential-value"));
        assert_eq!(Arc::strong_count(&audio_bytes), 1);
        assert!(trace.transitions.len() <= trace.limits.max_transitions);
    }

    #[test]
    fn missing_artifacts_are_explicit_after_cancellation() {
        let mut trace = RequestTrace::new(
            metadata(CharacterId::Crazy),
            "hello",
            DiagnosticsLimits::default(),
        );
        trace.mark_started();
        trace.cancelled();
        trace.finished();
        let view = trace.view();
        assert!(view.prompt.is_none());
        assert!(
            view.missing
                .iter()
                .any(|entry| entry.starts_with("provider_request"))
        );
        assert!(view.missing.iter().any(|entry| entry.contains("cancelled")));
    }

    #[test]
    fn retention_distinguishes_expired_from_never_seen() {
        let mut store = DiagnosticsStore::new(DiagnosticsLimits::default().with_retention(1, 1));
        let mut first = RequestTrace::new(metadata(CharacterId::Crazy), "one", store.limits());
        first.cancelled();
        first.finished();
        let first_id = first.metadata.id;
        let mut second = RequestTrace::new(metadata(CharacterId::Crazy), "two", store.limits());
        second.cancelled();
        second.finished();
        let second_id = second.metadata.id;
        store.complete(Arc::new(first));
        store.complete(Arc::new(second));
        assert!(matches!(
            store.find(first_id),
            DiagnosticsLookup::Expired { .. }
        ));
        assert!(matches!(store.find(second_id), DiagnosticsLookup::Trace(_)));
        assert!(matches!(
            store.find(RequestId::new()),
            DiagnosticsLookup::Missing
        ));
    }
}
