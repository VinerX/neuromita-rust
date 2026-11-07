use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

use neuromita::migration::{
    MigrationError, MigrationSchema, MigrationService, MigrationStep, MigrationStore,
    StartupMigration, StartupMigrations, VersionedData,
};

struct Store<Data> {
    value: Mutex<VersionedData<Data>>,
    commits: Mutex<Vec<u32>>,
    fail_at: AtomicU32,
}

impl<Data> Store<Data> {
    fn new(version: u32, data: Data) -> Self {
        Self {
            value: Mutex::new(VersionedData { version, data }),
            commits: Mutex::new(Vec::new()),
            fail_at: AtomicU32::new(0),
        }
    }
}

impl<Data: Clone + PartialEq + Send> MigrationStore<Data> for Store<Data> {
    fn read_versioned(&self) -> Result<VersionedData<Data>, MigrationError> {
        Ok(self.value.lock().unwrap().clone())
    }
    fn commit(
        &self,
        expected: &VersionedData<Data>,
        next: &VersionedData<Data>,
    ) -> Result<(), MigrationError> {
        let mut value = self.value.lock().unwrap();
        if *value != *expected {
            return Err(MigrationError::Conflict);
        }
        if self.fail_at.load(Ordering::SeqCst) == next.version {
            return Err(MigrationError::Storage("injected commit failure".into()));
        }
        *value = next.clone();
        self.commits.lock().unwrap().push(next.version);
        Ok(())
    }
}

struct NumberSchema(u32);
impl MigrationSchema<u64> for NumberSchema {
    fn current_version(&self) -> u32 {
        self.0
    }
    fn validate(&self, value: &VersionedData<u64>) -> Result<(), MigrationError> {
        if value.data == u64::MAX {
            Err(MigrationError::InvalidData("invalid number".into()))
        } else {
            Ok(())
        }
    }
}

struct Step {
    from: u32,
    to: u32,
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    invalid: bool,
}

impl Step {
    fn new(from: u32, to: u32) -> Self {
        Self {
            from,
            to,
            calls: Arc::new(AtomicUsize::new(0)),
            fail: Arc::new(AtomicBool::new(false)),
            invalid: false,
        }
    }
}

impl MigrationStep<u64> for Step {
    fn source_version(&self) -> u32 {
        self.from
    }
    fn target_version(&self) -> u32 {
        self.to
    }
    fn migrate(&self, data: u64) -> Result<u64, MigrationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(MigrationError::InvalidData("injected step failure".into()));
        }
        Ok(if self.invalid { u64::MAX } else { data + 10 })
    }
}

fn service() -> MigrationService<u64> {
    MigrationService::new(
        NumberSchema(3),
        vec![Box::new(Step::new(1, 2)), Box::new(Step::new(2, 3))],
    )
    .unwrap()
}

#[test]
fn prepare_and_persist_share_a_chain_and_restart_is_a_noop() {
    let store = Store::new(1, 5);
    let service = service();
    let original = store.read_versioned().unwrap();
    let prepared = service.prepare(original.clone()).unwrap();
    assert_eq!(
        prepared,
        VersionedData {
            version: 3,
            data: 25
        }
    );
    assert_eq!(store.read_versioned().unwrap(), original);
    assert_eq!(service.run(&store).unwrap(), prepared);
    assert_eq!(*store.commits.lock().unwrap(), [2, 3]);
    assert_eq!(service.run(&store).unwrap(), prepared);
    assert_eq!(*store.commits.lock().unwrap(), [2, 3]);
}

