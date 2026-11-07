mod cli;
mod config;
mod controller;
mod discovery;
mod opener;
mod process;
mod session;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use thiserror::Error;

pub use config::{ConfigStore, LauncherConfig};
pub use controller::{LauncherCommand, LauncherController, LauncherUpdate};
pub use discovery::InstallSelection;
pub use opener::{LocationOpener, SystemLocationOpener};
pub use process::{IdentityProbe, ProcessIdentity, ProcessScan, identity_for_pid};
pub use session::{LaunchRequest, LauncherSession, OwnedProcessState, SessionStatus};

use crate::launcher::{discovery::resolve_install, process::scan_install};

pub fn run_cli(arguments: Vec<std::ffi::OsString>) -> std::io::Result<()> {
    cli::run(arguments)
}

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("launcher config is missing: {0}")]
    MissingConfig(PathBuf),
    #[error("launcher config {path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("launcher config schema {schema} is unsupported: {path}")]
    UnsupportedConfigSchema { path: PathBuf, schema: u32 },
    #[error("install root is missing: {0}")]
    MissingInstall(PathBuf),
    #[error("install root is not a directory: {0}")]
    InstallRootNotDirectory(PathBuf),
    #[error("no verified game executable was found in {0}")]
    NoCandidate(PathBuf),
    #[error("game executable {candidate} is outside install root {root}")]
    CandidateOutsideRoot { root: PathBuf, candidate: PathBuf },
    #[error("game executable is a symbolic link or reparse point: {0}")]
    UnsafeCandidate(PathBuf),
    #[error("game executable is not a regular file: {0}")]
    CandidateNotFile(PathBuf),
    #[error("game executable is not a Windows executable: {0}")]
    CandidateNotExecutable(PathBuf),
    #[error("multiple verified game executables found in {root}: {candidates:?}")]
    AmbiguousCandidates {
        root: PathBuf,
        candidates: Vec<PathBuf>,
    },
    #[error("access denied while {operation} at {path}: {source}")]
    AccessDenied {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("I/O error while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("game is already running as an owned process (pid {pid})")]
    AlreadyRunning { pid: u32 },
    #[error("game is already running outside this launcher session (pid {pid})")]
    AlreadyRunningExternally { pid: u32 },
    #[error("launcher session does not own a running game")]
    NotRunning,
    #[error("process {pid}: {message}")]
    Process { pid: u32, message: String },
    #[error("failed to open location {path}: {message}")]
    OpenLocation { path: PathBuf, message: String },
}

impl LauncherError {
    pub fn is_missing_config(&self) -> bool {
        matches!(self, Self::MissingConfig(_))
    }
}

pub(crate) fn classify_io(
    operation: &'static str,
    path: &Path,
    error: std::io::Error,
) -> LauncherError {
    if error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(5 | 32))
    {
        LauncherError::AccessDenied {
            operation,
            path: path.to_path_buf(),
            source: error,
        }
    } else {
        LauncherError::Io {
            operation,
            path: path.to_path_buf(),
            source: error,
        }
    }
}

pub(crate) fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = left.to_string_lossy();
    let right = right.to_string_lossy();
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

pub(crate) fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = path.to_string_lossy();
    let root = root.to_string_lossy();
    if cfg!(windows) {
        let path = path.to_ascii_lowercase();
        let mut root = root.to_ascii_lowercase();
        if !root.ends_with('\\') {
            root.push('\\');
        }
        path == root.trim_end_matches('\\') || path.starts_with(&root)
    } else {
        path == root || path.starts_with(&format!("{root}/"))
    }
}

#[derive(Debug, Clone)]
pub struct LauncherSnapshot {
    pub install: InstallSelection,
    pub processes: ProcessScan,
}

pub struct LauncherApi;

impl LauncherApi {
    pub fn install(config: &LauncherConfig) -> Result<InstallSelection, LauncherError> {
        resolve_install(config)
    }

    pub fn status(config: &LauncherConfig) -> Result<LauncherSnapshot, LauncherError> {
        let install = Self::install(config)?;
        let processes = scan_install(&install.executable)?;
        Ok(LauncherSnapshot { install, processes })
    }

    pub fn open_location(
        config: &LauncherConfig,
        opener: &dyn LocationOpener,
    ) -> Result<(), LauncherError> {
        let root = discovery::existing_root(&config.install_root)?;
        opener.open(&root)
    }

    pub fn inspect_identity(identity: &ProcessIdentity) -> Result<IdentityProbe, LauncherError> {
        process::inspect_identity(identity)
    }

    pub fn session(config: LauncherConfig) -> LauncherSession {
        LauncherService::new(config).session()
    }
}

#[derive(Clone)]
pub struct LauncherService {
    config: LauncherConfig,
    opener: Arc<dyn LocationOpener>,
}

impl LauncherService {
    pub fn new(config: LauncherConfig) -> Self {
        Self::with_opener(config, Arc::new(SystemLocationOpener))
    }

    pub fn with_opener(config: LauncherConfig, opener: Arc<dyn LocationOpener>) -> Self {
        Self { config, opener }
    }

    pub fn config(&self) -> &LauncherConfig {
        &self.config
    }

    pub fn install(&self) -> Result<InstallSelection, LauncherError> {
        LauncherApi::install(&self.config)
    }

    pub fn status(&self) -> Result<LauncherSnapshot, LauncherError> {
        LauncherApi::status(&self.config)
    }

    pub fn open_location(&self) -> Result<(), LauncherError> {
        LauncherApi::open_location(&self.config, self.opener.as_ref())
    }

    pub fn session(self) -> LauncherSession {
        LauncherSession::new(self)
    }
}
