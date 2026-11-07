use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use neuromita::launcher::{
    ConfigStore, IdentityProbe, LaunchRequest, LauncherApi, LauncherConfig, LauncherError,
    LauncherService, LocationOpener, OwnedProcessState, identity_for_pid,
};

const HELPER_ENVIRONMENT: &str = "NEUROMITA_LAUNCHER_HELPER_MS";

#[test]
#[ignore = "spawned by launcher integration tests"]
fn launcher_helper_process() {
    let Ok(milliseconds) = std::env::var(HELPER_ENVIRONMENT) else {
        return;
    };
    let milliseconds = milliseconds.parse::<u64>().expect("helper duration");
    thread::sleep(Duration::from_millis(milliseconds));
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "neuromita-launcher-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("live helper").id()
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("live helper")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn fixture_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neuromita"))
}

fn test_executable() -> PathBuf {
    std::env::current_exe().expect("test executable")
}

fn install_candidate(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("create install root");
    let candidate = root.join("NeuroMita.exe");
    fs::copy(test_executable(), &candidate).expect("copy fixture executable");
    candidate
}

fn helper_arguments(milliseconds: u64) -> (Vec<OsString>, Vec<(OsString, OsString)>) {
    (
        [
            "--exact",
            "launcher_helper_process",
            "--ignored",
            "--nocapture",
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
        vec![(
            OsString::from(HELPER_ENVIRONMENT),
            OsString::from(milliseconds.to_string()),
        )],
    )
}

fn helper_request(milliseconds: u64) -> LaunchRequest {
    let (arguments, environment) = helper_arguments(milliseconds);
    LaunchRequest::new()
        .with_arguments(arguments)
        .with_environment(environment)
}

fn spawn_helper(executable: &Path, milliseconds: u64) -> ChildGuard {
    let (arguments, environment) = helper_arguments(milliseconds);
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .envs(environment)
        .current_dir(executable.parent().expect("fixture parent"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    ChildGuard::new(command.spawn().expect("spawn helper"))
}

fn wait_for_identity(pid: u32) -> neuromita::launcher::ProcessIdentity {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(identity) = identity_for_pid(pid).expect("read process identity") {
            return identity;
        }
        assert!(Instant::now() < deadline, "helper identity did not appear");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(25));
    }
}

fn installed_service(root: &Path) -> LauncherService {
    LauncherService::new(LauncherConfig::new(root))
}

#[derive(Default)]
struct RecordingOpener {
    calls: Mutex<Vec<PathBuf>>,
}

impl LocationOpener for RecordingOpener {
    fn open(&self, directory: &Path) -> Result<(), LauncherError> {
        self.calls
            .lock()
            .expect("recorder lock")
            .push(directory.to_path_buf());
        Ok(())
    }
}

#[test]
fn missing_install_is_reported_by_status_start_and_open_location() {
    let directory = TestDirectory::new("missing");
    let root = directory.path().join("absent");
    let config = LauncherConfig::new(&root);

    assert!(
        matches!(LauncherApi::status(&config), Err(LauncherError::MissingInstall(path)) if path == root)
    );
    let service = LauncherService::new(config.clone());
    assert!(
        matches!(service.open_location(), Err(LauncherError::MissingInstall(path)) if path == root)
    );
    let mut session = LauncherApi::session(config);
    assert!(matches!(session.start(), Err(LauncherError::MissingInstall(path)) if path == root));
    assert!(matches!(session.stop(), Err(LauncherError::NotRunning)));
}

#[test]
fn discovery_rejects_unverified_candidates() {
    let directory = TestDirectory::new("discovery");
    let root = directory.path().join("install");
    fs::create_dir_all(&root).expect("create install root");
    assert!(matches!(
        installed_service(&root).status(),
        Err(LauncherError::NoCandidate(path)) if path == root.canonicalize().unwrap()
    ));

    let text = root.join("NeuroMita.exe");
    fs::write(&text, b"not an executable").expect("write text candidate");
    assert!(matches!(
        installed_service(&root).status(),
        Err(LauncherError::CandidateNotExecutable(path)) if path == text.canonicalize().unwrap()
    ));
    fs::remove_file(&text).expect("remove text candidate");

    let unity_editor = root.join("Unity.exe");
    fs::copy(test_executable(), &unity_editor).expect("copy ambiguous executable");
    assert!(matches!(
        installed_service(&root).status(),
        Err(LauncherError::NoCandidate(path)) if path == root.canonicalize().unwrap()
    ));

    let explicit = LauncherService::new(LauncherConfig::new(&root).with_candidate("Unity.exe"));
    assert_eq!(
        explicit
            .status()
            .expect("explicit candidate")
            .install
            .executable,
        unity_editor.canonicalize().unwrap()
    );

    let outside = directory.path().join("outside.exe");
    fs::copy(test_executable(), &outside).expect("copy outside executable");
    let escape = LauncherService::new(LauncherConfig::new(&root).with_candidate(&outside));
    assert!(matches!(
        escape.status(),
        Err(LauncherError::CandidateOutsideRoot { .. })
    ));

    let not_a_directory = directory.path().join("install-root-file");
    fs::write(&not_a_directory, b"file").expect("write root file");
    assert!(matches!(
        installed_service(&not_a_directory).status(),
        Err(LauncherError::InstallRootNotDirectory(_))
    ));
}

#[test]
fn config_persists_only_install_selection_and_reopens() {
    let directory = TestDirectory::new("config");
    let root = directory.path().join("install");
    install_candidate(&root);
    let path = directory.path().join("launcher.json");
    let store = ConfigStore::new(&path);
    let config = LauncherConfig::new(&root).with_candidate("NeuroMita.exe");
    store.save(&config).expect("save config");

    let mut updated = config.clone();
    updated.candidate = Some(PathBuf::from(if cfg!(windows) {
        ".\\NeuroMita.exe"
    } else {
        "./NeuroMita.exe"
    }));
    store.save(&updated).expect("overwrite existing config");

    let reopened = ConfigStore::new(&path).load().expect("reload config");
    assert_eq!(reopened, updated);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read config")).expect("json config");
    let object = value.as_object().expect("config object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["candidate", "install_root", "schema_version"]);

    let api = LauncherService::new(reopened);
    let install = api.status().expect("reopened install").install;
    assert_eq!(install.executable.file_name().unwrap(), "NeuroMita.exe");
    let mut session = api.session();
    assert_eq!(
        session.owned_state().expect("fresh session state"),
        OwnedProcessState::Idle
    );
}

#[test]
fn owned_helper_completes_with_its_real_exit_code() {
    let directory = TestDirectory::new("completion");
    let root = directory.path().join("install");
    install_candidate(&root);
    let mut session = installed_service(&root).session();
    let started = session
        .start_with(helper_request(200))
        .expect("start helper");
    assert!(matches!(started, OwnedProcessState::Running { .. }));

    let mut finished = None;
    wait_until(Duration::from_secs(5), || {
        let state = session.owned_state().expect("poll owned state");
        if let OwnedProcessState::Finished {
            exit_code,
            requested_stop,
        } = state
        {
            finished = Some((exit_code, requested_stop));
            true
        } else {
            false
        }
    });
    assert_eq!(finished, Some((Some(0), false)));
}

#[test]
fn already_running_is_rejected_and_stop_completes_the_owned_child() {
    let directory = TestDirectory::new("stop");
    let root = directory.path().join("install");
    install_candidate(&root);
    let mut session = installed_service(&root).session();
    let started = session
        .start_with(helper_request(30_000))
        .expect("start helper");
    let pid = started.pid().expect("running pid");
    assert!(matches!(
        session.start(),
        Err(LauncherError::AlreadyRunning { pid: running }) if running == pid
    ));

    let stopped = session.stop().expect("stop owned child");
    assert!(matches!(
        stopped,
        OwnedProcessState::Finished {
            requested_stop: true,
            ..
        }
    ));
    assert!(matches!(session.stop(), Err(LauncherError::NotRunning)));
    assert_eq!(session.status().expect("post-stop status").identity, None);
}

#[test]
fn exact_install_running_outside_the_session_blocks_start_and_is_not_owned() {
    let directory = TestDirectory::new("external");
    let root = directory.path().join("install");
    let candidate = install_candidate(&root);
    let mut external = spawn_helper(&candidate, 30_000);
    wait_for_identity(external.pid());
    let mut session = installed_service(&root).session();
    assert!(matches!(
        session.start(),
        Err(LauncherError::AlreadyRunningExternally { pid }) if pid == external.pid()
    ));
    assert!(matches!(session.stop(), Err(LauncherError::NotRunning)));
    assert_eq!(
        external.child_mut().try_wait().expect("external status"),
        None
    );
}

#[test]
fn stale_identity_and_real_foreign_same_name_process_are_never_owned() {
    let directory = TestDirectory::new("identity");
    let root = directory.path().join("install");
    let candidate = install_candidate(&root);
    let api = installed_service(&root);

    let mut helper = spawn_helper(&candidate, 30_000);
    let identity = wait_for_identity(helper.pid());
    assert_eq!(
        LauncherApi::inspect_identity(&identity).expect("inspect live identity"),
        IdentityProbe::Running
    );
    let mut stale = identity.clone();
    stale.created = stale.created.wrapping_add(1);
    assert_eq!(
        LauncherApi::inspect_identity(&stale).expect("inspect stale identity"),
        IdentityProbe::Stale
    );
    helper.child_mut().kill().expect("kill identity helper");
    helper.child_mut().wait().expect("reap identity helper");
    assert_eq!(
        LauncherApi::inspect_identity(&identity).expect("inspect gone identity"),
        IdentityProbe::Gone
    );

    let foreign_root = directory.path().join("foreign");
    fs::create_dir_all(&foreign_root).expect("create foreign root");
    let foreign_executable = foreign_root.join("NeuroMita.exe");
    fs::copy(test_executable(), &foreign_executable).expect("copy foreign executable");
    let mut foreign = spawn_helper(&foreign_executable, 30_000);
    let foreign_pid = foreign.pid();
    let scan = api.status().expect("scan processes").processes;
    assert!(scan.matching.is_empty());
    assert!(
        scan.same_name_foreign
            .iter()
            .any(|identity| identity.pid == foreign_pid)
    );

    let mut session = api.session();
    assert!(matches!(session.stop(), Err(LauncherError::NotRunning)));
    assert_eq!(
        foreign.child_mut().try_wait().expect("foreign status"),
        None
    );
}

#[cfg(windows)]
#[test]
fn permission_denied_start_is_reported_without_owning_a_child() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = TestDirectory::new("denied");
    let root = directory.path().join("install");
    let candidate = install_candidate(&root);
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&candidate)
        .expect("lock candidate");
    let mut session = installed_service(&root).session();
    assert!(matches!(
        session.start_with(helper_request(30_000)),
        Err(LauncherError::AccessDenied { .. })
    ));
    assert_eq!(
        session.owned_state().expect("state after denied start"),
        OwnedProcessState::Idle
    );
}

