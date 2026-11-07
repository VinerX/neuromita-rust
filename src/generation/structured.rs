use std::sync::Arc;

use crate::reminders::{MAX_TEXT_BYTES, ReminderRequest, ReminderToolCommand, parse_repeat};
use crate::turn::object_or_json_string;
pub use crate::turn::{ResponseSegment, SegmentIntent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{GenerationError, ResponseEffect};
use crate::games::ModelGameAction;
pub use crate::timeline::domain::CharacterWorkingState as WorkingState;
use crate::timeline::domain::{CharacterStateDelta, MemoryCommand, MemoryId, MemoryPriority};
use uuid::Uuid;

pub const RESPONSE_PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    #[serde(default, deserialize_with = "object_or_json_string")]
    pub args: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StructuredResponse {
    pub reasoning: Option<String>,
    pub image_description: Option<String>,
    pub segments: Vec<ResponseSegment>,
    pub secret_exposed: Option<bool>,
    pub attitude_change: f64,
    pub boredom_change: f64,
    pub stress_change: f64,
    pub memory_add: Option<Vec<String>>,
    pub memory_update: Option<Vec<String>>,
    pub memory_delete: Option<Vec<String>>,
    pub memory_merge: Option<Vec<String>>,
    pub reminder_add: Option<Vec<String>>,
    pub reminder_delete: Option<Vec<String>>,
    pub timer_add: Option<Vec<String>>,
    pub entities: Option<Vec<String>>,
    pub relations: Option<Vec<String>>,
    pub tool_call: Option<ToolCall>,
    pub custom_fields: Option<Map<String, Value>>,
    pub working_state: Option<WorkingState>,
}

impl StructuredResponse {
    pub fn spoken(text: impl Into<String>) -> Self {
        Self {
            segments: vec![ResponseSegment::spoken(text)],
            ..Self::default()
        }
    }

    pub fn parse(raw: &str) -> Result<Self, GenerationError> {
        let result: Self = serde_json::from_str(raw).map_err(|error| {
            GenerationError::Response(format!("invalid structured response v3: {error}"))
        })?;
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), GenerationError> {
        if self.segments.is_empty() || self.full_text().trim().is_empty() {
            return Err(GenerationError::Response(
                "structured response has no spoken segments".into(),
            ));
        }
        if self.segments.iter().any(|segment| {
            segment
                .intents
                .iter()
                .any(|intent| intent.kind.trim().is_empty())
        }) {
            return Err(GenerationError::Response(
                "structured intent has empty type".into(),
            ));
        }
        if self
            .working_state
            .as_ref()
            .is_some_and(|state| !state.is_valid())
        {
            return Err(GenerationError::Response(
                "working state exceeds its bounded handoff budget".into(),
            ));
        }
        for delta in [
            self.attitude_change,
            self.boredom_change,
            self.stress_change,
        ] {
            if !delta.is_finite() {
                return Err(GenerationError::Response(
                    "nonfinite character state delta".into(),
                ));
            }
        }
        self.memory_commands()?;
        Ok(())
    }

    pub(crate) fn reminder_requests(&self) -> Result<Vec<ReminderRequest>, GenerationError> {
        let invalid = |reason: &str| {
            GenerationError::Response(format!("invalid reminder operation: {reason}"))
        };
        let mut requests = Vec::new();
        for raw in self.reminder_add.iter().flatten() {
            let (due, text) = raw
                .split_once('|')
                .ok_or_else(|| invalid("reminder_add requires due_iso|text"))?;
            let due = due.trim();
            let text = text.trim();
            if due.is_empty() || text.is_empty() || text.len() > MAX_TEXT_BYTES {
                return Err(invalid(
                    "reminder_add requires nonempty due and bounded text",
                ));
            }
            requests.push(ReminderRequest::StructuredAdd {
                due_iso: due.to_owned(),
                text: text.to_owned(),
            });
        }
        for raw in self.timer_add.iter().flatten() {
            let (delay, instruction) = raw
                .split_once('|')
                .ok_or_else(|| invalid("timer_add requires delay_seconds|instruction"))?;
            let delay = delay
                .trim()
                .parse::<f64>()
                .map_err(|_| invalid("timer_add delay is not a number"))?;
            let instruction = instruction.trim();
            if !delay.is_finite() || delay <= 0.0 || instruction.is_empty() {
                return Err(invalid("timer_add requires positive delay and instruction"));
            }
            if instruction.len() > MAX_TEXT_BYTES {
                return Err(invalid("timer_add instruction is too large"));
            }
            requests.push(ReminderRequest::StructuredTimer {
                delay_seconds: delay,
                instruction: instruction.to_owned(),
            });
        }
        for raw in self.reminder_delete.iter().flatten() {
            let number = raw
                .trim()
                .parse::<u64>()
                .map_err(|_| invalid("reminder_delete requires a positive numeric N"))?;
            if number == 0 {
                return Err(invalid("reminder_delete requires a positive numeric N"));
            }
            requests.push(ReminderRequest::StructuredDelete { number });
        }
        if let Some(tool) = &self.tool_call {
            if tool.name.trim() != "reminder" {
                return Err(invalid("only the reminder tool is supported"));
            }
            requests.push(ReminderRequest::Tool(parse_reminder_tool_args(&tool.args)?));
        }
        if requests.len() > 32 {
            return Err(invalid("too many reminder operations in one response"));
        }
        Ok(requests)
    }

    fn memory_commands(&self) -> Result<Vec<MemoryCommand>, GenerationError> {
        let invalid =
            |reason: &str| GenerationError::Response(format!("invalid memory operation: {reason}"));
        let total = [
            &self.memory_add,
            &self.memory_update,
            &self.memory_delete,
            &self.memory_merge,
        ]
        .iter()
        .map(|items| items.as_ref().map_or(0, Vec::len))
        .sum::<usize>();
        if total > 16 {
            return Err(invalid("too many operations in one response"));
        }
        let parse_id = |text: &str| -> Result<MemoryId, GenerationError> {
            Uuid::parse_str(text.trim())
                .map(MemoryId)
                .map_err(|_| invalid("expected stable UUID identity"))
        };
        let priority = |text: &str| -> Option<MemoryPriority> {
            match text.trim().to_ascii_lowercase().as_str() {
                "low" => Some(MemoryPriority::Low),
                "normal" => Some(MemoryPriority::Normal),
                "high" => Some(MemoryPriority::High),
                "critical" => Some(MemoryPriority::Critical),
                _ => None,
            }
        };
        let validate_text = |text: &str| -> Result<String, GenerationError> {
            let content = text.trim();
            if content.is_empty() || content.chars().count() > 2000 {
                return Err(invalid("content must contain 1-2000 characters"));
            }
            Ok(content.to_owned())
        };
        let mut commands = Vec::new();
        for raw in self.memory_add.iter().flatten() {
            let (priority, content) = raw
                .split_once('|')
                .and_then(|(tag, text)| priority(tag).map(|p| (p, text)))
                .unwrap_or((MemoryPriority::Normal, raw.as_str()));
            if raw.trim().starts_with("island:") {
                return Err(invalid("island upsert not implemented"));
            }
            commands.push(MemoryCommand::Add {
                priority,
                content: validate_text(content)?,
            });
        }
        for raw in self.memory_update.iter().flatten() {
            let (id, remainder) = raw
                .split_once('|')
                .ok_or_else(|| invalid("update requires UUID|content"))?;
            let id = parse_id(id)?;
            let (priority, content) = remainder
                .split_once('|')
                .and_then(|(tag, text)| priority(tag).map(|p| (Some(p), text)))
                .unwrap_or((None, remainder));
            commands.push(MemoryCommand::Update {
                id,
                priority,
                content: validate_text(content)?,
            });
        }
        for raw in self.memory_delete.iter().flatten() {
            commands.push(MemoryCommand::SoftDelete { id: parse_id(raw)? });
        }
        for raw in self.memory_merge.iter().flatten() {
            let (ids, content) = raw
                .split_once(':')
                .map_or((raw.as_str(), None), |(ids, text)| (ids, Some(text)));
            let mut parts = ids.split(',');
            let target = parse_id(parts.next().unwrap_or(""))?;
            let sources = parts.map(parse_id).collect::<Result<Vec<_>, _>>()?;
            if sources.is_empty() {
                return Err(invalid("merge requires at least two IDs"));
            }
            commands.push(MemoryCommand::Merge {
                target,
                sources,
                content: content.map(validate_text).transpose()?,
            });
        }
        Ok(commands)
    }

    pub fn proposed_effects(&self) -> Result<Vec<ResponseEffect>, GenerationError> {
        let change = CharacterStateDelta {
            attitude: self.attitude_change,
            boredom: self.boredom_change,
            stress: self.stress_change,
        };
        let mut effects = Vec::new();
        if !change.is_empty() {
            effects.push(ResponseEffect::ChangeCharacterState(change));
        }
        if let Some(state) = &self.working_state {
            effects.push(ResponseEffect::ReplaceWorkingState(state.clone()));
        }
        let memory = self.memory_commands()?;
        if !memory.is_empty() {
            effects.push(ResponseEffect::MutateMemory(memory));
        }
        let game = self.game_actions()?;
        if !game.is_empty() {
            effects.push(ResponseEffect::Game(game));
        }
        let reminders = self.reminder_requests()?;
        if !reminders.is_empty() {
            effects.push(ResponseEffect::MutateReminders(reminders));
        }
        Ok(effects)
    }

    /// Parse the executable game subset of a response. The order is the order
    /// in which the owner will validate and apply the actions.
    pub fn game_actions(&self) -> Result<Vec<ModelGameAction>, GenerationError> {
        let mut actions = Vec::new();
        for segment in &self.segments {
            if let Some(start) = &segment.start_game {
                actions.push(ModelGameAction::parse_start(start).ok_or_else(|| {
                    GenerationError::Response("unsupported start_game identifier".into())
                })?);
            }
            if let Some(end) = &segment.end_game {
                actions.push(ModelGameAction::parse_end(end).ok_or_else(|| {
                    GenerationError::Response("unsupported end_game identifier".into())
                })?);
            }
            for command in &segment.commands {
                actions.push(ModelGameAction::parse_command(command).ok_or_else(|| {
                    GenerationError::Response(format!(
                        "unsupported structured command: {}",
                        command.chars().take(64).collect::<String>()
                    ))
                })?);
            }
        }
        if actions.len() > 24 {
            return Err(GenerationError::Response(
                "too many game commands in one response".into(),
            ));
        }
        Ok(actions)
    }

    pub fn full_text(&self) -> Arc<str> {
        Arc::from(
            self.segments
                .iter()
                .map(|segment| segment.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    pub fn ensure_supported_effects(&self) -> Result<(), GenerationError> {
        if self
            .segments
            .iter()
            .any(ResponseSegment::has_unsupported_non_game_content)
            || self.image_description.is_some()
            || self.secret_exposed.is_some()
            || self.entities.is_some()
            || self.relations.is_some()
            || self.custom_fields.is_some()
        {
            return Err(GenerationError::Response(
                "structured response contains fields without an enabled executor".into(),
            ));
        }
        self.game_actions()?;
        self.reminder_requests()?;
        Ok(())
    }
}

fn parse_reminder_tool_args(
    args: &Map<String, Value>,
) -> Result<ReminderToolCommand, GenerationError> {
    let invalid =
        |reason: &str| GenerationError::Response(format!("invalid reminder tool call: {reason}"));
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("action is required"))?
        .trim()
        .to_ascii_lowercase();
    let string = |key: &str| -> Result<Option<String>, GenerationError> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(invalid(&format!("{key} must be a string"))),
        }
    };
    let number = |key: &str| -> Result<Option<u64>, GenerationError> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(value)) => value
                .as_u64()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or_else(|| invalid(&format!("{key} must be a positive integer"))),
            Some(_) => Err(invalid(&format!("{key} must be a positive integer"))),
        }
    };
    match action.as_str() {
        "list" => Ok(ReminderToolCommand::List),
        "add" => {
            let text = string("text")?.ok_or_else(|| invalid("text is required"))?;
            let due = string("due")?.ok_or_else(|| invalid("due is required"))?;
            let repeat = string("repeat")?
                .map(|value| parse_repeat(&value))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?;
            Ok(ReminderToolCommand::Add { text, due, repeat })
        }
        "timer" => {
            let instruction =
                string("instruction")?.ok_or_else(|| invalid("instruction is required"))?;
            let delay = args
                .get("delay_seconds")
                .and_then(Value::as_f64)
                .ok_or_else(|| invalid("delay_seconds is required"))?;
            if !delay.is_finite() || delay <= 0.0 {
                return Err(invalid("delay_seconds must be a positive finite number"));
            }
            Ok(ReminderToolCommand::Timer {
                instruction,
                delay_seconds: delay,
            })
        }
        "delete" => Ok(ReminderToolCommand::Delete {
            number: number("n")?.ok_or_else(|| invalid("n is required"))?,
        }),
        "update" => {
            let number = number("n")?.ok_or_else(|| invalid("n is required"))?;
            let text = string("text")?;
            let due = string("due")?;
            let repeat = string("repeat")?
                .map(|value| parse_repeat(&value))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?;
            if text.is_none() && due.is_none() && repeat.is_none() {
                return Err(invalid("update requires text, due or repeat"));
            }
            Ok(ReminderToolCommand::Update {
                number,
                text,
                due,
                repeat,
            })
        }
        other => Err(invalid(&format!("unknown action '{other}'"))),
    }
}

