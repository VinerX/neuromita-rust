use std::sync::Arc;

use thiserror::Error;

use super::{MigrationError, MigrationService, MigrationStore};

pub trait StartupMigration: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self) -> Result<(), MigrationError>;
}

pub struct MigrationJob<Data> {
    pub(super) name: String,
    pub(super) service: MigrationService<Data>,
    pub(super) store: Arc<dyn MigrationStore<Data>>,
}

impl<Data: Clone + Send + Sync> StartupMigration for MigrationJob<Data> {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self) -> Result<(), MigrationError> {
        self.service.run(&*self.store).map(|_| ())
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("startup migration {module} failed: {source}")]
pub struct StartupMigrationError {
    pub module: String,
    #[source]
    pub source: MigrationError,
}

#[derive(Default)]
pub struct StartupMigrations {
    jobs: Vec<Box<dyn StartupMigration>>,
}

impl StartupMigrations {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, job: impl StartupMigration + 'static) -> Result<(), MigrationError> {
        if job.name().trim().is_empty()
            || self
                .jobs
                .iter()
                .any(|registered| registered.name() == job.name())
        {
            return Err(MigrationError::InvalidPlan(format!(
                "empty or duplicate startup module: {}",
                job.name()
            )));
        }
        self.jobs.push(Box::new(job));
        Ok(())
    }

    pub fn append(&mut self, other: Self) -> Result<(), MigrationError> {
        if let Some(duplicate) = other.jobs.iter().find(|job| {
            self.jobs
                .iter()
                .any(|registered| registered.name() == job.name())
        }) {
            return Err(MigrationError::InvalidPlan(format!(
                "duplicate startup module: {}",
                duplicate.name()
            )));
        }
        self.jobs.extend(other.jobs);
        Ok(())
    }

    /// Registration order is execution order. Completed modules remain
    /// committed if a later one fails; there is no cross-store transaction.
    pub fn run(&self) -> Result<(), StartupMigrationError> {
        for job in &self.jobs {
            tracing::debug!(module = job.name(), "startup migration started");
            job.run().map_err(|source| StartupMigrationError {
                module: job.name().to_owned(),
                source,
            })?;
            tracing::debug!(module = job.name(), "startup migration complete");
        }
        Ok(())
    }
}
