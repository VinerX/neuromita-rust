use neuromita::ai_runtime::runtime_status::{RuntimePhase, ServiceRuntimeStatus};

pub fn describe(status: &ServiceRuntimeStatus) -> String {
    let text = match status.phase {
        RuntimePhase::Stopped => "Worker остановлен".to_owned(),
        RuntimePhase::Starting => format!(
            "Инициализация {}",
            status.pending_model.as_deref().unwrap_or("worker"),
        ),
        RuntimePhase::WorkerRunning => "Worker запущен, модель не инициализирована".to_owned(),
        RuntimePhase::ModelReady => status.active_model.as_ref().map_or_else(
            || "Worker запущен, модель не подтверждена".to_owned(),
            |model| format!("Активна модель {}", model.plugin_id),
        ),
        RuntimePhase::Failed => "Ошибка AI worker".to_owned(),
    };
    let text = if let Some(pending) = &status.pending_model {
        if status.phase == RuntimePhase::ModelReady {
            format!("{text}; подготавливается {pending}")
        } else {
            text
        }
    } else {
        text
    };
    if let Some(error) = &status.last_error {
        format!("{text}; ошибка: {error}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuromita::ai_runtime::runtime_status::LoadedModel;

    #[test]
    fn never_calls_running_worker_a_loaded_model() {
        let s = ServiceRuntimeStatus {
            phase: RuntimePhase::WorkerRunning,
            ..Default::default()
        };
        assert!(describe(&s).contains("не инициализирована"));
        let s = ServiceRuntimeStatus {
            phase: RuntimePhase::ModelReady,
            active_model: Some(LoadedModel {
                plugin_id: "f5_tts".into(),
                asset_revision: None,
            }),
            pending_model: Some("new_tts".into()),
            last_error: Some("candidate failed".into()),
            ..Default::default()
        };
        let message = describe(&s);
        assert!(message.contains("f5_tts"));
        assert!(message.contains("new_tts"));
        assert!(message.contains("candidate failed"));
    }
}