pub(crate) fn gemini_message_schema() -> Value {
    serde_json::from_str(include_str!("structured_message_gemini.json"))
        .expect("checked-in Gemini schema derived from Python v3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_python_v3_text_and_preserve_order() {
        let parsed =
            StructuredResponse::parse(r#"{"segments":[{"text":"Привет"},{"text":"мир"}]}"#)
                .unwrap();
        assert_eq!(parsed.full_text().as_ref(), "Привет мир");
        assert_eq!(RESPONSE_PROTOCOL_VERSION, 3);
        parsed.ensure_supported_effects().unwrap();
    }

    #[test]
    fn structured_fields_roundtrip_without_executing_actions() {
        let data = json!({
            "segments": [{"text":"hello", "intents":[{"type":"inventory.collect", "payload":"{\"object\":\"apple\"}"}],
                "emotions":["happy"], "target":"KindMita"}],
            "memory_add":["high|Player prefers tea"],
            "tool_call":{"name":"search", "args":"{\"query\":\"tea\"}"},
            "working_state":{"focus":"conversation"}
        });
        let parsed = StructuredResponse::parse(&data.to_string()).unwrap();
        assert_eq!(parsed.segments[0].intents[0].payload["object"], "apple");
        assert_eq!(parsed.tool_call.as_ref().unwrap().args["query"], "tea");
        assert!(parsed.ensure_supported_effects().is_err());
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["segments"][0]["target"],
            "KindMita"
        );
    }

    #[test]
    fn missing_segment_text_is_invalid_even_when_other_segments_are_valid() {
        assert!(StructuredResponse::parse(r#"{"segments":[{}, {"text":"valid"}]}"#).is_err());
    }

    #[test]
    fn rejects_malformed_or_unimplemented_effects() {
        for data in [
            r#"{"segments":[]}"#,
            r#"{"segments":[{"text":" "}]}"#,
            r#"{"segments":[{"text":"hi","unexpected_action":true}]}"#,
            r#"{"segments":[{"text":"hi"}],"tool_call":{"name":"search","args":"not-json"}}"#,
            "plain text",
        ] {
            assert!(StructuredResponse::parse(data).is_err(), "{data}");
        }
        let proposed =
            StructuredResponse::parse(r#"{"segments":[{"text":"hello", "commands":["run"]}]}"#)
                .unwrap();
        assert!(proposed.ensure_supported_effects().is_err());
    }

    #[test]
    fn state_changes_are_supported_but_other_side_effects_are_not() {
        let response = StructuredResponse::parse(
            r#"{"segments":[{"text":"hi"}],"attitude_change":1.25,"stress_change":-2.5}"#,
        )
        .unwrap();
        response.ensure_supported_effects().unwrap();
        assert!(matches!(response.proposed_effects().unwrap().as_slice(),
            [ResponseEffect::ChangeCharacterState(delta)] if delta.attitude == 1.25 && delta.stress == -2.5
        ));
        let unsupported = StructuredResponse::parse(
            r#"{"segments":[{"text":"hi"}],"attitude_change":1,"memory_add":["remember"]}"#,
        )
        .unwrap();
        unsupported.ensure_supported_effects().unwrap();
        assert!(matches!(
            unsupported.proposed_effects().unwrap().last(),
            Some(ResponseEffect::MutateMemory(_))
        ));
    }

    #[test]
    fn working_state_is_a_typed_replacement_not_a_textual_command() {
        let response = StructuredResponse::parse(
            r#"{"segments":[{"text":"hi"}],"working_state":{"focus":"Resolve the question","next_steps":["Ask a follow-up"]}}"#,
        ).unwrap();
        response.ensure_supported_effects().unwrap();
        assert!(
            matches!(response.proposed_effects().unwrap().as_slice(), [ResponseEffect::ReplaceWorkingState(state)] if state.focus == "Resolve the question")
        );
        let clear = StructuredResponse::parse(r#"{"segments":[{"text":"hi"}],"working_state":{}}"#)
            .unwrap();
        assert!(
            matches!(clear.proposed_effects().unwrap().as_slice(), [ResponseEffect::ReplaceWorkingState(state)] if state.is_empty())
        );
        assert!(
            StructuredResponse::parse(
                &serde_json::json!({
                    "segments":[{"text":"hi"}],
                    "working_state":{"focus":"z".repeat(601)}
                })
                .to_string()
            )
            .is_err()
        );
        StructuredResponse::parse(r#"{"segments":[{"text":"hi"}],"memory_add":["normal|later"]}"#)
            .unwrap()
            .ensure_supported_effects()
            .unwrap();
    }

    #[test]
    fn provider_schema_is_capability_limited() {
        let schema = gemini_message_schema();
        assert_eq!(
            schema["properties"]["segments"]["items"]["properties"]["emotions"]["type"],
            "array"
        );
        assert_eq!(schema["required"], json!(["segments"]));
        assert_eq!(
            schema["properties"]["segments"]["items"]["required"],
            json!(["text"])
        );
        let properties = schema["properties"].as_object().unwrap();
        for field in ["reminder_add", "reminder_delete", "timer_add", "tool_call"] {
            assert!(properties.contains_key(field), "{field} must be reachable");
        }
        assert_eq!(
            properties["tool_call"]["properties"]["args"]["properties"]["action"]["enum"],
            json!(["list", "add", "timer", "delete", "update"])
        );
        assert!(properties.get("entities").is_none());
        assert!(properties.get("relations").is_none());
        let segment = &schema["properties"]["segments"]["items"]["properties"];
        assert_eq!(segment["commands"]["type"], "array");
        assert_eq!(segment["start_game"]["type"], "string");
        assert_eq!(segment["end_game"]["type"], "string");
        assert!(segment.as_object().unwrap().get("animations").is_none());
    }

    #[test]
    fn accepted_segments_never_persist_executable_game_fields() {
        let parsed = StructuredResponse::parse(
            r#"{"segments":[{"text":"your move","start_game":"seabattle","commands":["PlaceShip,D5,4,H"]}]}"#,
        )
        .unwrap();
        let proposal = crate::generation::ProposedResponse::from_structured(parsed).unwrap();
        assert!(proposal.segments()[0].commands.is_empty());
        assert!(proposal.segments()[0].start_game.is_none());
        assert!(matches!(
            proposal.effects(),
            [ResponseEffect::Game(actions)] if actions.len() == 2
        ));
        let unknown =
            StructuredResponse::parse(r#"{"segments":[{"text":"hi","commands":["DeleteSave"]}]}"#)
                .unwrap();
        assert!(crate::generation::ProposedResponse::from_structured(unknown).is_err());
        let foreign_game =
            StructuredResponse::parse(r#"{"segments":[{"text":"hi","start_game":"chess"}]}"#)
                .unwrap();
        assert!(crate::generation::ProposedResponse::from_structured(foreign_game).is_err());
    }
}
