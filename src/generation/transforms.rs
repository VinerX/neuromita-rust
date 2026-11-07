use serde::{Deserialize, Serialize};

use super::{PromptMessage, PromptRole};

/// Ordered, provider-owned transformations of the text prompt. Timeline facts
/// and character history are never modified by this pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "id",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MessageTransform {
    MergeSystemMessages,
    EnsureAlternatingRoles,
    EnsureLastMessageUser { fallback_user_text: String },
    SystemToUserPrefix { tag: String },
    SystemMessagesToUser { tag: String },
    NormalizeSystemMessages { tag: String },
    TrailingSystemToUserPrefix { tag: String },
}

fn joined<'a>(texts: impl Iterator<Item = &'a str>) -> String {
    texts
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn prefix(tag: &str, text: &str) -> String {
    format!("{tag}\n{text}")
}

fn convert_system(messages: Vec<PromptMessage>, tag: &str) -> Vec<PromptMessage> {
    let leading = messages
        .iter()
        .take_while(|m| m.role == PromptRole::System)
        .count();
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(index, mut message)| {
            if message.role == PromptRole::System && message.text.trim().is_empty() {
                return None;
            }
            if index >= leading && message.role == PromptRole::System {
                message.role = PromptRole::User;
                message.text = prefix(tag, &message.text).into();
            }
            Some(message)
        })
        .collect()
}

