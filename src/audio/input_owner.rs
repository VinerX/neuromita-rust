//! Serializes microphone administration and recognition suspension in Core.

use super::{
    controller::{SpeechSessionController, SuspendedCapture},
    device::DeviceError,
    monitor::{MicrophoneMonitor, MonitorSnapshot},
};
use std::sync::Arc;
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};

pub struct AudioInputOwner {
    monitor: Mutex<MicrophoneMonitor>,
    capture: Arc<Mutex<SpeechSessionController>>,
    suspended: Arc<Mutex<Option<SuspendedCapture>>>,
    operation: Arc<Mutex<()>>,
    watcher: Mutex<Option<JoinHandle<()>>>,
    updates: watch::Receiver<MonitorSnapshot>,
}

impl AudioInputOwner {
    pub fn new(capture: Arc<Mutex<SpeechSessionController>>) -> Self {
        let monitor = MicrophoneMonitor::default();
        Self {
            updates: monitor.observe(),
            monitor: Mutex::new(monitor),
            capture,
            suspended: Arc::new(Mutex::new(None)),
            operation: Arc::new(Mutex::new(())),
            watcher: Mutex::new(None),
        }
    }

    pub fn observe(&self) -> watch::Receiver<MonitorSnapshot> {
        self.updates.clone()
    }

    async fn stop_watcher(&self) {
        if let Some(task) = self.watcher.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }

    async fn restore(&self) -> Result<(), DeviceError> {
        if let Some(session) = self.suspended.lock().await.take() {
            self.capture
                .lock()
                .await
                .resume(session)
                .await
                .map_err(|error| DeviceError::Stopped(error.to_string()))?;
        }
        Ok(())
    }

    pub async fn start_test(&self, backend: &str, identity: &str) -> Result<(), DeviceError> {
        let _operation = self.operation.lock().await;
        self.stop_watcher().await;
        self.monitor.lock().await.stop().await;
        if let Some(session) = self.capture.lock().await.suspend().await {
            *self.suspended.lock().await = Some(session);
        }
        if let Err(error) = self.monitor.lock().await.start(backend, identity).await {
            self.restore().await?;
            return Err(error);
        }
        let mut updates = self.observe();
        updates.borrow_and_update();
        let operation = self.operation.clone();
        let suspended = self.suspended.clone();
        let capture = self.capture.clone();
        *self.watcher.lock().await = Some(tokio::spawn(async move {
            while updates.changed().await.is_ok() {
                if !updates.borrow_and_update().running {
                    let _operation = operation.lock().await;
                    if let Some(session) = suspended.lock().await.take()
                        && let Err(error) = capture.lock().await.resume(session).await
                    {
                        tracing::error!(%error, "failed to resume recognition after microphone test");
                    }
                    break;
                }
            }
        }));
        Ok(())
    }

    pub async fn start_capture(
        &self,
        handler: super::session::SpeechInputHandler,
        stop: tokio_util::sync::CancellationToken,
        config: super::controller::SpeechCaptureConfig,
    ) -> Result<(), super::controller::SpeechCaptureError> {
        let _operation = self.operation.lock().await;
        self.stop_watcher().await;
        self.monitor.lock().await.stop().await;
        self.suspended.lock().await.take();
        self.capture.lock().await.start(handler, stop, config).await
    }

    pub async fn stop_capture(&self) {
        let _operation = self.operation.lock().await;
        self.stop_watcher().await;
        self.monitor.lock().await.stop().await;
        self.suspended.lock().await.take();
        self.capture.lock().await.stop().await;
    }

    pub async fn stop_test(&self, resume: bool) -> Result<(), DeviceError> {
        let _operation = self.operation.lock().await;
        self.stop_watcher().await;
        self.monitor.lock().await.stop().await;
        if resume {
            self.restore().await?;
        } else {
            self.suspended.lock().await.take();
        }
        Ok(())
    }
}

impl Drop for AudioInputOwner {
    fn drop(&mut self) {
        if let Some(task) = self.watcher.get_mut().take() {
            task.abort();
        }
    }
}
