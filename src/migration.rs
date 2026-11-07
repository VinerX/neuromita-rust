mod service;
mod startup;

pub use service::MigrationService;
pub use startup::{MigrationJob, StartupMigration, StartupMigrationError, StartupMigrations};

use thiserror::Error;

/// Converts a module's persisted or external input before it is published.
/// The caller owns startup ordering, persistence and atomic application.
pub trait Migration<Input> {
    type Output;
    type Error;

    fn migrate(&self, input: Input) -> Result<Self::Output, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedData<Data> {
    pub version: u32,
    pub data: Data,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum MigrationError {
    #[error("invalid migration plan: {0}")]
    InvalidPlan(String),
    #[error("unsupported schema {found}; current schema is {current}")]
    UnsupportedVersion { found: u32, current: u32 },
    #[error("missing migration from schema {from} toward {current}")]
    MissingStep { from: u32, current: u32 },
    #[error("invalid migration data: {0}")]
    InvalidData(String),
    #[error("migration {from} -> {to} failed: {source}")]
    StepFailed {
        from: u32,
        to: u32,
        source: Box<MigrationError>,
    },
    #[error("migration storage: {0}")]
    Storage(String),
    #[error("migration storage changed since it was read")]
    Conflict,
}

pub trait MigrationSchema<Data>: Send + Sync {
    fn current_version(&self) -> u32;
    fn validate(&self, value: &VersionedData<Data>) -> Result<(), MigrationError>;
}

/// A deterministic data transformation without external side effects. An
/// uncommitted step can run again after failure or process interruption.
pub trait MigrationStep<Data>: Send + Sync {
    fn source_version(&self) -> u32;
    fn target_version(&self) -> u32;
    fn migrate(&self, data: Data) -> Result<Data, MigrationError>;
}

/// All writers must participate in the store's exclusion mechanism. Commit
/// compares the complete expected snapshot and atomically replaces data and
/// schema version together. Failure leaves the previous snapshot intact.
pub trait MigrationStore<Data>: Send + Sync {
    fn read_versioned(&self) -> Result<VersionedData<Data>, MigrationError>;
    fn commit(
        &self,
        expected: &VersionedData<Data>,
        next: &VersionedData<Data>,
    ) -> Result<(), MigrationError>;
}