pub fn apply_message_transforms(
    mut messages: Vec<PromptMessage>,
    transforms: &[MessageTransform],
) -> Vec<PromptMessage> {
    for transform in transforms {
        match transform {
            MessageTransform::MergeSystemMessages => {
                let leading = messages
                    .iter()
                    .take_while(|m| m.role == PromptRole::System)
                    .count();
                if leading > 0 {
                    let text = joined(messages[..leading].iter().map(|m| m.text.as_ref()));
                    if !text.is_empty() {
                        messages.drain(..leading);
                        messages.insert(
                            0,
                            PromptMessage {
                                role: PromptRole::System,
                                text: text.into(),
                            },
                        );
                    }
                }
            }
            MessageTransform::EnsureAlternatingRoles => {
                let mut merged: Vec<PromptMessage> = Vec::with_capacity(messages.len());
                for message in messages {
                    if let Some(last) = merged.last_mut().filter(|m| m.role == message.role) {
                        last.text = [last.text.as_ref(), message.text.as_ref()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                            .into();
                    } else {
                        merged.push(message);
                    }
                }
                messages = merged;
            }
            MessageTransform::EnsureLastMessageUser { fallback_user_text } => {
                if messages
                    .last()
                    .is_none_or(|m| m.role == PromptRole::Assistant)
                {
                    messages.push(PromptMessage {
                        role: PromptRole::User,
                        text: fallback_user_text.as_str().into(),
                    });
                }
            }
            MessageTransform::SystemToUserPrefix { tag } => {
                let context = messages
                    .iter()
                    .filter(|m| m.role == PromptRole::System)
                    .map(|m| m.text.trim())
                    .filter(|s| !s.is_empty())
                    .map(|text| format!("{tag} {text}"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.retain(|m| m.role != PromptRole::System);
                if !context.is_empty() {
                    let context = context.trim().to_owned();
                    if let Some(message) = messages.iter_mut().find(|m| m.role == PromptRole::User)
                    {
                        message.text = format!("{context}\n\n{}", message.text).into();
                    } else {
                        messages.push(PromptMessage {
                            role: PromptRole::User,
                            text: context.into(),
                        });
                    }
                }
            }
            MessageTransform::SystemMessagesToUser { tag } => {
                messages = convert_system(messages, tag)
            }
            MessageTransform::NormalizeSystemMessages { tag } => {
                messages = convert_system(messages, tag);
                let leading = messages
                    .iter()
                    .take_while(|m| m.role == PromptRole::System)
                    .count();
                if leading >= 2 {
                    let text = messages[..leading]
                        .iter()
                        .map(|m| m.text.as_ref())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    messages.drain(..leading);
                    messages.insert(
                        0,
                        PromptMessage {
                            role: PromptRole::System,
                            text: text.into(),
                        },
                    );
                }
            }
            MessageTransform::TrailingSystemToUserPrefix { tag } => {
                if let Some(mut message) = messages.pop_if(|m| m.role == PromptRole::System)
                    && !message.text.trim().is_empty()
                {
                    message.role = PromptRole::User;
                    message.text = format!("{tag} {}", message.text.trim()).into();
                    messages.push(message);
                }
            }
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agrees_with_python_reference_on_text_edge_cases() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/python-message-transforms.json"
        ))
        .unwrap();
        let parse_messages = |value: &serde_json::Value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|message| {
                    let role = match message["role"].as_str().unwrap() {
                        "system" => PromptRole::System,
                        "user" => PromptRole::User,
                        "assistant" => PromptRole::Assistant,
                        _ => panic!(),
                    };
                    PromptMessage {
                        role,
                        text: message["content"].as_str().unwrap().into(),
                    }
                })
                .collect::<Vec<_>>()
        };
        for case in cases.as_array().unwrap() {
            let transform: MessageTransform =
                serde_json::from_value(case["transform"].clone()).unwrap();
            assert_eq!(
                apply_message_transforms(parse_messages(&case["input"]), &[transform]),
                parse_messages(&case["expected"]),
                "{}",
                case["name"]
            );
        }
    }

    fn message(role: PromptRole, text: &str) -> PromptMessage {
        PromptMessage {
            role,
            text: text.into(),
        }
    }

    #[test]
    fn normalization_preserves_position_and_merges_only_leading_instructions() {
        use PromptRole::*;
        let input = vec![
            message(System, " first "),
            message(System, "second"),
            message(User, "hello"),
            message(Assistant, "answer"),
            message(System, "now"),
            message(User, "again"),
        ];
        let output = apply_message_transforms(
            input.clone(),
            &[MessageTransform::NormalizeSystemMessages {
                tag: "[SYSTEM INFO]".into(),
            }],
        );
        assert_eq!(
            output,
            vec![
                message(System, " first \n\nsecond"),
                message(User, "hello"),
                message(Assistant, "answer"),
                message(User, "[SYSTEM INFO]\nnow"),
                message(User, "again")
            ]
        );
        assert_eq!(input.len(), 6);
    }

    #[test]
    fn ordered_pipeline_merges_roles_then_appends_explicit_user_prompt() {
        use PromptRole::*;
        let output = apply_message_transforms(
            vec![
                message(System, "instructions"),
                message(User, "one"),
                message(User, "two"),
                message(Assistant, "reply"),
            ],
            &[
                MessageTransform::SystemToUserPrefix {
                    tag: "[SYSTEM CONTEXT]".into(),
                },
                MessageTransform::EnsureAlternatingRoles,
                MessageTransform::EnsureLastMessageUser {
                    fallback_user_text: ".".into(),
                },
            ],
        );
        assert_eq!(
            output,
            vec![
                message(User, "[SYSTEM CONTEXT] instructions\n\none\n\ntwo"),
                message(Assistant, "reply"),
                message(User, ".")
            ]
        );
    }

    #[test]
    fn empty_and_system_ending_prompts_follow_python_semantics() {
        use PromptRole::*;
        let ensure = MessageTransform::EnsureLastMessageUser {
            fallback_user_text: ".".into(),
        };
        assert_eq!(
            apply_message_transforms(vec![], std::slice::from_ref(&ensure)),
            vec![message(User, ".")]
        );
        assert_eq!(
            apply_message_transforms(vec![message(System, "keep")], &[ensure]),
            vec![message(System, "keep")]
        );
        assert!(
            apply_message_transforms(
                vec![message(System, " ")],
                &[MessageTransform::TrailingSystemToUserPrefix { tag: "tag".into() }]
            )
            .is_empty()
        );
    }

    #[test]
    fn unknown_transform_or_parameter_is_rejected() {
        assert!(serde_json::from_str::<MessageTransform>(r#"{"id":"unknown"}"#).is_err());
        assert!(serde_json::from_str::<MessageTransform>(r#"{"id":"ensure_last_message_user","params":{"fallback_user_text":".","typo":true}}"#).is_err());
    }
}
