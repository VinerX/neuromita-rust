use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{registry::RegistryInner, *};
use crate::timeline::selection::TimelineAdmission;
#[cfg(test)]
use crate::timeline::selection::TimelineSelection;

pub(crate) struct RequestRecord {
    inner: Mutex<Execution>,
    updates: watch::Sender<Arc<RequestSnapshot>>,
    pub(crate) cancellation: CancellationToken,
    registry: Weak<RegistryInner>,
    admission: TimelineAdmission,
}

struct Execution {
    snapshot: Arc<RequestSnapshot>,
    dataset: DatasetCapture,
    phase: EffectPhase,
    cancel_reason: Option<CancelReason>,
    message: Option<Box<dyn GenerationCommand>>,
    trace: Option<RequestTrace>,
    draft_text: Option<Arc<str>>,
}

#[derive(Clone)]
pub struct RequestHandle {
    pub(crate) record: Arc<RequestRecord>,
}

impl std::fmt::Debug for RequestHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestHandle")
            .field("id", &self.id())
            .finish()
    }
}

impl RequestHandle {
    pub fn id(&self) -> RequestId {
        self.record.snapshot().metadata.id
    }
    pub fn snapshot(&self) -> Arc<RequestSnapshot> {
        self.record.snapshot()
    }
    pub fn cancel(&self) -> CancelResult {
        self.record.cancel()
    }
    pub(crate) fn cancel_with_reason(&self, reason: CancelReason) -> CancelResult {
        self.record.cancel_with_reason(reason)
    }
    pub fn observe(&self) -> RequestObserver {
        RequestObserver {
            updates: self.record.updates.subscribe(),
            initial: true,
            finished: false,
        }
    }
    pub async fn wait(&self) -> Result<Arc<RequestSnapshot>, ObserveError> {
        let mut observer = self.observe();
        while let Some(snapshot) = observer.next().await? {
            if snapshot.state.is_terminal() {
                return Ok(snapshot);
            }
        }
        Err(ObserveError::ClosedBeforeCompletion)
    }
}

pub struct RequestObserver {
    updates: watch::Receiver<Arc<RequestSnapshot>>,
    initial: bool,
    finished: bool,
}

impl RequestObserver {
    pub async fn next(&mut self) -> Result<Option<Arc<RequestSnapshot>>, ObserveError> {
        if self.finished {
            return Ok(None);
        }
        if !self.initial {
            self.updates
                .changed()
                .await
                .map_err(|_| ObserveError::ClosedBeforeCompletion)?;
        }
        self.initial = false;
        let snapshot = Arc::clone(&self.updates.borrow_and_update());
        self.finished = snapshot.state.is_terminal();
        Ok(Some(snapshot))
    }
}

impl RequestRecord {
    pub(crate) fn new(
        message: Box<dyn GenerationCommand>,
        id: RequestId,
        admission: TimelineAdmission,
        registry: Weak<RegistryInner>,
    ) -> Arc<Self> {
        let snapshot = Arc::new(RequestSnapshot {
            metadata: RequestMetadata {
                id,
                character_id: message.character_id(),
                source: message.source(),
                intent: message.intent(),
            },
            revision: 0,
            state: RequestState::Pending,
            dataset: DatasetCapture::default(),
            cancel_reason: None,
        });
        let (updates, _) = watch::channel(Arc::clone(&snapshot));
        let diagnostics_limits = registry
            .upgrade()
            .map_or_else(DiagnosticsLimits::default, |registry| {
                registry.diagnostics_limits()
            });
        let trace = RequestTrace::new(
            snapshot.metadata.clone(),
            message.guidance(),
            diagnostics_limits,
        );
        Arc::new(Self {
            inner: Mutex::new(Execution {
                snapshot,
                dataset: DatasetCapture::default(),
                phase: EffectPhase::Working,
                cancel_reason: None,
                message: Some(message),
                trace: Some(trace),
                draft_text: None,
            }),
            updates,
            cancellation: CancellationToken::new(),
            registry,
            admission,
        })
    }

