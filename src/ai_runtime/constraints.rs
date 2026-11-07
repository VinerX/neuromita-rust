//! Plugin-declared restrictions are combined with Core policy, never used as defaults.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSeverity {
    Warning,
    Forbidden,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationConstraint {
    pub field: String,
    pub value: Value,
    pub severity: ConstraintSeverity,
    pub code: String,
    pub message: String,
}

pub fn validate_constraints(items: &[ConfigurationConstraint]) -> Result<(), String> {
    if items.len() > 256 {
        return Err("too many configuration constraints".into());
    }
    for item in items {
        let (scope, field) = item
            .field
            .split_once('.')
            .ok_or("invalid constraint field")?;
        if !matches!(scope, "settings" | "runtime")
            || field.is_empty()
            || field.contains('.')
            || item.field.len() > 2048
            || item.code.is_empty()
            || item.code.len() > 2048
            || item.message.is_empty()
            || item.message.len() > 2048
            || item.value.is_array()
            || item.value.is_object()
        {
            return Err("invalid configuration constraint".into());
        }
    }
    Ok(())
}

pub fn enforce_constraints(
    items: &[ConfigurationConstraint],
    settings: &Value,
    runtime: Option<&Value>,
) -> Result<(), String> {
    validate_constraints(items)?;
    for item in items {
        let (scope, field) = item
            .field
            .split_once('.')
            .ok_or("invalid constraint field")?;
        let values = if scope == "settings" {
            Some(settings)
        } else {
            runtime
        };
        if item.severity == ConstraintSeverity::Forbidden
            && values.and_then(|value| value.get(field)) == Some(&item.value)
        {
            return Err(format!("{}: {}", item.code, item.message));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forbids_fp16_without_rewriting_the_selection() {
        let items: Vec<ConfigurationConstraint> = serde_json::from_value(serde_json::json!([{
            "field":"runtime.precision", "value":"fp16", "severity":"forbidden", "code":"unsupported_fp16", "message":"Unsupported device"
        }])).unwrap();
        let runtime = serde_json::json!({"precision":"fp16"});
        assert!(enforce_constraints(&items, &serde_json::json!({}), Some(&runtime)).is_err());
        assert_eq!(runtime["precision"], "fp16");
        assert!(
            enforce_constraints(
                &items,
                &serde_json::json!({}),
                Some(&serde_json::json!({"precision":"fp32"}))
            )
            .is_ok()
        );
    }
}
