use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, SettingsRepository, StoredSettings, Volume,
};
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_neuromita");

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("neuromita-settings-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn stored(revision: u64, volume: u8) -> StoredSettings {
    let mut settings = AppSettings::initial_profile();
    settings.voice.volume = Volume::new(volume).unwrap();
    StoredSettings { revision, settings }
}

fn run(arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .args(arguments)
        .env_remove("GEMINI_API_KEY")
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("ALL_PROXY")
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn offline_export_and_import_roundtrip_without_api_key() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let backup_path = directory.path("backup.json");
    let restored_path = directory.path("restored.json");
    let settings = stored(5, 33);
    JsonSettingsRepository::new(store_path.clone())
        .save(&settings)
        .unwrap();

    let exported = run(&[
        "settings",
        "export",
        "--source",
        store_path.to_str().unwrap(),
        "--destination",
        backup_path.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    assert!(
        stdout(&exported).contains("Exported full Rust AppSettings (revision 5)"),
        "{}",
        stdout(&exported)
    );
    let backup = fs::read(&backup_path).unwrap();

    let exported_again = run(&[
        "settings",
        "export",
        "--source",
        store_path.to_str().unwrap(),
        "--destination",
        backup_path.to_str().unwrap(),
    ]);
    assert!(exported_again.status.success());
    assert_eq!(fs::read(&backup_path).unwrap(), backup);

    let imported = run(&[
        "settings",
        "import",
        "--source",
        backup_path.to_str().unwrap(),
        "--destination",
        restored_path.to_str().unwrap(),
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert!(
        stdout(&imported).contains("Imported full Rust AppSettings (revision 5)"),
        "{}",
        stdout(&imported)
    );
    assert_eq!(
        JsonSettingsRepository::new(restored_path).load().unwrap(),
        settings
    );
}

#[test]
fn invalid_cli_import_preserves_the_destination() {
    let directory = TempDir::new();
    let destination_path = directory.path("store.json");
    JsonSettingsRepository::new(destination_path.clone())
        .save(&stored(2, 70))
        .unwrap();
    let before = fs::read(&destination_path).unwrap();

    let invalid_source = directory.path("future.json");
    let mut invalid = serde_json::to_value(stored(9, 10)).unwrap();
    invalid["schema_version"] = serde_json::json!(5);
    fs::write(
        &invalid_source,
        serde_json::to_vec_pretty(&invalid).unwrap(),
    )
    .unwrap();
    let rejected = run(&[
        "settings",
        "import",
        "--source",
        invalid_source.to_str().unwrap(),
        "--destination",
        destination_path.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("invalid settings transfer"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&destination_path).unwrap(), before);

    let partial_source = directory.path("partial.json");
    fs::write(&partial_source, br#"{"volume":70}"#).unwrap();
    let rejected = run(&[
        "settings",
        "import",
        "--source",
        partial_source.to_str().unwrap(),
        "--destination",
        destination_path.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("not a complete envelope"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&destination_path).unwrap(), before);
}

#[test]
fn help_documents_offline_commands_without_api_key() {
    let help = run(&["--help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let text = stdout(&help);
    assert!(text.contains("neuromita settings export --source"));
    assert!(text.contains("neuromita settings import --source"));
    assert!(text.contains("no GEMINI_API_KEY"));
}
