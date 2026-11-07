use std::{
    ffi::OsString,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use super::{
    InstallSelection, LauncherError, LauncherService, ProcessIdentity, ProcessScan, classify_io,
    identity_for_pid, scan_install,
};

#[derive(Debug, Clone, Default)]
pub struct LaunchRequest {
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

impl LaunchRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_arguments(mut self, arguments: impl IntoIterator<Item = OsString>) -> Self {
        self.arguments = arguments.into_iter().collect();
        self
    }

    pub fn with_environment(
        mut self,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedProcessState {
    Idle,
    Running {
        pid: u32,
        executable: PathBuf,
    },
    Finished {
        exit_code: Option<i32>,
        requested_stop: bool,
    },
    StatusError {
        pid: u32,
        message: String,
    },
}

impl OwnedProcessState {
    pub fn pid(&self) -> Option<u32> {
        match self {
            Self::Running { pid, .. } | Self::StatusError { pid, .. } => Some(*pid),
            Self::Idle | Self::Finished { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub install: InstallSelection,
    pub owned: OwnedProcessState,
    pub processes: ProcessScan,
    pub identity: Option<ProcessIdentity>,
}

struct OwnedChild {
    child: Child,
    pid: u32,
    executable: PathBuf,
}

pub struct LauncherSession {
    service: LauncherService,
    owned: Option<OwnedChild>,
    state: OwnedProcessState,
    identity: Option<ProcessIdentity>,
}

impl LauncherSession {
    pub(crate) fn new(service: LauncherService) -> Self {
        Self {
            service,
            owned: None,
            state: OwnedProcessState::Idle,
            identity: None,
        }
    }

    pub fn service(&self) -> &LauncherService {
        &self.service
    }

    pub fn status(&mut self) -> Result<SessionStatus, LauncherError> {
        self.refresh()?;
        let install = self.service.install()?;
        let processes = scan_install(&install.executable)?;
        Ok(SessionStatus {
            install,
            owned: self.state.clone(),
            processes,
            identity: self.identity.clone(),
        })
    }

    pub fn owned_state(&mut self) -> Result<OwnedProcessState, LauncherError> {
        self.refresh()?;
        Ok(self.state.clone())
    }

    pub fn start(&mut self) -> Result<OwnedProcessState, LauncherError> {
        self.start_with(LaunchRequest::new())
    }

    pub fn start_with(
        &mut self,
        request: LaunchRequest,
    ) -> Result<OwnedProcessState, LauncherError> {
        self.refresh()?;
        if let Some(pid) = self.state.pid() {
            return Err(LauncherError::AlreadyRunning { pid });
        }
        let install = self.service.install()?;
        let processes = scan_install(&install.executable)?;
        if let Some(identity) = processes.matching.first() {
            return Err(LauncherError::AlreadyRunningExternally { pid: identity.pid });
        }

        let mut command = Command::new(&install.executable);
        command
            .current_dir(&install.root)
            .args(&request.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in request.environment {
            command.env(name, value);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let child = command
            .spawn()
            .map_err(|error| classify_io("starting game", &install.executable, error))?;
        let pid = child.id();
        self.identity = identity_for_pid(pid).ok().flatten();
        self.owned = Some(OwnedChild {
            child,
            pid,
            executable: install.executable.clone(),
        });
        self.state = OwnedProcessState::Running {
            pid,
            executable: install.executable,
        };
        Ok(self.state.clone())
    }

    pub fn stop(&mut self) -> Result<OwnedProcessState, LauncherError> {
        self.refresh()?;
        let Some(owned) = self.owned.as_mut() else {
            return Err(LauncherError::NotRunning);
        };
        let executable = owned.executable.clone();
        if let Err(error) = owned.child.kill() {
            if let Ok(Some(status)) = owned.child.try_wait() {
                self.owned = None;
                self.identity = None;
                self.state = OwnedProcessState::Finished {
                    exit_code: status.code(),
                    requested_stop: true,
                };
                return Ok(self.state.clone());
            }
            return Err(classify_io("stopping owned game", &executable, error));
        }
        let status = owned
            .child
            .wait()
            .map_err(|error| classify_io("waiting for owned game", &executable, error))?;
        self.owned = None;
        self.identity = None;
        self.state = OwnedProcessState::Finished {
            exit_code: status.code(),
            requested_stop: true,
        };
        Ok(self.state.clone())
    }

    pub fn open_location(&self) -> Result<(), LauncherError> {
        self.service.open_location()
    }

    pub fn shutdown(&mut self) -> Result<(), LauncherError> {
        self.refresh()?;
        if self.owned.is_some() {
            self.stop()?;
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), LauncherError> {
        let Some(owned) = self.owned.as_mut() else {
            return Ok(());
        };
        match owned.child.try_wait() {
            Ok(Some(status)) => {
                self.owned = None;
                self.state = OwnedProcessState::Finished {
                    exit_code: status.code(),
                    requested_stop: false,
                };
                self.identity = None;
                Ok(())
            }
            Ok(None) => {
                self.state = OwnedProcessState::Running {
                    pid: owned.pid,
                    executable: owned.executable.clone(),
                };
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                self.state = OwnedProcessState::StatusError {
                    pid: owned.pid,
                    message: message.clone(),
                };
                Err(LauncherError::Process {
                    pid: owned.pid,
                    message,
                })
            }
        }
    }
}

impl Drop for LauncherSession {
    fn drop(&mut self) {
        if let Some(owned) = self.owned.as_mut() {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
            self.owned = None;
            self.identity = None;
            self.state = OwnedProcessState::Finished {
                exit_code: None,
                requested_stop: true,
            };
        }
    }
}
