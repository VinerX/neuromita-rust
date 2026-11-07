//! Typed ASR client over the application-owned Python inference worker.

use std::sync::Arc;

use base64::Engine;
use serde_json::{Value, json};
use thiserror::Error;

use super::{AiRuntimeError, AiService, AiSupervisor};

const MAX_WAV_BYTES: usize = 44 + 600 * 16_000 * 2;

#[derive(Debug, Error)]
pub enum SpeechRecognitionError {
    #[error("invalid PCM audio segment: {0}")]
    InvalidAudio(String),
    #[error("invalid ASR response: {0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Runtime(#[from] AiRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

pub struct PythonSpeechRecognizer {
    supervisor: Arc<AiSupervisor>,
}

impl PythonSpeechRecognizer {
    pub fn new(supervisor: Arc<AiSupervisor>) -> Self {
        Self { supervisor }
    }

    pub async fn transcribe_wav(&self, wav: &[u8]) -> Result<Transcript, SpeechRecognitionError> {
        if wav.len() < 44
            || wav.len() > MAX_WAV_BYTES
            || &wav[..4] != b"RIFF"
            || &wav[8..12] != b"WAVE"
        {
            return Err(SpeechRecognitionError::InvalidAudio(
                "expected a bounded RIFF/WAVE segment".into(),
            ));
        }
        let result = self
            .supervisor
            .call(
                AiService::Asr,
                "infer",
                json!({
                    "kind": "asr",
                    "input": {"audio_base64": base64::engine::general_purpose::STANDARD.encode(wav)},
                }),
            )
            .await?;
        let text = result.get("text").and_then(Value::as_str).ok_or_else(|| {
            SpeechRecognitionError::InvalidResponse("missing transcript text".into())
        })?;
        if text.len() > 128 * 1024 || text.contains('\0') {
            return Err(SpeechRecognitionError::InvalidResponse(
                "transcript exceeds text bounds or contains NUL".into(),
            ));
        }
        let language = result
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if language.as_ref().is_some_and(|value| value.len() > 64) {
            return Err(SpeechRecognitionError::InvalidResponse(
                "invalid transcript language".into(),
            ));
        }
        Ok(Transcript {
            text: text.to_owned(),
            language,
        })
    }
}
