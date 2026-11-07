//! Explicit, backup-first administrative maintenance. Never invoked by generation or TTL.

use std::{fs, io, path::Path};

use thiserror::Error;

use crate::adapters::memory_backup::persist_exclusive_backup;
use crate::timeline::{
    SessionTimeline,
    domain::TimelineError,
    memory_purge::{MemoryPurgeBackup, MemoryPurgePreview},
};

#[derive(Debug, Error)]
pub(crate) enum SafePurgeError {
    #[error("memory purge requires explicit administrative confirmation")]
    ConfirmationRequired,
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error(transparent)]
    Backup(#[from] io::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
}

/// The caller must first obtain authorization and explicit user confirmation.
/// Backup must be placed in a trusted directory; it contains sensitive plaintext.
/// Purge affects the current projection, not historical append-only Timeline events.
pub(crate) fn purge_with_backup(
    timeline: &mut SessionTimeline,
    preview: &MemoryPurgePreview,
    confirmed: bool,
    destination: &Path,
) -> Result<usize, SafePurgeError> {
    if !confirmed {
        return Err(SafePurgeError::ConfirmationRequired);
    }
    timeline.validate_memory_purge(preview)?;
    let payload = serde_json::to_vec_pretty(&preview.backup())?;
    persist_exclusive_backup(destination, &payload)?;
    timeline.commit_memory_purge(preview).map_err(Into::into)
}

/// Admin-only recovery of exactly the soft-deleted snapshot preserved before purge.
/// Restoration never activates a memory; a later explicit Restore command is needed.
pub(crate) fn restore_from_backup(
    timeline: &mut SessionTimeline,
    confirmed: bool,
    source: &Path,
) -> Result<usize, SafePurgeError> {
    if !confirmed {
        return Err(SafePurgeError::ConfirmationRequired);
    }
    let backup: MemoryPurgeBackup = serde_json::from_slice(&fs::read(source)?)?;
    timeline.restore_memory_backup(&backup).map_err(Into::into)
}
