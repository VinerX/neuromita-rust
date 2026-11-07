use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tokio::sync::broadcast;

use super::*;
use crate::timeline::selection::TimelineAdmission;

pub(crate) struct RequestRegistry {
    inner: Arc<RegistryInner>,
}

pub(crate) struct RegistryInner {
    records: Mutex<HashMap<RequestId, Arc<RequestRecord>>>,
    accepted: Mutex<Option<broadcast::Sender<RequestHandle>>>,
    diagnostics: Mutex<DiagnosticsStore>,
}

impl RegistryInner {
    pub(crate) fn complete(&self, record: &RequestRecord) {
        let mut records = self.records.lock().expect("registry mutex poisoned");
        if let Some(trace) = record.take_trace() {
            self.diagnostics
                .lock()
                .expect("diagnostics mutex poisoned")
                .complete(trace);
        }
        records.remove(&record.snapshot().metadata.id);
    }

    pub(crate) fn diagnostics_limits(&self) -> DiagnosticsLimits {
        self.diagnostics
            .lock()
            .expect("diagnostics mutex poisoned")
            .limits()
    }

    pub(crate) fn diagnostic(&self, id: RequestId) -> DiagnosticsLookup {
        let active = self
            .records
            .lock()
            .expect("registry mutex poisoned")
            .get(&id)
            .cloned();
        if let Some(trace) = active.and_then(|record| record.trace()) {
            return DiagnosticsLookup::Trace(trace);
        }
        self.diagnostics
            .lock()
            .expect("diagnostics mutex poisoned")
            .find(id)
    }

    pub(crate) fn diagnostic_latest(&self, scope: DiagnosticScope) -> Option<Arc<RequestTrace>> {
        let active = self
            .records
            .lock()
            .expect("registry mutex poisoned")
            .values()
            .filter_map(|record| record.trace())
            .filter(|trace| scope.allows(trace.metadata.character_id, trace.metadata.source))
            .max_by_key(|trace| trace.timing.started_at_unix_ms);
        let completed = self
            .diagnostics
            .lock()
            .expect("diagnostics mutex poisoned")
            .latest(scope);
        match (active, completed) {
            (Some(active), Some(completed)) => Some(
                if active.timing.started_at_unix_ms >= completed.timing.started_at_unix_ms {
                    active
                } else {
                    completed
                },
            ),
            (active, completed) => active.or(completed),
        }
    }

    pub(crate) fn diagnostic_recent(
        &self,
        scope: DiagnosticScope,
        limit: usize,
    ) -> Vec<RequestTraceSummary> {
        let mut rows: Vec<_> = self
            .records
            .lock()
            .expect("registry mutex poisoned")
            .values()
            .filter_map(|record| record.trace())
            .filter(|trace| scope.allows(trace.metadata.character_id, trace.metadata.source))
            .map(|trace| trace.summary())
            .collect();
        rows.extend(
            self.diagnostics
                .lock()
                .expect("diagnostics mutex poisoned")
                .recent(scope, limit),
        );
        rows.sort_by_key(|row| std::cmp::Reverse(row.started_at_unix_ms));
        rows.truncate(limit.max(1));
        rows
    }

    pub(crate) fn diagnostic_errors(
        &self,
        scope: DiagnosticScope,
        limit: usize,
    ) -> Vec<DiagnosticErrorSummary> {
        let records = self.records.lock().expect("registry mutex poisoned");
        let mut errors: Vec<_> = records
            .values()
            .filter(|record| record.snapshot().state.is_terminal())
            .filter_map(|record| record.trace())
            .filter(|trace| scope.allows(trace.metadata.character_id, trace.metadata.source))
            .filter_map(|trace| trace.error_history_entry())
            .collect();
        errors.sort_by_key(|error| std::cmp::Reverse(error.at_unix_ms));
        errors.extend(
            self.diagnostics
                .lock()
                .expect("diagnostics mutex poisoned")
                .errors(scope, limit),
        );
        errors.truncate(limit.max(1));
        errors
    }
}

pub struct RequestDiscovery {
    pub active: Vec<RequestHandle>,
    accepted: broadcast::Receiver<RequestHandle>,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error(
        "request discovery lagged by {0} notifications; resubscribe to recover active requests"
    )]
    Lagged(u64),
    #[error("request discovery closed")]
    Closed,
}

impl RequestDiscovery {
    pub(crate) fn try_next(&mut self) -> Option<Result<RequestHandle, DiscoveryError>> {
        match self.accepted.try_recv() {
            Ok(handle) => Some(Ok(handle)),
            Err(broadcast::error::TryRecvError::Lagged(count)) => {
                Some(Err(DiscoveryError::Lagged(count)))
            }
            Err(_) => None,
        }
    }

    pub async fn next(&mut self) -> Result<RequestHandle, DiscoveryError> {
        self.accepted.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(count) => DiscoveryError::Lagged(count),
            broadcast::error::RecvError::Closed => DiscoveryError::Closed,
        })
    }
}

impl RequestRegistry {
    #[cfg(test)]
    pub(crate) fn new(discovery_capacity: usize) -> Self {
        Self::new_with_diagnostics(discovery_capacity, DiagnosticsLimits::default())
    }

