use super::*;
use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const DELIVERY_CAPACITY: usize = 16;
pub const DELIVERY_RECORD_BYTES: usize = 32 * 1024 * 1024;
pub const DELIVERY_DRAIN: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Default)]
pub struct DeliveryStatus {
    pub admitted: u64,
    pub completed: u64,
    pub dropped: u64,
    pub rejected: u64,
    pub errors: u64,
    pub pending: usize,
    pub last_error: Option<String>,
    pub closed: bool,
}

enum Command {
    Sample(Box<FineTuneSample>),
    Outcome(OutcomeRecord),
}

#[derive(Clone)]
pub(super) struct Delivery(Arc<Owner>);
struct Owner {
    sender: Mutex<Option<mpsc::SyncSender<Command>>>,
    status: Arc<Mutex<DeliveryStatus>>,
    stop: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Delivery {
    pub fn start(store: FineTuneStore, retention: Retention) -> Result<Self, FineTuneError> {
        let cancellation = CancellationToken::new();
        let store = store.with_cancellation(cancellation.clone());
        Self::start_writer(cancellation, move |command| match command {
            Command::Sample(sample) => store
                .append_sample(&sample, retention)
                .and_then(|result| result.retention.map(|_| ())),
            Command::Outcome(record) => store.append_outcome(&record),
        })
    }
    fn start_writer(
        stop: CancellationToken,
        mut write: impl FnMut(Command) -> Result<(), FineTuneError> + Send + 'static,
    ) -> Result<Self, FineTuneError> {
        let (sender, receiver) = mpsc::sync_channel(DELIVERY_CAPACITY);
        let status = Arc::new(Mutex::new(DeliveryStatus::default()));
        let worker_status = status.clone();
        let worker_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("dataset-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    let result = if worker_stop.is_cancelled() {
                        let mut state = worker_status.lock().unwrap();
                        state.pending -= 1;
                        state.dropped += 1;
                        continue;
                    } else {
                        write(command)
                    };
                    if let Err(error) = &result {
                        tracing::warn!(error = %error, "dataset delivery failed");
                    }
                    let mut state = worker_status.lock().unwrap();
                    state.pending -= 1;
                    match result {
                        Ok(()) => state.completed += 1,
                        Err(error) => {
                            state.errors += 1;
                            state.last_error = Some(error.to_string());
                        }
                    }
                }
            })?;
        Ok(Self(Arc::new(Owner {
            sender: Mutex::new(Some(sender)),
            status,
            stop,
            thread: Mutex::new(Some(thread)),
        })))
    }
    fn submit(&self, command: Command) -> Result<(), FineTuneError> {
        let sender = self.0.sender.lock().unwrap();
        let mut status = self.0.status.lock().unwrap();
        match sender.as_ref().map(|sender| sender.try_send(command)) {
            Some(Ok(())) => {
                status.admitted += 1;
                status.pending += 1;
                Ok(())
            }
            failure => {
                let reason = match failure {
                    Some(Err(mpsc::TrySendError::Full(_))) => "dataset delivery queue full",
                    _ => "dataset delivery closed",
                };
                status.dropped += 1;
                status.rejected += 1;
                status.last_error = Some(reason.into());
                Err(FineTuneError::SettingsInvalid(reason.into()))
            }
        }
    }
    pub fn sample(&self, sample: FineTuneSample) -> Result<(), FineTuneError> {
        if serde_json::to_vec(&sample)
            .map_err(|error| FineTuneError::SettingsInvalid(error.to_string()))?
            .len()
            > DELIVERY_RECORD_BYTES
        {
            let message = "dataset delivery record exceeds 32 MiB";
            let mut status = self.0.status.lock().unwrap();
            status.dropped += 1;
            status.rejected += 1;
            status.last_error = Some(message.into());
            return Err(FineTuneError::SettingsInvalid(message.into()));
        }
        self.submit(Command::Sample(Box::new(sample)))
    }
    pub fn outcome(&self, record: OutcomeRecord) -> Result<(), FineTuneError> {
        self.submit(Command::Outcome(record))
    }
    pub fn status(&self) -> DeliveryStatus {
        self.0.status.lock().unwrap().clone()
    }
    pub fn stop(&self) {
        self.0.sender.lock().unwrap().take();
        self.0.status.lock().unwrap().closed = true;
        self.0.stop.cancel();
    }
    pub async fn drain(&self, budget: Duration, close: bool) -> DeliveryStatus {
        if close {
            self.0.sender.lock().unwrap().take();
            self.0.status.lock().unwrap().closed = true;
        }
        let deadline = Instant::now() + budget;
        loop {
            let status = self.status();
            if status.pending == 0 {
                let mut thread = self.0.thread.lock().unwrap();
                if thread.as_ref().is_some_and(|thread| thread.is_finished()) {
                    let _ = thread.take().unwrap().join();
                }
                return status;
            }
            if Instant::now() >= deadline {
                if close {
                    self.0.stop.cancel();
                    self.0.status.lock().unwrap().last_error = Some(format!(
                        "dataset drain deadline: {} pending records; queued records will be discarded",
                        status.pending
                    ));
                }
                return status;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> Command {
        Command::Sample(Box::new(FineTuneSample::new(
            UtcTimestamp::now(),
            "mita",
            None,
            vec![],
            "fixture",
        )))
    }
    #[tokio::test]
    async fn full_queue_and_stalled_writer_have_bounded_drain_and_accounted_drops() {
        let (entered, enter) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let delivery = Delivery::start_writer(CancellationToken::new(), move |_| {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(())
        })
        .unwrap();
        delivery.submit(command()).unwrap();
        enter.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..DELIVERY_CAPACITY {
            delivery.submit(command()).unwrap();
        }
        assert!(delivery.submit(command()).is_err());
        let status = delivery.status();
        assert_eq!(status.pending, DELIVERY_CAPACITY + 1);
        assert_eq!(status.dropped, 1);
        let status = delivery.drain(Duration::from_millis(10), true).await;
        assert!(status.closed);
        assert_eq!(status.pending, DELIVERY_CAPACITY + 1);
        assert!(delivery.submit(command()).is_err());
        release.send(()).unwrap();
        let status = delivery.drain(Duration::from_secs(2), true).await;
        assert_eq!(status.pending, 0);
        assert_eq!(status.completed, 1);
        assert_eq!(status.dropped, DELIVERY_CAPACITY as u64 + 2);
        assert_eq!(
            status.admitted + status.rejected,
            status.completed + status.errors + status.dropped + status.pending as u64
        );
    }
    #[tokio::test]
    async fn writer_errors_are_observable_and_do_not_stop_later_delivery() {
        let delivery = Delivery::start_writer(CancellationToken::new(), |_| {
            Err(FineTuneError::SettingsInvalid(
                "controlled storage error".into(),
            ))
        })
        .unwrap();
        delivery.submit(command()).unwrap();
        delivery.submit(command()).unwrap();
        let status = delivery.drain(Duration::from_secs(2), true).await;
        assert_eq!(status.errors, 2);
        assert_eq!(status.pending, 0);
        assert!(
            status
                .last_error
                .unwrap()
                .contains("controlled storage error")
        );
    }
}
