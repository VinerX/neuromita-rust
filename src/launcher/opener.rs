use std::{
    path::Path,
    process::{Command, Stdio},
};

use super::LauncherError;

pub trait LocationOpener: Send + Sync {
    fn open(&self, directory: &Path) -> Result<(), LauncherError>;
}

pub struct SystemLocationOpener;

impl SystemLocationOpener {
    fn command(directory: &Path) -> Command {
        let mut command = Command::new(if cfg!(windows) {
            "explorer.exe"
        } else {
            "xdg-open"
        });
        command.arg(directory);
        command
    }
}

impl LocationOpener for SystemLocationOpener {
    fn open(&self, directory: &Path) -> Result<(), LauncherError> {
        let mut command = Self::command(directory);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|error| LauncherError::OpenLocation {
                path: directory.to_path_buf(),
                message: error.to_string(),
            })?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SystemLocationOpener;

    #[test]
    fn location_command_passes_the_directory_as_one_argument() {
        let directory = std::path::Path::new("C:\\Games & Tools\\NeuroMita");
        let command = SystemLocationOpener::command(directory);
        assert_eq!(
            command.get_program(),
            if cfg!(windows) {
                "explorer.exe"
            } else {
                "xdg-open"
            }
        );
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, [directory.as_os_str()]);
    }
}
