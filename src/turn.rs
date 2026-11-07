use crate::{
    character::state::{CharacterStateDelta, CharacterWorkingState},
    games::ModelGameAction,
    memory::MemoryCommand,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

/// A typed change proposed by a validated model response. No world mutation
/// occurs while these values are being assembled.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEffect {
    ChangeCharacterState(CharacterStateDelta),
    ReplaceWorkingState(CharacterWorkingState),
    MutateMemory(Vec<MemoryCommand>),
    /// Validated model-authored game commands, applied atomically with the reply.
    Game(Vec<ModelGameAction>),
    MutateReminders(Vec<crate::reminders::ReminderRequest>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SegmentIntent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "object_or_json_string")]
    pub payload: Map<String, Value>,
}

pub(crate) fn object_or_json_string<'de, D>(deserializer: D) -> Result<Map<String, Value>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Object(fields) => Ok(fields),
        Value::String(raw) => {
            serde_json::from_str::<Map<String, Value>>(&raw).map_err(serde::de::Error::custom)
        }
        _ => Err(serde::de::Error::custom(
            "expected JSON object or encoded object",
        )),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResponseSegment {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emotions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub idle_animations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub movement_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visual_effects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clothes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub music: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub face_params: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<SegmentIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_game: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_game: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_sleep: Option<bool>,
}

impl ResponseSegment {
    pub fn spoken(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Game commands are the only executable segment fields the core accepts;
    /// everything else still has no executor.
    pub(crate) fn has_unsupported_non_game_content(&self) -> bool {
        !self.animations.is_empty()
            || !self.idle_animations.is_empty()
            || !self.movement_modes.is_empty()
            || !self.visual_effects.is_empty()
            || !self.clothes.is_empty()
            || !self.music.is_empty()
            || !self.interactions.is_empty()
            || !self.face_params.is_empty()
            || !self.intents.is_empty()
            || self.target.is_some()
            || self.hint.is_some()
            || self.allow_sleep.is_some()
    }

    /// Remove executable game fields before the segment is persisted as a
    /// dialogue message. Placement commands would otherwise disclose a fleet
    /// to anyone who can read the accepted message.
    pub(crate) fn sanitized_for_persistence(&self) -> Self {
        let mut segment = self.clone();
        segment.commands.clear();
        segment.start_game = None;
        segment.end_game = None;
        segment
    }
}

#[derive(Debug, Clone)]
pub struct TurnProposal {
    text: Arc<str>,
    segments: Arc<[ResponseSegment]>,
    effects: Arc<[ResponseEffect]>,
}

impl TurnProposal {
    pub(crate) fn validated(
        text: Arc<str>,
        segments: Vec<ResponseSegment>,
        effects: Vec<ResponseEffect>,
    ) -> Self {
        Self {
            text,
            segments: segments.into(),
            effects: effects.into(),
        }
    }
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }
    pub fn segments(&self) -> &[ResponseSegment] {
        &self.segments
    }
    pub fn effects(&self) -> &[ResponseEffect] {
        &self.effects
    }
}
