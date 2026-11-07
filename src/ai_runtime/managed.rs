//! Bootstrap orchestration: a pinned uv binary precedes its managed CPython install.

use std::{path::PathBuf, time::Duration};

use tokio::{process::Command, time::timeout};

use thiserror::Error;

use super::{
    bootstrap::{PythonBootstrap, PythonBootstrapError},
    uv::{UvInstallError, UvInstaller},
};

pub const MANAGED_PYTHON_MINOR: &str = "3.12";

#[derive(Clone)]
pub struct ManagedRuntimeBootstrap {
    pub uv: UvInstaller,
    pub python_version: String,
    pub python_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntime {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
}

#[derive(Debug, Error)]
pub enum ManagedRuntimeError {
    #[error("uv bootstrap failed: {0}")]
    Uv(#[from] UvInstallError),
    #[error("managed Python bootstrap failed: {0}")]
    Python(#[from] PythonBootstrapError),
    #[error("managed AI Runtime requires CPython 3.12.x, got {0}")]
    UnsupportedPython(String),
    #[error("managed Python version verification failed: {0}")]
    Interpreter(String),
}

impl ManagedRuntimeBootstrap {
    pub async fn prepare(&self) -> Result<ManagedRuntime, ManagedRuntimeError> {
        if self
            .python_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".")
            != MANAGED_PYTHON_MINOR
            || self.python_version.split('.').count() != 3
            || !self
                .python_version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(ManagedRuntimeError::UnsupportedPython(
                self.python_version.clone(),
            ));
        }
        let uv_executable = self.uv.prepare().await?;
        let mut python = PythonBootstrap::new(
            uv_executable.clone(),
            self.uv.data_directory.clone(),
            self.python_version.clone(),
        );
        python.command_timeout = self.python_timeout;
        let python_executable = python.prepare().await?;
        let output = timeout(
            self.python_timeout,
            Command::new(&python_executable)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| ManagedRuntimeError::Interpreter("version probe timed out".into()))?
        .map_err(|error| ManagedRuntimeError::Interpreter(error.to_string()))?;
        let version = if output.stdout.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        if !output.status.success() || version != format!("Python {}", self.python_version) {
            return Err(ManagedRuntimeError::Interpreter(format!(
                "expected Python {}, interpreter returned {version:?}",
                self.python_version
            )));
        }
        Ok(ManagedRuntime {
            uv_executable,
            python_executable,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ai_runtime::uv::{UvArchiveKind, UvDistribution};
    use std::os::unix::fs::PermissionsExt;
    use tokio::process::Command;
    use uuid::Uuid;

    #[tokio::test]
    async fn pinned_uv_bootstraps_managed_python_without_system_install() {
        let root = std::env::temp_dir().join(format!("nm-runtime-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(
            &uv,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 'uv 0.13.4'; exit 0; fi
if [ "$1" = "python" ] && [ "$2" = "install" ]; then
    mkdir -p "$UV_PYTHON_INSTALL_DIR/cpython-test/bin"
    printf '#!/bin/sh\necho Python 3.12.10\n' > "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    chmod +x "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
if [ "$1" = "python" ] && [ "$2" = "find" ]; then
    [ -f "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3" ] || exit 2
    printf '%s\n' "$UV_PYTHON_INSTALL_DIR/cpython-test/bin/python3"
    exit 0
fi
exit 4
"#,
        )
        .await
        .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let archive = root.join("uv.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&root)
                .arg("uv")
                .output()
                .await
                .unwrap()
                .status
                .success()
        );
        let archive_bytes = tokio::fs::read(&archive).await.unwrap();
        let bytes = ring::digest::digest(&ring::digest::SHA256, &archive_bytes);
        let digest: String = bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let bootstrap = ManagedRuntimeBootstrap {
            uv: UvInstaller {
                data_directory: root.join("AIData"),
                distribution: UvDistribution {
                    version: "0.13.4".into(),
                    archive_url: format!("file://{}", archive.display()),
                    archive_sha256: digest,
                    archive_kind: UvArchiveKind::TarGz,
                },
                timeout: Duration::from_secs(5),
            },
            python_version: "3.12.10".into(),
            python_timeout: Duration::from_secs(5),
        };
        let runtime = bootstrap.prepare().await.unwrap();
        assert!(runtime.uv_executable.is_file());
        assert!(runtime.python_executable.is_file());
        assert!(
            runtime
                .python_executable
                .starts_with(root.join("AIData/bootstrap/python"))
        );
        assert_eq!(bootstrap.prepare().await.unwrap(), runtime);
        assert!(!root.join("AIData/.venv").exists());
        let unsupported = ManagedRuntimeBootstrap {
            python_version: "3.13.5".into(),
            ..bootstrap
        };
        assert!(matches!(
            unsupported.prepare().await,
            Err(ManagedRuntimeError::UnsupportedPython(_))
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