    pub(crate) fn new_with_diagnostics(
        discovery_capacity: usize,
        limits: DiagnosticsLimits,
    ) -> Self {
        let (accepted, _) = broadcast::channel(discovery_capacity);
        Self {
            inner: Arc::new(RegistryInner {
                records: Mutex::new(HashMap::new()),
                accepted: Mutex::new(Some(accepted)),
                diagnostics: Mutex::new(DiagnosticsStore::new(limits)),
            }),
        }
    }

    pub(crate) fn diagnostics_access(&self, scope: DiagnosticScope) -> DiagnosticsAccess {
        DiagnosticsAccess::new(Arc::downgrade(&self.inner), scope)
    }

    pub(crate) fn register(
        &self,
        message: Box<dyn GenerationCommand>,
        id: RequestId,
        admission: TimelineAdmission,
    ) -> RequestHandle {
        let record = RequestRecord::new(message, id, admission, Arc::downgrade(&self.inner));
        let handle = record.handle();
        let mut records = self.inner.records.lock().expect("registry mutex poisoned");
        records.insert(handle.id(), record);
        if let Some(accepted) = &*self
            .inner
            .accepted
            .lock()
            .expect("discovery mutex poisoned")
        {
            let _ = accepted.send(handle.clone());
        }
        handle
    }

    pub(crate) fn get(&self, id: RequestId) -> Option<Arc<RequestRecord>> {
        self.inner
            .records
            .lock()
            .expect("registry mutex poisoned")
            .get(&id)
            .cloned()
    }

    pub(crate) fn trace(&self, id: RequestId) -> Option<Arc<RequestTrace>> {
        match self.inner.diagnostic(id) {
            DiagnosticsLookup::Trace(trace) => Some(trace),
            DiagnosticsLookup::Expired { .. } | DiagnosticsLookup::Missing => None,
        }
    }
    pub(crate) fn observe(&self) -> RequestDiscovery {
        let records = self.inner.records.lock().expect("registry mutex poisoned");
        let accepted = self
            .inner
            .accepted
            .lock()
            .expect("discovery mutex poisoned")
            .as_ref()
            .expect("request registry is open")
            .subscribe();
        let active = records
            .values()
            .filter(|record| !record.snapshot().state.is_terminal())
            .map(|record| record.handle())
            .collect();
        RequestDiscovery { active, accepted }
    }

    pub(crate) fn cancel_all(&self) {
        self.cancel_all_with_reason(CancelReason::Shutdown);
    }

    pub(crate) fn cancel_all_with_reason(&self, reason: CancelReason) {
        let handles: Vec<_> = self
            .inner
            .records
            .lock()
            .expect("registry mutex poisoned")
            .values()
            .map(|record| record.handle())
            .collect();
        for handle in handles {
            handle.cancel_with_reason(reason);
        }
    }

    pub(crate) fn close(&self) {
        self.inner
            .accepted
            .lock()
            .expect("discovery mutex poisoned")
            .take();
    }

    pub(crate) fn active_count(&self) -> usize {
        self.inner
            .records
            .lock()
            .expect("registry mutex poisoned")
            .values()
            .filter(|record| !record.snapshot().state.is_terminal())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::CharacterId,
        identity::EventId,
        request::{ReactRequest, RequestSource},
        timeline::selection::TimelineSelection,
    };

    #[test]
    fn terminal_error_is_visible_before_archival_and_is_not_duplicated_afterward() {
        let registry = RequestRegistry::new_with_diagnostics(4, DiagnosticsLimits::default());
        let id = RequestId::new();
        let record = RequestRecord::new(
            Box::new(ReactRequest::new(
                CharacterId::Crazy,
                EventId::new(),
                "fixture",
                RequestSource::Presenter,
            )),
            id,
            TimelineAdmission {
                selection: TimelineSelection {
                    branch_id: crate::timeline::domain::BranchId::new(),
                    generation: 1,
                },
                context_epoch: 0,
            },
            std::sync::Weak::new(),
        );
        registry
            .inner
            .records
            .lock()
            .unwrap()
            .insert(id, record.clone());
        record.finish(RequestOutcome::InternalError(
            Arc::<str>::from("fixture failure").into(),
        ));
        let scope = DiagnosticScope::character(CharacterId::Crazy);
        let errors = registry.inner.diagnostic_errors(scope, 8);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].request_id, id.0);
        registry.inner.complete(&record);
        assert_eq!(registry.inner.diagnostic_errors(scope, 8).len(), 1);
        assert!(
            registry
                .inner
                .diagnostic_errors(DiagnosticScope::character(CharacterId::Kind), 8)
                .is_empty()
        );
    }

    #[test]
    fn registry_limits_apply_to_records_created_by_the_registry() {
        let limits = DiagnosticsLimits {
            max_text_bytes: 8,
            ..DiagnosticsLimits::default()
        };
        let registry = RequestRegistry::new_with_diagnostics(4, limits);
        let command = ReactRequest::new(
            CharacterId::Crazy,
            EventId::new(),
            "x".repeat(100),
            RequestSource::Presenter,
        );
        let handle = registry.register(
            Box::new(command),
            RequestId::new(),
            TimelineAdmission {
                selection: TimelineSelection {
                    branch_id: crate::timeline::domain::BranchId::new(),
                    generation: 1,
                },
                context_epoch: 0,
            },
        );
        let trace = registry.trace(handle.id()).unwrap();
        assert!(trace.input.truncated);
        assert!(trace.guidance.len() <= 8);
    }
}
