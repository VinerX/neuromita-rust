//! Transient runtime information captured from one connected game session.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GameRuntimeContext {
    pub revision: u64,
    pub scene: Option<String>,
    pub world: Map<String, Value>,
    pub capabilities: Vec<String>,
}

impl GameRuntimeContext {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.scene.as_ref().is_some_and(|s| s.len() > 256)
            || self.capabilities.len() > 64
            || self
                .capabilities
                .iter()
                .any(|c| c.is_empty() || c.len() > 128)
            || self.world.len() > 128
            || self.world.keys().any(|key| key.len() > 128)
            || serde_json::to_vec(&self.world).map_or(true, |data| data.len() > 32768)
        {
            return Err("game runtime context exceeds limits");
        }
        Ok(())
    }

    pub fn render(&self) -> Arc<str> {
        Arc::from(serde_json::to_string(self).expect("validated runtime context is serializable"))
    }
}
