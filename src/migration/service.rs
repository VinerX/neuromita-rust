use std::{collections::BTreeMap, sync::Arc};

use super::{
    MigrationError, MigrationJob, MigrationSchema, MigrationStep, MigrationStore, VersionedData,
};

pub struct MigrationService<Data> {
    current: u32,
    schema: Box<dyn MigrationSchema<Data>>,
    steps: BTreeMap<u32, RegisteredStep<Data>>,
}

struct RegisteredStep<Data> {
    to: u32,
    implementation: Box<dyn MigrationStep<Data>>,
}

impl<Data: Clone> MigrationService<Data> {
    pub fn new(
        schema: impl MigrationSchema<Data> + 'static,
        steps: Vec<Box<dyn MigrationStep<Data>>>,
    ) -> Result<Self, MigrationError> {
        let current = schema.current_version();
        let mut indexed = BTreeMap::new();
        for step in steps {
            let from = step.source_version();
            let to = step.target_version();
            if from >= to || to > current {
                return Err(MigrationError::InvalidPlan(format!(
                    "invalid step {from} -> {to} for current schema {current}"
                )));
            }
            if indexed
                .insert(
                    from,
                    RegisteredStep {
                        to,
                        implementation: step,
                    },
                )
                .is_some()
            {
                return Err(MigrationError::InvalidPlan(format!(
                    "multiple steps from schema {from}"
                )));
            }
        }
        Ok(Self {
            current,
            schema: Box::new(schema),
            steps: indexed,
        })
    }

    /// Pure preparation for imports; never accesses a MigrationStore.
    pub fn prepare(
        &self,
        mut value: VersionedData<Data>,
    ) -> Result<VersionedData<Data>, MigrationError> {
        let route = self.route(value.version)?;
        self.schema.validate(&value)?;
        for from in route {
            value = self.apply(value, &self.steps[&from])?;
        }
        Ok(value)
    }

    /// Commits one validated step at a time. A restart reads the last committed
    /// version; a failed or interrupted step is never recorded as completed.
    pub fn run(
        &self,
        store: &dyn MigrationStore<Data>,
    ) -> Result<VersionedData<Data>, MigrationError> {
        let mut value = store.read_versioned()?;
        let route = self.route(value.version)?;
        self.schema.validate(&value)?;
        for from in route {
            let next = self.apply(value.clone(), &self.steps[&from])?;
            store.commit(&value, &next)?;
            value = next;
        }
        Ok(value)
    }

    fn route(&self, mut from: u32) -> Result<Vec<u32>, MigrationError> {
        let current = self.current;
        if from > current {
            return Err(MigrationError::UnsupportedVersion {
                found: from,
                current,
            });
        }
        let mut route = Vec::new();
        while from != current {
            let step = self
                .steps
                .get(&from)
                .ok_or(MigrationError::MissingStep { from, current })?;
            route.push(from);
            from = step.to;
        }
        Ok(route)
    }

    fn apply(
        &self,
        value: VersionedData<Data>,
        step: &RegisteredStep<Data>,
    ) -> Result<VersionedData<Data>, MigrationError> {
        let from = value.version;
        let to = step.to;
        let data = step.implementation.migrate(value.data).map_err(|source| {
            MigrationError::StepFailed {
                from,
                to,
                source: Box::new(source),
            }
        })?;
        let next = VersionedData { version: to, data };
        self.schema.validate(&next)?;
        Ok(next)
    }
}

impl<Data: Clone + Send + Sync + 'static> MigrationService<Data> {
    pub fn into_startup(
        self,
        name: impl Into<String>,
        store: Arc<dyn MigrationStore<Data>>,
    ) -> MigrationJob<Data> {
        MigrationJob {
            name: name.into(),
            service: self,
            store,
        }
    }
}
