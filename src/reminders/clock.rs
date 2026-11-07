use std::{
    future::Future,
    pin::Pin,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::Notify;

use super::time::UtcInstant;

pub type ClockFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub trait Clock: Send + Sync {
    fn now(&self) -> UtcInstant;
    fn sleep_until(&self, deadline: UtcInstant) -> ClockFuture<'_>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcInstant {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros())
            .unwrap_or(0);
        UtcInstant::from_unix_micros(i64::try_from(micros).unwrap_or(i64::MAX))
    }
    fn sleep_until(&self, deadline: UtcInstant) -> ClockFuture<'_> {
        Box::pin(async move {
            let now = self.now();
            if deadline > now {
                tokio::time::sleep(deadline.duration_since(now)).await;
            }
        })
    }
}

/// Explicitly advanced clock for deterministic scheduler and DST tests.
pub struct ManualClock {
    now: Mutex<UtcInstant>,
    changed: Notify,
}

impl ManualClock {
    pub fn new(now: UtcInstant) -> Self {
        Self {
            now: Mutex::new(now),
            changed: Notify::new(),
        }
    }
    pub fn set(&self, now: UtcInstant) {
        *self.now.lock().expect("manual clock poisoned") = now;
        self.changed.notify_waiters();
    }
    pub fn advance(&self, duration: Duration) {
        let next = self
            .now
            .lock()
            .expect("manual clock poisoned")
            .checked_add(duration)
            .expect("manual clock overflow");
        self.set(next);
    }
    pub fn current(&self) -> UtcInstant {
        *self.now.lock().expect("manual clock poisoned")
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UtcInstant {
        self.current()
    }
    fn sleep_until(&self, deadline: UtcInstant) -> ClockFuture<'_> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.now() >= deadline {
                    return;
                }
                changed.await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_clock_wakes_sleepers() {
        let clock = ManualClock::new(UtcInstant::from_unix_seconds(100));
        let wait = clock.sleep_until(UtcInstant::from_unix_seconds(110));
        let advance = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            clock.advance(Duration::from_secs(10));
        };
        tokio::join!(wait, advance);
        assert_eq!(clock.now(), UtcInstant::from_unix_seconds(110));
    }
}
