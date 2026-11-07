//! Managed Python bootstrap through an explicitly supplied uv executable.

use std::{path::PathBuf, time::Duration};

use thiserror::Error;
use tokio::{process::Command, time::timeout};

#[derive(Debug, Clone)]
pub struct PythonBootstrap {
    pub uv_executable: PathBuf,
    pub data_directory: PathBuf,
    pub python_version: String,
    pub command_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonBootstrapState {
    UvMissing,
    PythonMissing,
    Ready(PathBuf),
}

#[derive(Debug, Error)]
pub enum PythonBootstrapError {
    #[error("bootstrap configuration is invalid: {0}")]
    Configuration(String),
    #[error("bootstrap file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv operation timed out")]
    Timeout,
    #[error("uv {operation} failed: {message}")]
    Uv {
        operation: &'static str,
        message: String,
    },
    #[error("uv returned an unmanaged Python interpreter: {0}")]
    UnmanagedPython(String),
}

impl PythonBootstrap {
    pub fn new(uv_executable: PathBuf, data_directory: PathBuf, python_version: String) -> Self {
        Self {
            uv_executable,
            data_directory,
            python_version,
            command_timeout: Duration::from_secs(600),
        }
    }

    pub fn python_directory(&self) -> PathBuf {
        self.data_directory.join("bootstrap").join("python")
    }

    pub fn cache_directory(&self) -> PathBuf {
        self.data_directory.join("cache").join("uv")
    }

    fn check_configuration(&self) -> Result<(), PythonBootstrapError> {
        if !self.uv_executable.is_absolute() || !self.data_directory.is_absolute() {
            return Err(PythonBootstrapError::Configuration(
                "uv path and AIData path must be absolute".into(),
            ));
        }
        let fields: Vec<&str> = self.python_version.split('.').collect();
        if fields.len() != 3
            || fields
                .iter()
                .any(|value| value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(PythonBootstrapError::Configuration(
                "pin an exact CPython version (major.minor.patch)".into(),
            ));
        }
        if self.command_timeout.is_zero() {
            return Err(PythonBootstrapError::Configuration(
                "timeout is zero".into(),
            ));
        }
        Ok(())
    }

    fn uv_command(&self) -> Command {
        let mut cmd = Command::new(&self.uv_executable);
        cmd.env("UV_PYTHON_INSTALL_DIR", self.python_directory())
            .env("UV_CACHE_DIR", self.cache_directory())
            .env("UV_PYTHON_PREFERENCE", "only-managed")
            .env("UV_NO_CONFIG", "1")
            .env_remove("VIRTUAL_ENV")
            .env_remove("UV_PROJECT_ENVIRONMENT")
            .current_dir(&self.data_directory)
            .kill_on_drop(true);
        cmd
    }

    async fn run(
        &self,
        command: &mut Command,
        operation: &'static str,
    ) -> Result<std::process::Output, PythonBootstrapError> {
        timeout(self.command_timeout, command.output())
            .await
            .map_err(|_| PythonBootstrapError::Timeout)?
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => PythonBootstrapError::Configuration(
                    "configured uv executable is missing".into(),
                ),
                _ => PythonBootstrapError::Io(error),
            })
            .and_then(|output| {
                if output.status.success() {
                    Ok(output)
                } else {
                    let message = String::from_utf8_lossy(&output.stderr);
                    let tail = message.chars().rev().take(1200).collect::<String>();
                    Err(PythonBootstrapError::Uv {
                        operation,
                        message: tail.chars().rev().collect(),
                    })
                }
            })
    }

    async fn locate(&self) -> Result<Option<PathBuf>, PythonBootstrapError> {
        let mut command = self.uv_command();
        command.args([
            "python",
            "find",
            "--managed-python",
            "--no-python-downloads",
            &self.python_version,
        ]);
        let output = match self.run(&mut command, "python find").await {
            Ok(value) => value,
            Err(PythonBootstrapError::Uv { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let root = tokio::fs::canonicalize(self.python_directory()).await?;
        let resolved = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| PythonBootstrapError::UnmanagedPython(path.display().to_string()))?;
        if !resolved.starts_with(root) || !resolved.is_file() || !path.is_absolute() {
            return Err(PythonBootstrapError::UnmanagedPython(
                path.display().to_string(),
            ));
        }
        Ok(Some(path))
    }

    pub async fn probe(&self) -> Result<PythonBootstrapState, PythonBootstrapError> {
        self.check_configuration()?;
        if !self.uv_executable.is_file() {
            return Ok(PythonBootstrapState::UvMissing);
        }
        tokio::fs::create_dir_all(&self.data_directory).await?;
        match self.locate().await? {
            Some(python) => Ok(PythonBootstrapState::Ready(python)),
            None => Ok(PythonBootstrapState::PythonMissing),
        }
    }

    pub async fn prepare(&self) -> Result<PathBuf, PythonBootstrapError> {
        self.check_configuration()?;
        if !self.uv_executable.is_file() {
            return Err(PythonBootstrapError::Configuration(
                "uv must be installed first".into(),
            ));
        }
        tokio::fs::create_dir_all(self.python_directory()).await?;
        tokio::fs::create_dir_all(self.cache_directory()).await?;
        if let Some(python) = self.locate().await? {
            return Ok(python);
        }
        let mut command = self.uv_command();
        command.args([
            "python",
            "install",
            &self.python_version,
            "--no-bin",
            "--install-dir",
        ]);
        command.arg(self.python_directory());
        self.run(&mut command, "python install").await?;
        self.locate().await?.ok_or_else(|| {
            PythonBootstrapError::Configuration(
                "uv installation completed without a managed Python".into(),
            )
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (PythonBootstrap, PathBuf) {
        let root = std::env::temp_dir().join(format!("nm-bootstrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("uv");
        std::fs::write(
            &script,
            r#"#!/bin/sh
if [ "$1" != "python" ]; then exit 1; fi
if [ "$2" = "install" ]; then
    mkdir -p "$UV_PYTHON_INSTALL_DIR/cpython-test/bin"
    printf '#!/bin/sh\nexit 0\n' > "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    chmod +x "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
if [ "$2" = "find" ]; then
    [ -f "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3" ] || exit 2
    printf '%s\n' "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
exit 3
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (
            PythonBootstrap::new(script, root.join("AIData"), "3.13.5".into()),
            root,
        )
    }

    #[tokio::test]
    async fn installs_once_without_venv_or_system_python() {
        let (bootstrap, root) = fixture();
        assert_eq!(
            bootstrap.probe().await.unwrap(),
            PythonBootstrapState::PythonMissing
        );
        let python = bootstrap.prepare().await.unwrap();
        assert!(python.starts_with(bootstrap.python_directory()));
        assert_eq!(
            bootstrap.probe().await.unwrap(),
            PythonBootstrapState::Ready(python.clone())
        );
        assert_eq!(bootstrap.prepare().await.unwrap(), python);
        assert!(!bootstrap.data_directory.join(".venv").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_unpinned_version() {
        let (mut bootstrap, root) = fixture();
        bootstrap.python_version = "3.13".into();
        assert!(matches!(
            bootstrap.prepare().await,
            Err(PythonBootstrapError::Configuration(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_executable_outside_managed_python() {
        let (bootstrap, root) = fixture();
        std::fs::create_dir_all(bootstrap.python_directory()).unwrap();
        let script = bootstrap.uv_executable.clone();
        std::fs::write(&script, "#!/bin/sh\nprintf '/usr/bin/python3\\n'\n").unwrap();
        assert!(matches!(
            bootstrap.probe().await,
            Err(PythonBootstrapError::UnmanagedPython(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
