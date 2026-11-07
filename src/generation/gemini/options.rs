use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeminiRequestOptions {
    pub system_instruction: SystemInstructionMode,
    pub send_temperature: bool,
    pub send_max_output_tokens: bool,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub thinking: Option<GeminiThinking>,
    pub structured_output: bool,
}

impl Default for GeminiRequestOptions {
    fn default() -> Self {
        Self {
            system_instruction: SystemInstructionMode::Native,
            send_temperature: true,
            send_max_output_tokens: true,
            top_p: None,
            top_k: None,
            thinking: None,
            structured_output: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SystemInstructionMode {
    Native,
    UserPrefix { tag: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum GeminiThinking {
    Level {
        level: ThinkingLevel,
        include_thoughts: Option<bool>,
    },
    Budget {
        tokens: i32,
        include_thoughts: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GeminiRequestOptions {
    pub(crate) fn validate(&self) -> Result<(), super::PresetError> {
        if self
            .top_p
            .is_some_and(|p| !p.is_finite() || !(0.0..=1.0).contains(&p))
            || matches!(self.thinking, Some(GeminiThinking::Budget { tokens, .. }) if tokens < -1)
        {
            return Err(super::PresetError::Options);
        }
        Ok(())
    }

    pub(crate) fn thinking_json(&self) -> Option<serde_json::Value> {
        self.thinking.as_ref().map(|thinking| {
            let (mut value, include) = match thinking {
                GeminiThinking::Level {
                    level,
                    include_thoughts,
                } => (
                    serde_json::json!({"thinkingLevel": level}),
                    include_thoughts,
                ),
                GeminiThinking::Budget {
                    tokens,
                    include_thoughts,
                } => (
                    serde_json::json!({"thinkingBudget": tokens}),
                    include_thoughts,
                ),
            };
            if let Some(include) = include {
                value["includeThoughts"] = (*include).into();
            }
            value
        })
    }
}
