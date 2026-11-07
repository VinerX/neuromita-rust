use std::sync::Arc;

use neuromita::{
    lifecycle::Lifecycle,
    presentation::{ReleaseCommandError, ReleaseLoadState, ReleasesPresenter},
    releases::{ReleaseFeed, ReleaseFuture},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

struct PendingFeed(Semaphore);
impl ReleaseFeed for PendingFeed {
    fn fetch(&self) -> ReleaseFuture<'_> {
        Box::pin(async move {
            let _permit = self.0.acquire().await.unwrap();
            Ok(Vec::new())
        })
    }
}

#[tokio::test]
async fn release_work_is_bounded_and_shutdown_cancels_inflight_fetch() {
    let (presenter, lifecycle) = ReleasesPresenter::new(Arc::new(PendingFeed(Semaphore::new(0))));
    let stop = CancellationToken::new();
    let task = tokio::spawn(lifecycle.run(stop.clone()));
    let mut updates = presenter.observe();
    presenter.refresh().unwrap();
    loop {
        if matches!(updates.borrow_and_update().state, ReleaseLoadState::Loading) {
            break;
        }
        updates.changed().await.unwrap();
    }
    presenter.refresh().unwrap();
    assert!(matches!(
        presenter.refresh(),
        Err(ReleaseCommandError::Busy)
    ));
    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(presenter.snapshot().closed);
    assert!(matches!(
        presenter.refresh(),
        Err(ReleaseCommandError::Stopped)
    ));
}
