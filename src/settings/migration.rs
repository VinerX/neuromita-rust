use super::{SettingsImportFormat, SettingsRepository, SettingsTransferError, StoredSettings};
use crate::migration::{Migration, MigrationError, StartupMigration};
use std::sync::Arc;

/// Strict current Rust settings decoder, not a historical upgrade chain.
pub struct RustSettingsMigration;
impl Migration<&[u8]> for RustSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let document: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| SettingsTransferError::MalformedJson(e.to_string()))?;
        let object = document.as_object().ok_or_else(|| {
            SettingsTransferError::NotFullSettingsTransfer("expected an envelope object".into())
        })?;
        for field in ["revision", "settings"] {
            if !object.contains_key(field) {
                return Err(SettingsTransferError::NotFullSettingsTransfer(format!(
                    "missing {field}"
                )));
            }
        }
        let stored: StoredSettings = serde_json::from_slice(bytes)
            .map_err(|e| SettingsTransferError::InvalidEnvelope(e.to_string()))?;
        stored
            .settings
            .validate()
            .map_err(|e| SettingsTransferError::InvalidValue(e.to_string()))?;
        Ok(stored)
    }
}

pub struct PythonSettingsMigration;
impl Migration<&[u8]> for PythonSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, _bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        Err(SettingsTransferError::NotImplemented {
            format: SettingsImportFormat::Python,
        })
    }
}

/// Keep the generic startup migration slot, but only validate current settings.
pub(crate) struct SettingsStartupMigration(pub Arc<dyn SettingsRepository>);
impl StartupMigration for SettingsStartupMigration {
    fn name(&self) -> &str {
        "settings"
    }
    fn run(&self) -> Result<(), MigrationError> {
        self.0
            .migrate()
            .map_err(|e| MigrationError::Storage(e.to_string()))
    }
}
