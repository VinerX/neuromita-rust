//! Pinned uv bootstrap: a verified archive is the only source of an executable.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};
use uuid::Uuid;

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvArchiveKind {
    TarGz,
    Zip,
}

#[derive(Debug, Clone)]
pub struct UvDistribution {
    pub version: String,
    pub archive_url: String,
    pub archive_sha256: String,
    pub archive_kind: UvArchiveKind,
}

#[derive(Debug, Clone)]
pub struct UvInstaller {
    pub data_directory: PathBuf,
    pub distribution: UvDistribution,
    pub timeout: Duration,
}

#[derive(Debug, Error)]
pub enum UvInstallError {
    #[error("invalid uv bootstrap configuration: {0}")]
    Invalid(String),
    #[error("uv bootstrap filesystem failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv download failure: {0}")]
    Download(String),
    #[error("uv checksum mismatch")]
    Checksum,
    #[error("uv archive extraction or version validation failed: {0}")]
    Verify(String),
    #[error("uv bootstrap timed out")]
    Timeout,
    #[error("existing uv revision is incomplete or corrupted")]
    Corrupted,
}

#[derive(Debug, Serialize, Deserialize)]
struct UvReceipt {
    version: String,
    archive_sha256: String,
    executable_sha256: String,
}

fn hex_digest(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}

#[cfg(all(test, unix))]
fn sha256(bytes: &[u8]) -> String {
    hex_digest(ring::digest::digest(&SHA256, bytes).as_ref())
}

async fn hash_file(path: &Path) -> Result<String, UvInstallError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0; 65536];
    loop {
        let amount = file.read(&mut buffer).await?;
        if amount == 0 {
            break;
        }
        context.update(&buffer[..amount]);
    }
    Ok(hex_digest(context.finish().as_ref()))
}

impl UvInstaller {
    pub fn uv_directory(&self) -> PathBuf {
        self.data_directory
            .join("bootstrap")
            .join("uv")
            .join(&self.distribution.version)
    }

    pub fn uv_executable(&self) -> PathBuf {
        self.uv_directory()
            .join(if cfg!(windows) { "uv.exe" } else { "uv" })
    }

