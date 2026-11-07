use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{FineTuneError, Retention};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FineTuneSettings {
    pub schema_version: u32,
    pub data_dir: PathBuf,
    pub enabled: bool,
    pub retention: Retention,
}

impl FineTuneSettings {
    pub fn new(
        data_dir: PathBuf,
        enabled: bool,
        retention: Retention,
    ) -> Result<Self, FineTuneError> {
        let settings = Self {
            schema_version: 1,
            data_dir,
            enabled,
            retention,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), FineTuneError> {
        if self.schema_version != 1 {
            return Err(FineTuneError::SettingsInvalid(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        if !self.data_dir.is_absolute() {
            return Err(FineTuneError::RelativeDirectory(self.data_dir.clone()));
        }
        if let Retention::Limited { max } = self.retention {
            Retention::limited(max)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, FineTuneError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(FineTuneError::SettingsNotFound(path.to_path_buf()));
            }
            Err(error) => return Err(io_error(path, error)),
        };
        let text = String::from_utf8(bytes).map_err(|error| {
            FineTuneError::SettingsCorrupt(format!("{}: {error}", path.display()))
        })?;
        let settings: Self = serde_json::from_str(&text).map_err(|error| {
            FineTuneError::SettingsCorrupt(format!("{}: {error}", path.display()))
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Creates a new settings file without replacing an existing one.
    pub fn create_new(path: &Path, settings: &Self) -> Result<(), FineTuneError> {
        settings.validate()?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| {
            FineTuneError::SettingsInvalid(format!("cannot encode settings: {error}"))
        })?;
        publish_new(path, &bytes)
    }

    /// Replaces an existing settings file atomically. The caller must load and
    /// validate the file first, so corrupt settings are never replaced blindly.
    pub fn save(&self, path: &Path) -> Result<(), FineTuneError> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            FineTuneError::SettingsInvalid(format!("cannot encode settings: {error}"))
        })?;
        replace_atomically(path, &bytes)
    }

    pub fn set_data_dir(&mut self, data_dir: PathBuf) -> Result<(), FineTuneError> {
        if !data_dir.is_absolute() {
            return Err(FineTuneError::RelativeDirectory(data_dir));
        }
        self.data_dir = data_dir;
        Ok(())
    }
}

fn io_error(path: &Path, source: io::Error) -> FineTuneError {
    FineTuneError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn parent(path: &Path) -> Result<&Path, FineTuneError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.ok_or_else(|| {
        FineTuneError::SettingsInvalid(format!(
            "settings path has no parent directory: {}",
            path.display()
        ))
    })?;
    if !parent.is_dir() {
        return Err(FineTuneError::NotDirectory(parent.to_path_buf()));
    }
    Ok(parent)
}

fn temporary_path(directory: &Path, stem: &str) -> PathBuf {
    directory.join(format!(".{stem}.{}.tmp", Uuid::new_v4()))
}

fn write_temporary(path: &Path, bytes: &[u8]) -> Result<(), FineTuneError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(io_error(path, error));
    }
    Ok(())
}

fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), FineTuneError> {
    let directory = parent(path)?;
    let temporary = temporary_path(directory, "finetune-settings");
    write_temporary(&temporary, bytes)?;
    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            let _ = fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if error.kind() == io::ErrorKind::AlreadyExists {
                Err(FineTuneError::SettingsInvalid(format!(
                    "settings file already exists: {}",
                    path.display()
                )))
            } else {
                Err(io_error(path, error))
            }
        }
    }
}

fn replace_atomically(path: &Path, bytes: &[u8]) -> Result<(), FineTuneError> {
    let directory = parent(path)?;
    let temporary = temporary_path(directory, "finetune-settings");
    write_temporary(&temporary, bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(io_error(path, error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_and_absolute_directory_are_validated() {
        assert!(Retention::limited(0).is_err());
        assert!(Retention::limited(1_000_001).is_err());
        assert!(Retention::limited(50).is_ok());
        assert!(
            FineTuneSettings::new(PathBuf::from("relative"), false, Retention::Unlimited).is_err()
        );
        assert!(
            FineTuneSettings::new(
                std::env::temp_dir().join("neuromita-finetune-settings"),
                false,
                Retention::Unlimited,
            )
            .is_ok()
        );
    }
}
