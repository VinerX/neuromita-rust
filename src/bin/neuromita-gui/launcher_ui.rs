use std::path::{Path, PathBuf};

use neuromita::{
    launcher::{ConfigStore, LauncherConfig, OwnedProcessState},
    localization::{LocalizedText, i18n},
};

pub struct LauncherUi {
    controller: neuromita::launcher::LauncherController,
}
impl LauncherUi {
    pub fn new(
        config: LauncherConfig,
        window: slint::Weak<super::view::AppWindow>,
    ) -> std::io::Result<Self> {
        let controller = neuromita::launcher::LauncherController::start(config, move |update| {
            let _ = window.upgrade_in_event_loop(move |window| {
                window.set_launcher_inspected(true);
                window.set_unity_installed(update.installed);
                window.set_game_running(matches!(update.state, OwnedProcessState::Running { .. }));
                window.set_notice(update.error.unwrap_or_default().into());
            });
        })?;
        Ok(Self { controller })
    }
    pub fn toggle_play(&mut self) -> LocalizedText {
        self.submit(neuromita::launcher::LauncherCommand::Toggle)
    }
    pub fn open_location(&self) -> LocalizedText {
        self.submit(neuromita::launcher::LauncherCommand::OpenLocation)
    }
    fn submit(&self, command: neuromita::launcher::LauncherCommand) -> LocalizedText {
        match self.controller.submit(command) {
            Ok(()) => i18n("launcher.game.queued", "Команда лаунчера принята."),
            Err(error) => i18n(
                "launcher.game.operation-error",
                "Не удалось запустить или остановить игру: {error}",
            )
            .arg("error", error),
        }
    }
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.controller.shutdown()
    }
}

pub fn load_config(
    unity_directory: Option<&Path>,
    config_path: Option<&Path>,
) -> Result<LauncherConfig, Box<dyn std::error::Error>> {
    if unity_directory.is_some() && config_path.is_some() {
        return Err("--unity-dir and --launcher-config cannot be combined".into());
    }
    if let Some(path) = config_path {
        return Ok(ConfigStore::new(path).load()?);
    }
    let root = match unity_directory {
        Some(root) => root.to_path_buf(),
        None => default_unity_directory()?,
    };
    let config = LauncherConfig::new(root);
    config.validate(Path::new("--unity-dir"))?;
    Ok(config)
}

fn default_unity_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(root) = std::env::var_os("NEUROMITA_BASE_DIR") {
        if root.is_empty() {
            return Err("NEUROMITA_BASE_DIR must not be empty".into());
        }
        return Ok(PathBuf::from(root).join("NeuroMita-Unity"));
    }
    Ok(std::env::current_exe()?
        .parent()
        .ok_or("executable has no parent directory")?
        .join("NeuroMita-Unity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_unity_directory_builds_a_launcher_config() {
        let root = Path::new("test-unity-install");
        let config = load_config(Some(root), None).unwrap();
        assert_eq!(config.install_root, root);
        assert!(config.candidate.is_none());
    }

    #[test]
    fn cannot_override_a_stored_launcher_config_silently() {
        assert!(load_config(Some(Path::new("unity")), Some(Path::new("launcher.json"))).is_err());
    }

    #[test]
    fn missing_explicit_config_is_an_error() {
        let path =
            std::env::temp_dir().join(format!("absent-launcher-{}.json", uuid::Uuid::new_v4()));
        assert!(load_config(None, Some(&path)).is_err());
    }
}
