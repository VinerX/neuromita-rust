mod migration;
mod model;
mod speech;
mod storage;
mod transfer;

pub use crate::generation::PresetId;
pub(crate) use migration::SettingsStartupMigration;
pub use migration::{PythonSettingsMigration, RustSettingsMigration};
pub use model::*;
pub use speech::*;
pub use storage::*;
pub use transfer::{SettingsImportFormat, SettingsTransfer, SettingsTransferError};

use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::lifecycle::Lifecycle;

/// Static settings entry point; its runtime-bound methods are implemented in `crate::api`.
pub struct SettingsApi;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("settings are not initialized")]
    Uninitialized,
    #[error("settings are stopping")]
    Stopped,
    #[error("settings conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("invalid settings: {0}")]
    Validation(String),
    #[error("settings storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceState {
    Pending,
    Saved(u64),
    Failed { revision: u64, error: SettingsError },
}

#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    pub closed: bool,
    pub revision: u64,
    pub values: AppSettings,
    pub persisted_revision: Option<u64>,
    pub persistence: PersistenceState,
}

pub enum SettingsSource {
    Initial(Box<AppSettings>),
    Stored,
}
impl SettingsSource {
    pub fn initial(settings: AppSettings) -> Self {
        Self::Initial(Box::new(settings))
    }
}

pub struct SettingsConfig {
    pub source: SettingsSource,
    pub repository: Arc<dyn SettingsRepository>,
}

struct State {
    snapshot: Arc<SettingsSnapshot>,
    closed: bool,
    save_generation: u64,
}
pub(crate) struct SettingsOwner {
    state: Mutex<State>,
    updates: watch::Sender<Arc<SettingsSnapshot>>,
    save: Notify,
}

impl SettingsOwner {
    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<SettingsSnapshot>> {
        self.updates.subscribe()
    }

    pub(crate) fn snapshot<T: SettingsSection>(&self) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.snapshot_all()?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    pub(crate) fn snapshot_all(&self) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        let state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        Ok(Arc::clone(&state.snapshot))
    }
    pub(crate) fn observe(&self) -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        let state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        Ok(self.updates.subscribe())
    }

    pub(crate) fn export(&self) -> Result<SettingsTransfer, SettingsError> {
        let snapshot = self.snapshot_all()?;
        Ok(SettingsTransfer::new(StoredSettings {
            revision: snapshot.revision,
            settings: snapshot.values.clone(),
        })?)
    }

    pub(crate) fn import(
        &self,
        expected_revision: u64,
        transfer: SettingsTransfer,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.change(Some(expected_revision), move |settings| {
            *settings = transfer.into_stored().settings;
        })
    }
    pub(crate) fn replace<T: SettingsSection>(
        &self,
        expected_revision: u64,
        value: T,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.change(Some(expected_revision), move |settings| {
            value.replace(settings)
        })?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    /// The edit must be short and must not block or re-enter SettingsApi.
    pub(crate) fn edit<T: SettingsSection>(
        &self,
        edit: impl FnOnce(&mut T),
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.change(None, move |settings| {
            let mut value = T::read(settings);
            edit(&mut value);
            value.replace(settings);
        })?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    pub(crate) async fn flush(&self) -> Result<(), SettingsError> {
        let mut updates = self.observe()?;
        let target = updates.borrow().revision;
        self.save.notify_one();
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            if snapshot
                .persisted_revision
                .is_some_and(|revision| revision >= target)
            {
                return Ok(());
            }
            if let PersistenceState::Failed { revision, error } = &snapshot.persistence
                && *revision >= target
            {
                return Err(error.clone());
            }
            if snapshot.closed {
                return Err(SettingsError::Stopped);
            }
            updates
                .changed()
                .await
                .map_err(|_| SettingsError::Stopped)?;
        }
    }
}

impl SettingsOwner {
    pub(crate) fn request_save(&self) -> Result<(), SettingsError> {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        state.save_generation = state
            .save_generation
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("save generation exhausted".into()))?;
        let mut snapshot = (*state.snapshot).clone();
        snapshot.persistence = PersistenceState::Pending;
        state.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&state.snapshot));
        self.save.notify_one();
        Ok(())
    }
    fn change(
        &self,
        expected: Option<u64>,
        change: impl FnOnce(&mut AppSettings),
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        if let Some(expected) = expected
            && expected != state.snapshot.revision
        {
            return Err(SettingsError::Conflict {
                expected,
                actual: state.snapshot.revision,
            });
        }
        let mut values = state.snapshot.values.clone();
        change(&mut values);
        values.validate()?;
        if values == state.snapshot.values {
            return Ok(Arc::clone(&state.snapshot));
        }
        let revision = state
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("revision exhausted".into()))?;
        let snapshot = Arc::new(SettingsSnapshot {
            closed: false,
            revision,
            values,
            persisted_revision: state.snapshot.persisted_revision,
            persistence: PersistenceState::Pending,
        });
        state.save_generation = state
            .save_generation
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("save generation exhausted".into()))?;
        state.snapshot = Arc::clone(&snapshot);
        self.updates.send_replace(Arc::clone(&snapshot));
        self.save.notify_one();
        Ok(snapshot)
    }
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        state.closed = true;
        let mut snapshot = (*state.snapshot).clone();
        snapshot.closed = true;
        state.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&state.snapshot));
    }
}

