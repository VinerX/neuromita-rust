use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{LauncherError, classify_io};

pub const LAUNCHER_CONFIG_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherConfig {
    pub schema_version: u32,
    pub install_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<PathBuf>,
}

impl LauncherConfig {
    pub fn new(install_root: impl Into<PathBuf>) -> Self {
        Self {
            schema_version: LAUNCHER_CONFIG_SCHEMA,
            install_root: install_root.into(),
            candidate: None,
        }
    }

    pub fn with_candidate(mut self, candidate: impl Into<PathBuf>) -> Self {
        self.candidate = Some(candidate.into());
        self
    }

    pub fn validate(&self, path: &Path) -> Result<(), LauncherError> {
        if self.install_root.as_os_str().is_empty() {
            return Err(LauncherError::Config {
                path: path.to_path_buf(),
                message: "install_root must not be empty".into(),
            });
        }
        if self.schema_version != LAUNCHER_CONFIG_SCHEMA {
            return Err(LauncherError::UnsupportedConfigSchema {
                path: path.to_path_buf(),
                schema: self.schema_version,
            });
        }
        Ok(())
    }
}

pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LauncherConfig, LauncherError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LauncherError::MissingConfig(self.path.clone()));
            }
            Err(error) => {
                return Err(classify_io("reading launcher config", &self.path, error));
            }
        };
        let config: LauncherConfig =
            serde_json::from_slice(&bytes).map_err(|error| LauncherError::Config {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        config.validate(&self.path)?;
        Ok(config)
    }

    pub fn save(&self, config: &LauncherConfig) -> Result<(), LauncherError> {
        config.validate(&self.path)?;
        let bytes = serde_json::to_vec_pretty(config).map_err(|error| LauncherError::Config {
            path: self.path.clone(),
            message: error.to_string(),
        })?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| classify_io("writing launcher config", &self.path, error))
    }
}