#[test]
fn open_location_is_explicit_and_can_be_reopened_without_explorer_in_tests() {
    let directory = TestDirectory::new("open");
    let root = directory.path().join("install");
    fs::create_dir_all(&root).expect("create install root");
    let recorder = Arc::new(RecordingOpener::default());
    let api = LauncherService::with_opener(LauncherConfig::new(&root), recorder.clone());
    api.open_location().expect("first open");
    api.open_location().expect("reopen");
    let calls = recorder.calls.lock().expect("recorder lock").clone();
    let canonical = root.canonicalize().expect("canonical root");
    assert_eq!(calls, [canonical.clone(), canonical]);
}

#[test]
fn dropping_the_session_stops_and_reaps_its_owned_child() {
    let directory = TestDirectory::new("drop");
    let root = directory.path().join("install");
    install_candidate(&root);
    let mut session = installed_service(&root).session();
    let state = session
        .start_with(helper_request(30_000))
        .expect("start helper");
    let pid = state.pid().expect("running pid");
    drop(session);

    wait_until(Duration::from_secs(5), || {
        identity_for_pid(pid)
            .map(|identity| identity.is_none())
            .unwrap_or(true)
    });
}

#[test]
fn launcher_cli_reads_status_without_gemini_credentials_and_rejects_unowned_start() {
    let directory = TestDirectory::new("cli-status");
    let root = directory.path().join("install");
    let candidate = install_candidate(&root);
    let output = Command::new(fixture_executable())
        .args(["launcher", "status", "--install-root"])
        .arg(&root)
        .env_remove("GEMINI_API_KEY")
        .output()
        .expect("run launcher status");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&candidate.canonicalize().unwrap().display().to_string()));

    let rejected = Command::new(fixture_executable())
        .args(["launcher", "start"])
        .env_remove("GEMINI_API_KEY")
        .output()
        .expect("run launcher start");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("launcher session"));
}

#[test]
fn interactive_cli_persists_selection_and_does_not_auto_start() {
    let directory = TestDirectory::new("cli-session");
    let root = directory.path().join("install");
    install_candidate(&root);
    let config = directory.path().join("launcher.json");
    let mut child = Command::new(fixture_executable())
        .args(["launcher", "session", "--config"])
        .arg(&config)
        .arg("--install-root")
        .arg(&root)
        .env_remove("GEMINI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start launcher session");
    child
        .stdin
        .take()
        .expect("session stdin")
        .write_all(b"status\nquit\n")
        .expect("write session commands");
    let output = child.wait_with_output().expect("wait for session");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Owned process: not running"));
    let saved = ConfigStore::new(&config).load().expect("saved config");
    assert_eq!(saved.install_root, root);
    assert_eq!(saved.candidate, None);
    let reopened = Command::new(fixture_executable())
        .args(["launcher", "session", "--config"])
        .arg(&config)
        .env_remove("GEMINI_API_KEY")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(reopened.status.success(), "{reopened:?}");
}
