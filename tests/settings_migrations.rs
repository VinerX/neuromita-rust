use std::{fs, num::NonZeroUsize, path::PathBuf, process::Command, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationError, ApplicationLifecycle, RuntimeLimits},
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    migration::{Migration, MigrationError, StartupMigration},
    settings::{
        AppSettings, JsonSettingsRepository, Language, PresetId, PresetSelection,
        PythonSettingsMigration, RustSettingsMigration, SettingsApi, SettingsConfig, SettingsError,
        SettingsImportFormat, SettingsRepository, SettingsSource, SettingsTransfer,
        SettingsTransferError, StoredSettings, Volume,
    },
};
use serde_json::{Value, json};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-transfer-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(revision: u64) -> StoredSettings {
    let mut settings = AppSettings::initial_profile();
    settings.presets.active = PresetId(3);
    settings.characters.crazy.presets.react = PresetSelection::Specific(PresetId(7));
    settings.characters.kind.presets.reply = PresetSelection::Specific(PresetId(11));
    StoredSettings { revision, settings }
}

#[test]
fn strict_current_format_and_import_have_the_same_contract() {
    let directory = Directory::new();
    let path = directory.path("settings.json");
    let current = serde_json::to_vec(&record(9)).unwrap();
    fs::write(&path, &current).unwrap();
    let repository = JsonSettingsRepository::new(path.clone());
    repository.migrate().unwrap();
    let loaded = repository.load().unwrap();
    assert_eq!(loaded, record(9));
    assert_eq!(
        RustSettingsMigration.migrate(current.as_slice()).unwrap(),
        loaded
    );
    assert_eq!(
        SettingsApi::prepare_import(SettingsImportFormat::Rust, &current)
            .unwrap()
            .stored(),
        &loaded
    );
    assert_eq!(fs::read(&path).unwrap(), current);
    let old = serde_json::json!({"schema_version":3, "revision":9, "settings":record(9).settings});
    let original = serde_json::to_vec(&old).unwrap();
    fs::write(&path, &original).unwrap();
    assert!(repository.migrate().is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn python_is_an_explicit_unimplemented_migration() {
    assert!(matches!(
        PythonSettingsMigration.migrate(b"{}".as_slice()),
        Err(SettingsTransferError::NotImplemented {
            format: SettingsImportFormat::Python
        })
    ));
}

fn config(repository: Arc<dyn SettingsRepository>) -> ApplicationConfig {
    let template = GeminiTemplate::google(
        "fixture",
        "unused",
        GenerationSettings::new(0.7, 64, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(2).unwrap(),
            discovery_buffer: NonZeroUsize::new(2).unwrap(),
            character_commands: NonZeroUsize::new(2).unwrap(),
            presenter_updates: NonZeroUsize::new(2).unwrap(),
            retained_finished: NonZeroUsize::new(2).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.settings = SettingsConfig {
        source: SettingsSource::Stored,
        repository,
    };
    config
}

struct StartupProbe(PathBuf);

struct StartupGate {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl StartupMigration for StartupGate {
    fn name(&self) -> &str {
        "gate"
    }
    fn run(&self) -> Result<(), MigrationError> {
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.release
            .lock()
            .unwrap()
            .recv()
            .map_err(|error| MigrationError::Storage(error.to_string()))
    }
}

impl StartupMigration for StartupProbe {
    fn name(&self) -> &str {
        "extension"
    }
    fn run(&self) -> Result<(), MigrationError> {
        let document: Value = serde_json::from_slice(&fs::read(&self.0).unwrap()).unwrap();
        assert!(document.get("schema_version").is_none());
        assert!(SettingsApi::snapshot_all().is_err());
        Err(MigrationError::Storage("injected extension failure".into()))
    }
}

#[tokio::test]
async fn live_import_uses_cas_not_file_revision_and_updates_bindings_and_restart() {
    let directory = Directory::new();
    let path = directory.path("settings.json");
    let repository = Arc::new(JsonSettingsRepository::new(path.clone()));
    let mut unsupported = serde_json::to_value(record(9)).unwrap();
    unsupported["schema_version"] = json!(5);
    let original = serde_json::to_vec(&unsupported).unwrap();
    fs::write(&path, &original).unwrap();
    assert!(
        ApplicationLifecycle::start(config(repository.clone()))
            .await
            .is_err()
    );
    assert!(SettingsApi::snapshot_all().is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::write(&path, serde_json::to_vec(&record(9)).unwrap()).unwrap();
    let mut failing = config(repository.clone());
    failing
        .migrations
        .register(StartupProbe(path.clone()))
        .unwrap();
    assert!(
        matches!(ApplicationLifecycle::start(failing).await, Err(ApplicationError::StartupMigration(error)) if error.module == "extension")
    );
    assert!(SettingsApi::snapshot_all().is_err());
    assert_eq!(repository.load().unwrap(), record(9));
    let (entered, running) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut blocked = config(repository.clone());
    blocked
        .migrations
        .register(StartupGate {
            entered: std::sync::Mutex::new(Some(entered)),
            release: std::sync::Mutex::new(released),
        })
        .unwrap();
    let starting = tokio::spawn(ApplicationLifecycle::start(blocked));
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    starting.abort();
    assert!(matches!(starting.await, Err(error) if error.is_cancelled()));
    assert!(matches!(
        ApplicationLifecycle::start(config(repository.clone())).await,
        Err(ApplicationError::Busy)
    ));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), ApplicationLifecycle::wait_stopped())
        .await
        .unwrap();
    let app = ApplicationLifecycle::start(config(repository.clone()))
        .await
        .unwrap();
    assert_eq!(SettingsApi::export().unwrap().stored(), &record(9));
    let mut volume = app.presenter().settings.voice.volume.observe();
    assert_eq!(volume.next().await.unwrap().get(), 80);
    let mut imported = record(1);
    imported.settings.voice.volume = Volume::new(31).unwrap();
    imported.settings.interface.language = Language::English;
    imported.settings.capture.enabled = true;
    imported.settings.presets.active = PresetId(17);
    let prepared = SettingsTransfer::new(imported.clone()).unwrap();
    assert!(matches!(
        SettingsApi::import(8, prepared.clone()),
        Err(SettingsError::Conflict {
            expected: 8,
            actual: 9
        })
    ));
    assert_eq!(
        SettingsApi::snapshot_all().unwrap().values,
        record(9).settings
    );
    let snapshot = SettingsApi::import(9, prepared.clone()).unwrap();
    assert_eq!(snapshot.revision, 10);
    assert_eq!(snapshot.values, imported.settings);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), volume.next())
            .await
            .unwrap()
            .unwrap()
            .get(),
        31
    );
    let repeated = app.presenter().settings.import(10, prepared).unwrap();
    assert_eq!(repeated.revision, 10);
    let exported = app.presenter().settings.export().unwrap();
    assert_eq!(exported.stored().revision, 10);
    SettingsApi::flush().await.unwrap();
    assert_eq!(repository.load().unwrap(), exported.stored().clone());
    app.shutdown().await.unwrap();
    assert!(matches!(SettingsApi::export(), Err(SettingsError::Stopped)));
    let restarted = ApplicationLifecycle::start(config(repository))
        .await
        .unwrap();
    assert_eq!(SettingsApi::export().unwrap(), exported);
    restarted.shutdown().await.unwrap();
}

#[cfg(windows)]
#[test]
fn a_locked_destination_keeps_the_previous_file_and_allows_retry() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = Directory::new();
    let path = directory.path("settings.json");
    let repository = JsonSettingsRepository::new(path.clone());
    repository.save(&record(3)).unwrap();
    let before = fs::read(&path).unwrap();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let mut next = record(4);
    next.settings.voice.volume = Volume::new(12).unwrap();
    let bytes = SettingsTransfer::new(next.clone())
        .unwrap()
        .encode()
        .unwrap();
    assert!(repository.import(&bytes).is_err());
    drop(locked);
    assert_eq!(fs::read(&path).unwrap(), before);
    repository.import(&bytes).unwrap();
    assert_eq!(repository.load().unwrap(), next);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn offline_cli_runs_without_a_key_and_exposes_the_python_stub() {
    let directory = Directory::new();
    let source = directory.path("source.json");
    let target = directory.path("target.json");
    let backup = directory.path("backup.json");
    fs::write(&source, serde_json::to_vec(&record(5)).unwrap()).unwrap();
    let run = |command: &str, from: &PathBuf, to: &PathBuf, extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .args(["settings", command, "--source"])
            .arg(from)
            .arg("--destination")
            .arg(to)
            .args(extra)
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    };
    assert!(run("export", &source, &backup, &[]).status.success());
    assert!(run("import", &backup, &target, &[]).status.success());
    assert_eq!(
        JsonSettingsRepository::new(target.clone()).load().unwrap(),
        record(5)
    );
    let before = fs::read(&target).unwrap();
    let python = run("import", &source, &target, &["--format", "python"]);
    assert!(!python.status.success());
    assert!(
        String::from_utf8_lossy(&python.stderr)
            .contains("Python settings migration is not implemented")
    );
    assert_eq!(fs::read(target).unwrap(), before);
}
