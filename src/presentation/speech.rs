//! Read-only projection of the application-owned speech capture session.

use tokio::sync::watch;

use crate::audio::controller::SpeechSessionSnapshot;

pub struct SpeechSessionPresenter {
    updates: watch::Receiver<SpeechSessionSnapshot>,
}

impl SpeechSessionPresenter {
    pub(crate) fn new(updates: watch::Receiver<SpeechSessionSnapshot>) -> Self {
        Self { updates }
    }

    pub fn snapshot(&self) -> SpeechSessionSnapshot {
        self.updates.borrow().clone()
    }

    pub fn observe(&self) -> watch::Receiver<SpeechSessionSnapshot> {
        self.updates.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::controller::{SpeechSessionPhase, SpeechSessionSnapshot};

    #[tokio::test]
    async fn presenter_reads_controller_state_without_owning_it() {
        let controller = crate::audio::controller::SpeechSessionController::default();
        let presenter = SpeechSessionPresenter::new(controller.observe());
        assert_eq!(presenter.snapshot(), SpeechSessionSnapshot::default());
        assert_eq!(
            presenter.observe().borrow().phase,
            SpeechSessionPhase::Stopped
        );
    }
}