pub struct SettingsLifecycle {
    owner: Arc<SettingsOwner>,
    repository: Arc<dyn SettingsRepository>,
}

impl SettingsLifecycle {
    pub(crate) async fn prepare(config: SettingsConfig) -> Result<Self, SettingsError> {
        let stored = match config.source {
            SettingsSource::Initial(settings) => {
                let settings = *settings;
                settings.validate()?;
                StoredSettings {
                    revision: 0,
                    settings,
                }
            }
            SettingsSource::Stored => {
                let repository = Arc::clone(&config.repository);
                tokio::task::spawn_blocking(move || repository.load())
                    .await
                    .map_err(|error| SettingsError::Storage(error.to_string()))??
            }
        };
        stored.settings.validate()?;
        let snapshot = Arc::new(SettingsSnapshot {
            closed: false,
            revision: stored.revision,
            values: stored.settings,
            persisted_revision: None,
            persistence: PersistenceState::Pending,
        });
        let (updates, _) = watch::channel(Arc::clone(&snapshot));
        Ok(Self {
            owner: Arc::new(SettingsOwner {
                state: Mutex::new(State {
                    snapshot,
                    closed: false,
                    save_generation: 0,
                }),
                updates,
                save: Notify::new(),
            }),
            repository: config.repository,
        })
    }
    pub(crate) fn owner(&self) -> Arc<SettingsOwner> {
        Arc::clone(&self.owner)
    }

    async fn persist(&self) -> Result<(), SettingsError> {
        let (snapshot, generation) = {
            let state = self.owner.state.lock().expect("settings mutex poisoned");
            (Arc::clone(&state.snapshot), state.save_generation)
        };
        if snapshot.persisted_revision == Some(snapshot.revision) {
            return Ok(());
        }
        let stored = StoredSettings {
            revision: snapshot.revision,
            settings: snapshot.values.clone(),
        };
        let repository = Arc::clone(&self.repository);
        let result = tokio::task::spawn_blocking(move || repository.save(&stored))
            .await
            .map_err(|error| SettingsError::Storage(error.to_string()))
            .and_then(|result| result);
        let mut state = self.owner.state.lock().expect("settings mutex poisoned");
        if result.is_err() && state.save_generation != generation {
            return result;
        }
        let mut next = (*state.snapshot).clone();
        match &result {
            Ok(()) => {
                next.persisted_revision = Some(snapshot.revision);
                next.persistence = if next.revision == snapshot.revision {
                    PersistenceState::Saved(snapshot.revision)
                } else {
                    PersistenceState::Pending
                };
            }
            Err(error) => {
                next.persistence = PersistenceState::Failed {
                    revision: snapshot.revision,
                    error: error.clone(),
                };
            }
        }
        state.snapshot = Arc::new(next);
        self.owner.updates.send_replace(Arc::clone(&state.snapshot));
        result
    }
}

impl Lifecycle for SettingsLifecycle {
    type Error = SettingsError;
    async fn run(self, stop: CancellationToken) -> Result<(), Self::Error> {
        let _ = self.persist().await;
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => { self.owner.close(); return self.persist().await; }
                _ = self.owner.save.notified() => { let _ = self.persist().await; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use tokio::sync::oneshot;

    struct GatedFailure {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: Arc<Barrier>,
    }
    impl SettingsRepository for GatedFailure {
        fn migrate(&self) -> Result<(), SettingsError> {
            Err(SettingsError::Storage("not used".into()))
        }
        fn load(&self) -> Result<StoredSettings, SettingsError> {
            Err(SettingsError::Storage("not used".into()))
        }
        fn save(&self, _: &StoredSettings) -> Result<(), SettingsError> {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            self.release.wait();
            Err(SettingsError::Storage("old attempt failed".into()))
        }
    }

    #[tokio::test]
    async fn old_save_failure_cannot_overwrite_explicit_retry() {
        let (entered, started) = oneshot::channel();
        let release = Arc::new(Barrier::new(2));
        let repository = Arc::new(GatedFailure {
            entered: Mutex::new(Some(entered)),
            release: Arc::clone(&release),
        });
        let lifecycle = Arc::new(
            SettingsLifecycle::prepare(SettingsConfig {
                source: SettingsSource::initial(AppSettings::initial_profile()),
                repository,
            })
            .await
            .unwrap(),
        );
        let saving = Arc::clone(&lifecycle);
        let save = tokio::spawn(async move { saving.persist().await });
        started.await.unwrap();
        lifecycle.owner.request_save().unwrap();
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();
        assert!(save.await.unwrap().is_err());
        assert_eq!(
            lifecycle.owner.state.lock().unwrap().snapshot.persistence,
            PersistenceState::Pending
        );
    }
}
