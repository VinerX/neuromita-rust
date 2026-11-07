use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use super::{LauncherApi, LauncherConfig, OwnedProcessState};

#[derive(Debug, Clone, Copy)]
pub enum LauncherCommand {
    Toggle,
    OpenLocation,
}

#[derive(Debug, Clone)]
pub struct LauncherUpdate {
    pub state: OwnedProcessState,
    pub installed: bool,
    pub error: Option<String>,
}

pub struct LauncherController {
    sender: Option<mpsc::SyncSender<LauncherCommand>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LauncherController {
    pub fn start(
        config: LauncherConfig,
        publish: impl Fn(LauncherUpdate) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receive) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("launcher-owner".into())
            .spawn(move || {
                let mut session = LauncherApi::session(config);
                let mut previous = None;
                let mut previous_installation = None;
                while !worker_stop.load(Ordering::Acquire) {
                    let result = match receive.recv_timeout(Duration::from_millis(250)) {
                        Ok(LauncherCommand::Toggle) => match session.owned_state() {
                            Ok(OwnedProcessState::Running { .. }) => session.stop().map(|_| ()),
                            Ok(_) => session.start().map(|_| ()),
                            Err(error) => Err(error),
                        },
                        Ok(LauncherCommand::OpenLocation) => session.open_location(),
                        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let error = result.err().map(|error| error.to_string());
                    let installed = session.service().install().is_ok();
                    match session.owned_state() {
                        Ok(state) => {
                            if previous.as_ref() != Some(&state)
                                || error.is_some()
                                || previous_installation != Some(installed)
                            {
                                previous = Some(state.clone());
                                previous_installation = Some(installed);
                                publish(LauncherUpdate {
                                    state,
                                    installed,
                                    error,
                                });
                            }
                        }
                        Err(error) => publish(LauncherUpdate {
                            state: previous.clone().unwrap_or(OwnedProcessState::Idle),
                            installed,
                            error: Some(error.to_string()),
                        }),
                    }
                }
                if let Err(error) = session.shutdown() {
                    publish(LauncherUpdate {
                        state: OwnedProcessState::Idle,
                        installed: session.service().install().is_ok(),
                        error: Some(error.to_string()),
                    });
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            stopped,
            worker: Some(worker),
        })
    }

    pub fn submit(&self, command: LauncherCommand) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or("launcher is stopped")?
            .try_send(command)
            .map_err(|error| error.to_string())
    }

    /// Disconnects admission and waits for owned process cleanup, after the UI loop exits.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "launcher owner panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for LauncherController {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_errors_are_observed_and_stopped_owner_rejects_commands() {
        let root = std::env::temp_dir().join(format!("absent-launcher-{}", uuid::Uuid::new_v4()));
        let (send, receive) = mpsc::channel();
        let mut owner = LauncherController::start(LauncherConfig::new(root), move |update| {
            let _ = send.send(update);
        })
        .unwrap();
        owner.submit(LauncherCommand::Toggle).unwrap();
        let update = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.error.is_some());
        assert_eq!(update.state, OwnedProcessState::Idle);
        owner.shutdown().unwrap();
        assert!(owner.submit(LauncherCommand::Toggle).is_err());
    }
}