    pub(crate) fn handle(self: &Arc<Self>) -> RequestHandle {
        RequestHandle {
            record: Arc::clone(self),
        }
    }
    pub(crate) fn snapshot(&self) -> Arc<RequestSnapshot> {
        Arc::clone(&self.inner.lock().expect("request mutex poisoned").snapshot)
    }

    fn publish(&self, inner: &mut Execution, state: RequestState) {
        let snapshot = Arc::new(RequestSnapshot {
            metadata: inner.snapshot.metadata.clone(),
            revision: inner.snapshot.revision + 1,
            state,
            dataset: inner.dataset.clone(),
            cancel_reason: inner.cancel_reason,
        });
        inner.snapshot = Arc::clone(&snapshot);
        let phase = inner.phase;
        if let Some(trace) = &mut inner.trace {
            trace.push_transition(snapshot.revision, phase, &snapshot.state);
        }
        self.updates.send_replace(snapshot);
    }

    fn retire(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.complete(self);
        }
    }

    pub(crate) fn set_preset_description(&self, description: String) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.set_preset_description(description);
        }
    }

    pub(crate) fn set_redaction_values(&self, values: Vec<String>) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.configure_redaction(values);
        }
    }

    pub(crate) fn admission(&self) -> TimelineAdmission {
        self.admission
    }

    pub(crate) fn try_start(&self) -> Option<Box<dyn GenerationCommand>> {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if !matches!(inner.snapshot.state, RequestState::Pending) {
            return None;
        }
        let message = inner.message.take()?;
        if let Some(trace) = &mut inner.trace {
            trace.mark_started();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::GeneratingText),
        );
        Some(message)
    }

    pub(crate) fn cancel(&self) -> CancelResult {
        self.cancel_with_reason(CancelReason::User)
    }

    pub(crate) fn cancel_with_reason(&self, reason: CancelReason) -> CancelResult {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.state.is_terminal() {
            return CancelResult::AlreadyFinished;
        }
        if matches!(inner.phase, EffectPhase::Accepting | EffectPhase::Applied) {
            return CancelResult::TooLate;
        }
        inner.cancel_reason.get_or_insert(reason);
        self.cancellation.cancel();
        if matches!(inner.snapshot.state, RequestState::Pending) {
            inner.phase = EffectPhase::Discarded;
            inner.message.take();
            self.publish(&mut inner, RequestState::Cancelled);
            drop(inner);
            self.retire();
            CancelResult::Cancelled
        } else {
            CancelResult::Requested
        }
    }

    pub(crate) fn begin_accepting(&self) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || inner.snapshot.state.is_terminal()
            || self.cancellation.is_cancelled()
        {
            return false;
        }
        let Some(text) = inner.draft_text.clone() else {
            return false;
        };
        inner.phase = EffectPhase::Accepting;
        if let Some(trace) = &mut inner.trace {
            trace.accepting();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::Accepting { text }),
        );
        true
    }

    pub(crate) fn mark_applied(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        assert_eq!(inner.phase, EffectPhase::Accepting);
        inner.phase = EffectPhase::Applied;
        let snapshot = Arc::clone(&inner.snapshot);
        if let Some(trace) = &mut inner.trace {
            trace.push_transition(snapshot.revision, EffectPhase::Applied, &snapshot.state);
        }
    }

    pub(crate) fn finish(&self, outcome: RequestOutcome) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.state.is_terminal() {
            return;
        }
        let cancelled = inner.phase == EffectPhase::Working && self.cancellation.is_cancelled();
        if inner.phase != EffectPhase::Applied {
            inner.phase = if matches!(
                outcome,
                RequestOutcome::Success(_) | RequestOutcome::VoiceGenerationFailed { .. }
            ) {
                EffectPhase::Applied
            } else {
                EffectPhase::Discarded
            };
        }
        let state = if cancelled {
            RequestState::Cancelled
        } else {
            RequestState::Finished(outcome)
        };
        if let Some(trace) = &mut inner.trace {
            match &state {
                RequestState::Finished(outcome) => trace.outcome(outcome),
                RequestState::Cancelled => trace.cancelled(),
                _ => {}
            }
            trace.finished();
        }
        self.publish(&mut inner, state);
        drop(inner);
        self.retire();
    }

    pub(crate) fn finish_cancelled(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.state.is_terminal()
            || matches!(inner.phase, EffectPhase::Accepting | EffectPhase::Applied)
        {
            return;
        }
        inner.phase = EffectPhase::Discarded;
        inner.message.take();
        if let Some(trace) = &mut inner.trace {
            trace.cancelled();
            trace.finished();
        }
        self.publish(&mut inner, RequestState::Cancelled);
        drop(inner);
        self.retire();
    }

    pub(crate) fn prepared(&self, context: crate::character::CharacterContext) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.prepared(&context);
        }
    }
    pub(crate) fn dataset_capture(
        &self,
        sample_id: Option<crate::finetune::SampleId>,
        warning: Option<Arc<str>>,
    ) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        inner.dataset.sample_id = sample_id;
        inner.dataset.warning = warning;
        let snapshot = Arc::new(RequestSnapshot {
            metadata: inner.snapshot.metadata.clone(),
            revision: inner.snapshot.revision + 1,
            state: inner.snapshot.state.clone(),
            cancel_reason: inner.cancel_reason,
            dataset: inner.dataset.clone(),
        });
        inner.snapshot = Arc::clone(&snapshot);
        self.updates.send_replace(snapshot);
    }
    pub(crate) fn text_ready(&self, text: Arc<str>) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::GeneratingText)
            )
        {
            return false;
        }
        inner.draft_text = Some(Arc::clone(&text));
        if let Some(trace) = &mut inner.trace {
            trace.text_ready(&text);
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::TextReady { text }),
        );
        true
    }
    pub(crate) fn failure(&self, message: String) -> InternalFailure {
        InternalFailure {
            message: Arc::from(message),
            partial_text: self
                .inner
                .lock()
                .expect("request mutex poisoned")
                .draft_text
                .clone(),
        }
    }
    pub(crate) fn take_trace(&self) -> Option<Arc<RequestTrace>> {
        self.inner
            .lock()
            .expect("request mutex poisoned")
            .trace
            .take()
            .map(Arc::new)
    }
    pub(crate) fn trace(&self) -> Option<Arc<RequestTrace>> {
        self.inner
            .lock()
            .expect("request mutex poisoned")
            .trace
            .clone()
            .map(Arc::new)
    }
    pub(crate) fn generating_voice(&self) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::TextReady { .. })
            )
        {
            return false;
        }
        let Some(text) = inner.draft_text.clone() else {
            return false;
        };
        if let Some(trace) = &mut inner.trace {
            trace.voice_started();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::GeneratingVoice { text }),
        );
        true
    }

    pub(crate) fn voice_finished(&self) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.voice_finished();
        }
    }

    pub(crate) fn provider_requested(&self, request: crate::generation::PreparedRequest) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_requested(request);
        }
    }

    pub(crate) fn provider_replied(&self, reply: crate::generation::ProviderReply) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_replied(reply);
        }
    }

    pub(crate) fn provider_failed(&self, error: &crate::generation::GenerationError) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_failed(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;
    use crate::{character::CharacterId, identity::EventId};

    fn request() -> ReplyRequest {
        ReplyRequest::new(CharacterId::Crazy, EventId::new(), RequestSource::Presenter)
    }

    fn selection() -> TimelineSelection {
        TimelineSelection {
            branch_id: crate::timeline::domain::BranchId::new(),
            generation: 1,
        }
    }

    #[test]
    fn cancellation_and_acceptance_have_one_winner() {
        for _ in 0..64 {
            let registry = RequestRegistry::new(4);
            let handle = registry.register(
                Box::new(request()),
                RequestId::new(),
                TimelineAdmission {
                    selection: selection(),
                    context_epoch: 0,
                },
            );
            let record = Arc::clone(&handle.record);
            assert!(record.try_start().is_some());
            assert!(record.text_ready(Arc::from("ready")));
            let barrier = Barrier::new(2);
            let (accepted, cancelled) = std::thread::scope(|scope| {
                let accepting = scope.spawn(|| {
                    barrier.wait();
                    record.begin_accepting()
                });
                let cancelling = scope.spawn(|| {
                    barrier.wait();
                    handle.cancel()
                });
                (accepting.join().unwrap(), cancelling.join().unwrap())
            });
            if accepted {
                assert_eq!(cancelled, CancelResult::TooLate);
                assert!(!record.cancellation.is_cancelled());
                record.finish(RequestOutcome::InternalError(
                    Arc::<str>::from("simulated acceptance rejection").into(),
                ));
                assert!(matches!(handle.snapshot().state, RequestState::Finished(_)));
            } else {
                assert_eq!(cancelled, CancelResult::Requested);
                record.finish_cancelled();
                assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
            }
            assert_eq!(registry.active_count(), 0);
        }
    }

    #[test]
    fn voice_remains_cancellable_until_acceptance() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        assert!(record.try_start().is_some());
        assert_eq!(handle.snapshot().provisional_text(), None);
        assert!(handle.snapshot().can_cancel());
        assert!(record.text_ready(Arc::from("provisional text")));
        assert_eq!(
            handle.snapshot().provisional_text(),
            Some("provisional text")
        );
        assert!(matches!(
            handle.snapshot().state,
            RequestState::Processing(ProcessingStage::TextReady { .. })
        ));
        assert!(record.generating_voice());
        assert_eq!(
            handle.snapshot().provisional_text(),
            Some("provisional text")
        );
        assert!(handle.snapshot().can_cancel());
        let trace = record.trace().unwrap();
        assert_eq!(
            trace.transitions.last().unwrap().phase,
            EffectPhase::Working
        );
        assert_eq!(handle.cancel(), CancelResult::Requested);
        assert!(!record.begin_accepting());
        record.finish_cancelled();
        assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
        assert_eq!(handle.snapshot().provisional_text(), None);
        assert!(!handle.snapshot().can_cancel());
    }

    #[test]
    fn accepting_reuses_provisional_text_and_closes_cancellation_boundary() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        assert!(!record.begin_accepting());
        assert!(record.try_start().is_some());
        assert!(!record.begin_accepting());
        assert!(record.text_ready(Arc::from("accepted after voice")));
        assert!(record.generating_voice());
        let voice_revision = handle.snapshot().revision;
        assert!(record.begin_accepting());
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.revision, voice_revision + 1);
        assert!(matches!(
            snapshot.state,
            RequestState::Processing(ProcessingStage::Accepting { .. })
        ));
        assert_eq!(snapshot.provisional_text(), Some("accepted after voice"));
        assert!(!snapshot.can_cancel());
        assert_eq!(handle.cancel(), CancelResult::TooLate);
        assert!(!record.cancellation.is_cancelled());
        let status = crate::server::status(&snapshot);
        assert_eq!(status["stage"], "accepting");
        assert_eq!(status["text"], "accepted after voice");
        assert_eq!(status["provisional"], true);
        assert_eq!(status["can_cancel"], false);
        record.finish(RequestOutcome::InternalError(
            Arc::<str>::from("rejected").into(),
        ));
        let finished = handle.snapshot();
        assert!(finished.state.is_terminal());
        assert_eq!(finished.provisional_text(), None);
        assert!(!finished.can_cancel());
        assert_eq!(
            crate::server::status(&finished)["provisional"],
            serde_json::Value::Null
        );
        assert_eq!(registry.active_count(), 0);
        let trace = registry.trace(handle.id()).unwrap();
        assert!(trace.transitions.iter().any(|transition| {
            transition.phase == EffectPhase::Accepting && transition.revision == snapshot.revision
        }));
    }

    #[test]
    fn queued_cancel_prevents_start_and_keeps_terminal_handle() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        assert_eq!(handle.cancel(), CancelResult::Cancelled);
        assert!(handle.record.try_start().is_none());
        assert_eq!(registry.active_count(), 0);
        assert!(registry.get(handle.id()).is_none());
        assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
    }
}