#[test]
fn missing_paths_and_future_schemas_fail_before_any_step_or_write() {
    let step = Step::new(1, 2);
    let calls = Arc::clone(&step.calls);
    let service = MigrationService::new(NumberSchema(3), vec![Box::new(step)]).unwrap();
    let store = Store::new(1, 5);
    assert!(matches!(
        service.run(&store),
        Err(MigrationError::MissingStep {
            from: 2,
            current: 3
        })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(store.commits.lock().unwrap().is_empty());
    let future = Store::new(4, 5);
    assert!(matches!(
        service.run(&future),
        Err(MigrationError::UnsupportedVersion {
            found: 4,
            current: 3
        })
    ));
    assert!(future.commits.lock().unwrap().is_empty());
}

#[test]
fn ambiguous_backwards_and_out_of_range_plans_are_rejected() {
    for steps in [
        vec![Step::new(1, 2), Step::new(1, 3)],
        vec![Step::new(2, 1)],
        vec![Step::new(1, 1)],
        vec![Step::new(1, 4)],
    ] {
        assert!(matches!(
            MigrationService::new(
                NumberSchema(3),
                steps
                    .into_iter()
                    .map(|step| Box::new(step) as Box<dyn MigrationStep<u64>>)
                    .collect()
            ),
            Err(MigrationError::InvalidPlan(_))
        ));
    }
}

#[test]
fn step_failure_resumes_from_the_last_committed_version() {
    let first = Step::new(1, 2);
    let first_calls = Arc::clone(&first.calls);
    let second = Step::new(2, 3);
    let fail = Arc::clone(&second.fail);
    fail.store(true, Ordering::SeqCst);
    let service =
        MigrationService::new(NumberSchema(3), vec![Box::new(first), Box::new(second)]).unwrap();
    let store = Store::new(1, 5);
    assert!(matches!(
        service.run(&store),
        Err(MigrationError::StepFailed { from: 2, to: 3, .. })
    ));
    assert_eq!(
        store.read_versioned().unwrap(),
        VersionedData {
            version: 2,
            data: 15
        }
    );
    fail.store(false, Ordering::SeqCst);
    assert_eq!(
        service.run(&store).unwrap(),
        VersionedData {
            version: 3,
            data: 25
        }
    );
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*store.commits.lock().unwrap(), [2, 3]);
}

#[test]
fn failed_commits_and_invalid_outputs_never_advance_the_stored_version() {
    let store = Store::new(1, 5);
    store.fail_at.store(3, Ordering::SeqCst);
    assert!(matches!(
        service().run(&store),
        Err(MigrationError::Storage(_))
    ));
    assert_eq!(
        store.read_versioned().unwrap(),
        VersionedData {
            version: 2,
            data: 15
        }
    );
    store.fail_at.store(0, Ordering::SeqCst);
    assert_eq!(
        service().run(&store).unwrap(),
        VersionedData {
            version: 3,
            data: 25
        }
    );
    let mut invalid = Step::new(1, 2);
    invalid.invalid = true;
    let service = MigrationService::new(NumberSchema(2), vec![Box::new(invalid)]).unwrap();
    let store = Store::new(1, 5);
    assert!(matches!(
        service.run(&store),
        Err(MigrationError::InvalidData(_))
    ));
    assert_eq!(store.read_versioned().unwrap().version, 1);
    assert!(store.commits.lock().unwrap().is_empty());
}

struct TextSchema;
impl MigrationSchema<String> for TextSchema {
    fn current_version(&self) -> u32 {
        2
    }
    fn validate(&self, value: &VersionedData<String>) -> Result<(), MigrationError> {
        if value.data.is_empty() {
            Err(MigrationError::InvalidData("empty text".into()))
        } else {
            Ok(())
        }
    }
}
struct TextStep;
impl MigrationStep<String> for TextStep {
    fn source_version(&self) -> u32 {
        1
    }
    fn target_version(&self) -> u32 {
        2
    }
    fn migrate(&self, data: String) -> Result<String, MigrationError> {
        Ok(format!("{data}!"))
    }
}

#[test]
fn unrelated_modules_register_their_own_data_without_changes_to_the_runner() {
    let numbers = Arc::new(Store::new(1, 5));
    let text = Arc::new(Store::new(1, "module".to_owned()));
    let mut startup = StartupMigrations::new();
    startup
        .register(service().into_startup("numbers", numbers.clone()))
        .unwrap();
    startup
        .register(
            MigrationService::new(TextSchema, vec![Box::new(TextStep)])
                .unwrap()
                .into_startup("text", text.clone()),
        )
        .unwrap();
    startup.run().unwrap();
    assert_eq!(
        numbers.read_versioned().unwrap(),
        VersionedData {
            version: 3,
            data: 25
        }
    );
    assert_eq!(
        text.read_versioned().unwrap(),
        VersionedData {
            version: 2,
            data: "module!".into()
        }
    );
}

struct Job {
    name: &'static str,
    fail: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}
impl StartupMigration for Job {
    fn name(&self) -> &str {
        self.name
    }
    fn run(&self) -> Result<(), MigrationError> {
        self.calls.lock().unwrap().push(self.name);
        if self.fail.load(Ordering::SeqCst) {
            Err(MigrationError::Storage("failed module".into()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn startup_is_ordered_stops_on_failure_and_rejects_duplicate_modules() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let failure = Arc::new(AtomicBool::new(true));
    let job = |name, fail| Job {
        name,
        fail,
        calls: Arc::clone(&calls),
    };
    let mut startup = StartupMigrations::new();
    startup
        .register(job("first", Arc::new(AtomicBool::new(false))))
        .unwrap();
    startup.register(job("second", failure.clone())).unwrap();
    startup
        .register(job("third", Arc::new(AtomicBool::new(false))))
        .unwrap();
    assert!(startup.register(job("first", failure.clone())).is_err());
    assert!(startup.register(job("", failure.clone())).is_err());
    let error = startup.run().unwrap_err();
    assert_eq!(error.module, "second");
    assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
    failure.store(false, Ordering::SeqCst);
    startup.run().unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        ["first", "second", "first", "second", "third"]
    );
}