    fn validate(&self) -> Result<(), UvInstallError> {
        let dist = &self.distribution;
        if !self.data_directory.is_absolute() || self.timeout.is_zero() {
            return Err(UvInstallError::Invalid(
                "absolute AIData directory and positive timeout required".into(),
            ));
        }
        if dist.version.is_empty()
            || !dist
                .version
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err(UvInstallError::Invalid(
                "uv version must be numerically pinned".into(),
            ));
        }
        if dist.archive_sha256.len() != 64
            || !dist.archive_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(UvInstallError::Invalid(
                "expected archive SHA-256 must be 64 hex digits".into(),
            ));
        }
        if !dist.archive_url.starts_with("https://") && !dist.archive_url.starts_with("file://") {
            return Err(UvInstallError::Invalid(
                "only HTTPS URLs or local file:// archives supported".into(),
            ));
        }
        if cfg!(windows) != (dist.archive_kind == UvArchiveKind::Zip) {
            return Err(UvInstallError::Invalid(
                "archive type is unsupported on this platform".into(),
            ));
        }
        Ok(())
    }

    pub async fn probe(&self) -> Result<Option<PathBuf>, UvInstallError> {
        self.validate()?;
        let root = self.uv_directory();
        if !root.exists() {
            return Ok(None);
        }
        let receipt: UvReceipt = serde_json::from_slice(
            &tokio::fs::read(root.join("receipt.json"))
                .await
                .map_err(|_| UvInstallError::Corrupted)?,
        )
        .map_err(|_| UvInstallError::Corrupted)?;
        if receipt.version != self.distribution.version
            || receipt.archive_sha256 != self.distribution.archive_sha256.to_ascii_lowercase()
            || !self.uv_executable().is_file()
            || hash_file(&self.uv_executable())
                .await
                .map_err(|_| UvInstallError::Corrupted)?
                != receipt.executable_sha256
        {
            return Err(UvInstallError::Corrupted);
        }
        Ok(Some(self.uv_executable()))
    }

    async fn download(&self, path: &Path) -> Result<(), UvInstallError> {
        let mut output = tokio::fs::File::create(path).await?;
        let mut context = Context::new(&SHA256);
        let mut total = 0_u64;
        if let Some(local) = self.distribution.archive_url.strip_prefix("file://") {
            let mut input = tokio::fs::File::open(local).await?;
            let mut buffer = [0; 65536];
            loop {
                let read = input.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                total += read as u64;
                if total > MAX_ARCHIVE_BYTES {
                    return Err(UvInstallError::Download("archive exceeds 64 MiB".into()));
                }
                context.update(&buffer[..read]);
                output.write_all(&buffer[..read]).await?;
            }
        } else {
            let client = reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .map_err(|e| UvInstallError::Download(e.to_string()))?;
            let mut response = client
                .get(&self.distribution.archive_url)
                .send()
                .await
                .map_err(|e| UvInstallError::Download(e.to_string()))?
                .error_for_status()
                .map_err(|e| UvInstallError::Download(e.to_string()))?;
            if response.url().scheme() != "https" {
                return Err(UvInstallError::Download(
                    "insecure download redirect".into(),
                ));
            }
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| UvInstallError::Download(e.to_string()))?
            {
                total += chunk.len() as u64;
                if total > MAX_ARCHIVE_BYTES {
                    return Err(UvInstallError::Download("archive exceeds 64 MiB".into()));
                }
                context.update(&chunk);
                output.write_all(&chunk).await?;
            }
        }
        output.flush().await?;
        if hex_digest(context.finish().as_ref())
            != self.distribution.archive_sha256.to_ascii_lowercase()
        {
            return Err(UvInstallError::Checksum);
        }
        Ok(())
    }

    #[cfg(unix)]
    async fn extract(&self, archive: &Path, destination: &Path) -> Result<(), UvInstallError> {
        use std::os::unix::fs::PermissionsExt;
        // Read a single archive member to stdout; tar never extracts arbitrary filesystem paths.
        let listing = Command::new("tar")
            .arg("-tzf")
            .arg(archive)
            .kill_on_drop(true)
            .output()
            .await?;
        if !listing.status.success() {
            return Err(UvInstallError::Verify("invalid tar archive".into()));
        }
        let listing =
            String::from_utf8(listing.stdout).map_err(|e| UvInstallError::Verify(e.to_string()))?;
        let member = listing
            .lines()
            .find(|path| *path == "uv" || path.ends_with("/uv") && !path.contains(".."))
            .ok_or_else(|| UvInstallError::Verify("missing uv archive member".into()))?;
        let output = Command::new("tar")
            .arg("-xOzf")
            .arg(archive)
            .arg(member)
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success()
            || output.stdout.is_empty()
            || output.stdout.len() > 128 * 1024 * 1024
        {
            return Err(UvInstallError::Verify(
                "invalid extracted executable".into(),
            ));
        }
        let executable = destination.join("uv");
        tokio::fs::write(&executable, &output.stdout).await?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    #[cfg(windows)]
    async fn extract(&self, archive: &Path, destination: &Path) -> Result<(), UvInstallError> {
        let command = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}'",
            archive.display().to_string().replace("'", "''"),
            destination.display().to_string().replace("'", "''")
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() {
            return Err(UvInstallError::Verify("Expand-Archive failed".into()));
        }
        if !destination.join("uv.exe").is_file() {
            return Err(UvInstallError::Verify("missing uv.exe".into()));
        }
        Ok(())
    }

    pub async fn prepare(&self) -> Result<PathBuf, UvInstallError> {
        self.validate()?;
        if let Some(executable) = self.probe().await? {
            return Ok(executable);
        }
        let root = self.uv_directory();
        let parent = root
            .parent()
            .ok_or_else(|| UvInstallError::Invalid("missing parent".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        let stage = parent.join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&stage).await?;
        let operation = async {
            let archive = stage.join(match self.distribution.archive_kind {
                UvArchiveKind::Zip => "source.zip",
                UvArchiveKind::TarGz => "source.tar.gz",
            });
            timeout(self.timeout, self.download(&archive))
                .await
                .map_err(|_| UvInstallError::Timeout)??;
            let extracted = stage.join("extracted");
            tokio::fs::create_dir(&extracted).await?;
            timeout(self.timeout, self.extract(&archive, &extracted))
                .await
                .map_err(|_| UvInstallError::Timeout)??;
            let executable = extracted.join(if cfg!(windows) { "uv.exe" } else { "uv" });
            let output = timeout(
                self.timeout,
                Command::new(&executable)
                    .arg("--version")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| UvInstallError::Timeout)??;
            let version = String::from_utf8_lossy(&output.stdout);
            if !output.status.success()
                || !version.starts_with(&format!("uv {} ", self.distribution.version))
                    && version.trim() != format!("uv {}", self.distribution.version)
            {
                return Err(UvInstallError::Verify(
                    "uv executable version mismatch".into(),
                ));
            }
            let receipt = UvReceipt {
                version: self.distribution.version.clone(),
                archive_sha256: self.distribution.archive_sha256.to_ascii_lowercase(),
                executable_sha256: hash_file(&executable).await?,
            };
            tokio::fs::write(
                extracted.join("receipt.json"),
                serde_json::to_vec_pretty(&receipt)
                    .map_err(|e| UvInstallError::Verify(e.to_string()))?,
            )
            .await?;
            tokio::fs::remove_file(&archive).await?;
            tokio::fs::rename(&extracted, &root).await?;
            Ok(root.join(if cfg!(windows) { "uv.exe" } else { "uv" }))
        }
        .await;
        let _ = tokio::fs::remove_dir_all(stage).await;
        operation
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn fixture() -> (UvInstaller, PathBuf) {
        let root = std::env::temp_dir().join(format!("nm-uv-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let payload = root.join("uv");
        tokio::fs::write(&payload, "#!/bin/sh\necho 'uv 0.13.4'\n")
            .await
            .unwrap();
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).unwrap();
        let archive = root.join("uv.tar.gz");
        assert!(
            Command::new("tar")
                .args(["-czf"])
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
        let digest = hash_file(&archive).await.unwrap();
        (
            UvInstaller {
                data_directory: root.join("AIData"),
                distribution: UvDistribution {
                    version: "0.13.4".into(),
                    archive_url: format!("file://{}", archive.display()),
                    archive_sha256: digest,
                    archive_kind: UvArchiveKind::TarGz,
                },
                timeout: Duration::from_secs(5),
            },
            root,
        )
    }

    #[tokio::test]
    async fn installs_and_reuses_verified_executable() {
        let (installer, root) = fixture().await;
        let uv = installer.prepare().await.unwrap();
        assert!(uv.is_file());
        assert_eq!(installer.prepare().await.unwrap(), uv);
        tokio::fs::write(&uv, b"tampered").await.unwrap();
        assert!(matches!(
            installer.probe().await,
            Err(UvInstallError::Corrupted)
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_wrong_digest_without_publish() {
        let (mut installer, root) = fixture().await;
        installer.distribution.archive_sha256 = sha256(b"wrong source");
        assert!(matches!(
            installer.prepare().await,
            Err(UvInstallError::Checksum)
        ));
        assert!(!installer.uv_directory().exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
