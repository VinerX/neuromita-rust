use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{AppSettings, SettingsError, SettingsImportFormat, SettingsTransfer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSettings {
    pub revision: u64,
    pub settings: AppSettings,
}

pub trait SettingsRepository: Send + Sync + 'static {
    /// Validates the current document before publishing settings; does not migrate old formats.
    fn migrate(&self) -> Result<(), SettingsError>;
    fn load(&self) -> Result<StoredSettings, SettingsError>;
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError>;
}

pub struct MemorySettingsRepository {
    value: Mutex<Option<StoredSettings>>,
}
impl MemorySettingsRepository {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }
}
impl Default for MemorySettingsRepository {
    fn default() -> Self {
        Self::new()
    }
}
impl SettingsRepository for MemorySettingsRepository {
    fn migrate(&self) -> Result<(), SettingsError> {
        SettingsTransfer::new(self.load()?)?;
        Ok(())
    }
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        self.value
            .lock()
            .expect("settings storage poisoned")
            .clone()
            .ok_or_else(|| SettingsError::Storage("no stored settings".into()))
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        SettingsTransfer::new(settings.clone())?;
        *self.value.lock().expect("settings storage poisoned") = Some(settings.clone());
        Ok(())
    }
}

pub struct JsonSettingsRepository {
    path: PathBuf,
}
impl JsonSettingsRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn export_to(&self, destination: &Path) -> Result<SettingsTransfer, SettingsError> {
        let transfer = SettingsTransfer::new(self.load()?)?;
        atomic_write(destination, &transfer.encode()?)?;
        Ok(transfer)
    }

    /// Offline replacement; the application using this file must be stopped.
    pub fn import(&self, bytes: &[u8]) -> Result<SettingsTransfer, SettingsError> {
        self.import_from(SettingsImportFormat::Rust, bytes)
    }

    pub fn import_from(
        &self,
        format: SettingsImportFormat,
        bytes: &[u8],
    ) -> Result<SettingsTransfer, SettingsError> {
        let transfer = SettingsTransfer::prepare(format, bytes)?;
        atomic_write(&self.path, &transfer.encode()?)?;
        Ok(transfer)
    }
}
impl SettingsRepository for JsonSettingsRepository {
    fn migrate(&self) -> Result<(), SettingsError> {
        self.load().map(|_| ())
    }
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        let bytes =
            std::fs::read(&self.path).map_err(|error| SettingsError::Storage(error.to_string()))?;
        Ok(SettingsTransfer::decode(&bytes)?.into_stored())
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        let transfer = SettingsTransfer::new(settings.clone())?;
        atomic_write(&self.path, &transfer.encode()?)
    }
}

/// Serializes cooperating writers using a stable sidecar lock, then publishes
/// one complete file. Directory/power-loss durability is not promised.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let _lock = writer_lock(path)?;
    atomic_replace(path, bytes)
}

fn writer_lock(path: &Path) -> Result<std::fs::File, SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_file_name(lock_name))
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    file.lock()
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    Ok(file)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = path.with_file_name(temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| SettingsError::Storage(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_previous_dev_document_without_rewriting() {
        let path =
            std::env::temp_dir().join(format!("strict-settings-{}.json", uuid::Uuid::new_v4()));
        let original = serde_json::to_vec(&serde_json::json!({"schema_version":3,"revision":1,"settings":AppSettings::initial_profile()})).unwrap();
        std::fs::write(&path, &original).unwrap();
        let repo = JsonSettingsRepository::new(path.clone());
        assert!(repo.load().is_err());
        assert!(repo.migrate().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }
}
