//! Read-only presentation of the application's AI worker lifecycle.

use std::sync::Weak;

use tokio::sync::watch;

use crate::ai_runtime::{
    AiService, AiSupervisor,
    runtime_status::{RuntimePhase, RuntimeSnapshot, ServiceRuntimeStatus},
};

/// Read-only presenter-facing view of the supervisor's latest state.
/// Subscribers receive coalesced snapshots rather than an event history.
pub struct RuntimeStatePresenter {
    updates: Option<watch::Receiver<RuntimeSnapshot>>,
    supervisor: Weak<AiSupervisor>,
    live_speech: Weak<crate::ai_runtime::live_speech::LiveSpeechRuntime>,
    startup_failure: Option<RuntimeSnapshot>,
}

impl RuntimeStatePresenter {
    pub(crate) fn new(updates: Option<watch::Receiver<RuntimeSnapshot>>) -> Self {
        Self {
            updates,
            supervisor: Weak::new(),
            live_speech: Weak::new(),
            startup_failure: None,
        }
    }

    /// An unsuccessful automatic activation is a real failed startup, not an active model.
    pub(crate) fn with_startup_failure(mut self, services: &[AiService], reason: &str) -> Self {
        let mut snapshot = RuntimeSnapshot::default();
        for service in services {
            snapshot.services.insert(
                *service,
                ServiceRuntimeStatus {
                    phase: RuntimePhase::Failed,
                    last_error: Some(reason.to_owned()),
                    ..Default::default()
                },
            );
        }
        self.startup_failure = Some(snapshot);
        self
    }

    pub(crate) fn with_supervisor(mut self, supervisor: Weak<AiSupervisor>) -> Self {
        self.supervisor = supervisor;
        self
    }

    pub(crate) fn with_live_speech(
        mut self,
        runtime: Weak<crate::ai_runtime::live_speech::LiveSpeechRuntime>,
    ) -> Self {
        self.live_speech = runtime;
        self
    }

    /// Reconcile a worker that exited outside any IPC operation.
    /// No new process or model is created by this call.
    pub async fn refresh_status(&self) -> RuntimeSnapshot {
        if let Some(runtime) = self.live_speech.upgrade() {
            runtime.refresh().await
        } else if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.refresh_status().await
        } else {
            self.snapshot()
        }
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.updates.as_ref().map_or_else(
            || self.startup_failure.clone().unwrap_or_default(),
            |updates| updates.borrow().clone(),
        )
    }

    pub fn observe(&self) -> Option<watch::Receiver<RuntimeSnapshot>> {
        self.updates.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::{
        AiService,
        runtime_status::{RuntimePhase, ServiceRuntimeStatus},
    };

    #[test]
    fn detached_presenter_never_reports_an_active_model() {
        let presenter = RuntimeStatePresenter::new(None);
        let snapshot = presenter.snapshot();
        for service in [AiService::Tts, AiService::Asr] {
            let status = snapshot.service(service);
            assert_eq!(status.phase, RuntimePhase::Stopped);
            assert!(status.generation.is_none());
            assert!(status.active_model.is_none());
        }
        assert!(presenter.observe().is_none());
    }

    #[test]
    fn startup_failure_is_observable_without_a_fake_worker() {
        let presenter = RuntimeStatePresenter::new(None)
            .with_startup_failure(&[AiService::Asr], "missing locked dependency");
        let status = presenter.snapshot().service(AiService::Asr);
        assert_eq!(status.phase, RuntimePhase::Failed);
        assert_eq!(
            status.last_error.as_deref(),
            Some("missing locked dependency")
        );
        assert!(status.active_model.is_none());
        assert_eq!(
            presenter.snapshot().service(AiService::Tts).phase,
            RuntimePhase::Stopped
        );
        assert!(presenter.observe().is_none());
    }

    #[tokio::test]
    async fn detached_refresh_is_inert_and_never_starts_a_worker() {
        let presenter = RuntimeStatePresenter::new(None);
        let state = presenter.refresh_status().await;
        for service in [AiService::Tts, AiService::Asr] {
            assert_eq!(state.service(service).phase, RuntimePhase::Stopped);
        }
    }

    #[tokio::test]
    async fn presenter_observes_typed_snapshots_without_editing_them() {
        let (sender, updates) = watch::channel(RuntimeSnapshot::default());
        let presenter = RuntimeStatePresenter::new(Some(updates));
        let mut observed = presenter.observe().unwrap();
        sender.send_modify(|snapshot| {
            snapshot.services.insert(
                AiService::Asr,
                ServiceRuntimeStatus {
                    phase: RuntimePhase::Starting,
                    pending_model: Some("whisper".into()),
                    ..Default::default()
                },
            );
        });
        observed.changed().await.unwrap();
        let state = presenter.snapshot().service(AiService::Asr);
        assert_eq!(state.phase, RuntimePhase::Starting);
        assert_eq!(state.pending_model.as_deref(), Some("whisper"));
        assert!(state.active_model.is_none());
    }
}
