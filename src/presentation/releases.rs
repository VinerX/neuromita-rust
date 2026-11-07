use std::{convert::Infallible, sync::Arc};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    lifecycle::Lifecycle,
    releases::{ReleaseFeed, ReleaseRecord},
};

#[derive(Debug, Clone, Default)]
pub enum ReleaseLoadState {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(Arc<str>),
}

#[derive(Debug, Clone, Default)]
pub struct ReleasesSnapshot {
    pub closed: bool,
    pub state: ReleaseLoadState,
    pub records: Arc<[ReleaseRecord]>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseCommandError {
    #[error("release refresh is already queued")]
    Busy,
    #[error("release feed has stopped")]
    Stopped,
}

#[derive(Clone)]
pub struct ReleasesPresenter {
    commands: mpsc::Sender<()>,
    updates: watch::Receiver<Arc<ReleasesSnapshot>>,
}

impl ReleasesPresenter {
    pub fn new(feed: Arc<dyn ReleaseFeed>) -> (Self, ReleasesLifecycle) {
        let (commands, requests) = mpsc::channel(1);
        let (updates, receiver) = watch::channel(Arc::new(ReleasesSnapshot::default()));
        (
            Self {
                commands,
                updates: receiver,
            },
            ReleasesLifecycle {
                feed,
                requests,
                updates,
            },
        )
    }

    pub fn refresh(&self) -> Result<(), ReleaseCommandError> {
        self.commands.try_send(()).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ReleaseCommandError::Busy,
            mpsc::error::TrySendError::Closed(_) => ReleaseCommandError::Stopped,
        })
    }

    pub fn snapshot(&self) -> Arc<ReleasesSnapshot> {
        self.updates.borrow().clone()
    }
    pub fn observe(&self) -> watch::Receiver<Arc<ReleasesSnapshot>> {
        self.updates.clone()
    }
}

pub struct ReleasesLifecycle {
    feed: Arc<dyn ReleaseFeed>,
    requests: mpsc::Receiver<()>,
    updates: watch::Sender<Arc<ReleasesSnapshot>>,
}

impl Lifecycle for ReleasesLifecycle {
    type Error = Infallible;

    async fn run(mut self, stop: CancellationToken) -> Result<(), Infallible> {
        let mut snapshot = ReleasesSnapshot::default();
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                request = self.requests.recv() => if request.is_none() { break; },
            }
            snapshot.state = ReleaseLoadState::Loading;
            self.updates.send_replace(Arc::new(snapshot.clone()));
            let result = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                result = self.feed.fetch() => result,
            };
            match result {
                Ok(records) => {
                    snapshot.records = records.into();
                    snapshot.state = ReleaseLoadState::Ready;
                }
                Err(error) => snapshot.state = ReleaseLoadState::Failed(error.into()),
            }
            self.updates.send_replace(Arc::new(snapshot.clone()));
        }
        self.requests.close();
        snapshot.closed = true;
        self.updates.send_replace(Arc::new(snapshot));
        Ok(())
    }
}
