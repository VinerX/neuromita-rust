//! Functional audio administration, independent of pages and recognition models.

use crate::audio::{
    device::DeviceError, input_catalog::AudioInputDevice, input_owner::AudioInputOwner,
    monitor::MonitorSnapshot,
};
use std::sync::Arc;
use tokio::sync::watch;

pub struct AudioInputPresenter {
    owner: Arc<AudioInputOwner>,
    updates: watch::Receiver<MonitorSnapshot>,
}

impl AudioInputPresenter {
    pub(crate) fn new(owner: Arc<AudioInputOwner>) -> Self {
        Self {
            updates: owner.observe(),
            owner,
        }
    }
    pub async fn devices(&self) -> Result<Vec<AudioInputDevice>, DeviceError> {
        tokio::task::spawn_blocking(crate::audio::native::discover)
            .await
            .map_err(|error| DeviceError::Stopped(error.to_string()))?
    }
    pub fn observe_test(&self) -> watch::Receiver<MonitorSnapshot> {
        self.updates.clone()
    }
    pub async fn start_test(&self, backend: &str, identity: &str) -> Result<(), DeviceError> {
        self.owner.start_test(backend, identity).await
    }
    pub async fn stop_test(&self) -> Result<(), DeviceError> {
        self.owner.stop_test(true).await
    }
}
