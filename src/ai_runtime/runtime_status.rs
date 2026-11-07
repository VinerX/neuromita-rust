//! Observable snapshots of real Python worker lifecycle; not an install inventory.

use std::collections::HashMap;

use uuid::Uuid;

use super::AiService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    Stopped,
    Starting,
    WorkerRunning,
    ModelReady,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModel {
    pub plugin_id: String,
    pub asset_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRuntimeStatus {
    pub phase: RuntimePhase,
    pub generation: Option<Uuid>,
    pub active_model: Option<LoadedModel>,
    pub pending_model: Option<String>,
    pub last_error: Option<String>,
}

impl Default for ServiceRuntimeStatus {
    fn default() -> Self {
        Self {
            phase: RuntimePhase::Stopped,
            generation: None,
            active_model: None,
            pending_model: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeSnapshot {
    pub services: HashMap<AiService, ServiceRuntimeStatus>,
}

impl RuntimeSnapshot {
    pub fn service(&self, service: AiService) -> ServiceRuntimeStatus {
        self.services.get(&service).cloned().unwrap_or_default()
    }
}
