//! Immutable, hash-verified model asset revisions; downloads never touch an active revision.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use uuid::Uuid;

use super::{
    install::{ModelAssetRequirement, PluginInstallPlan},
    layer::valid_segment,
};

const RECEIPT: &str = ".neuromita-assets.json";

pub mod catalog;
mod generation;
pub use generation::{ModelGenerationError, ModelGenerationRequest};

#[derive(Debug, Clone)]
pub enum AssetSource {
    LocalFile(PathBuf),
    Https(String),
}

#[derive(Debug, Clone)]
pub struct AssetInput {
    pub role: String,
    pub relative_path: String,
    pub sha256: String,
    pub source: AssetSource,
}

#[derive(Debug, Clone)]
pub struct ModelAssetStore {
    pub root: PathBuf,
    pub max_asset_bytes: u64,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct VerifiedAsset {
    role: String,
    relative_path: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AssetReceipt {
    plugin_id: String,
    revision: String,
    assets: Vec<VerifiedAsset>,
}

#[derive(Debug, Error)]
pub enum ModelAssetError {
    #[error("invalid model asset request: {0}")]
    Invalid(String),
    #[error("model asset revision already exists with different or corrupted contents")]
    Conflict,
    #[error("model asset checksum mismatch: {0}")]
    Checksum(String),
    #[error("model asset exceeded size limit")]
    TooLarge,
    #[error("model asset transfer timed out")]
    Timeout,
    #[error("model asset transport error: {0}")]
    Transport(String),
    #[error("model asset filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path.bytes().all(|b| !b.is_ascii_control())
}

async fn reject_symlinks(path: &Path) -> Result<(), ModelAssetError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ModelAssetError::Invalid(
                    "asset destination contains a symlink".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
    }
    Ok(())
}

async fn hash_file(path: &Path, limit: u64) -> Result<String, ModelAssetError> {
    let mut input = tokio::fs::File::open(path).await?;
    let mut digest = Context::new(&SHA256);
    let mut total = 0;
    let mut block = [0u8; 65536];
    loop {
        let size = input.read(&mut block).await?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > limit {
            return Err(ModelAssetError::TooLarge);
        }
        digest.update(&block[..size]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

impl ModelAssetStore {
    pub fn revision_path(
        &self,
        plugin_id: &str,
        revision: &str,
    ) -> Result<PathBuf, ModelAssetError> {
        if !self.root.is_absolute()
            || !valid_segment(plugin_id)
            || !valid_segment(revision)
            || self.max_asset_bytes == 0
            || self.timeout.is_zero()
        {
            return Err(ModelAssetError::Invalid(
                "absolute root, safe IDs, positive size and timeout required".into(),
            ));
        }
        Ok(self.root.join(plugin_id).join(revision))
    }

    async fn checked_existing(
        &self,
        path: &Path,
        wanted: &AssetReceipt,
    ) -> Result<bool, ModelAssetError> {
        if !path.exists() {
            return Ok(false);
        }
        reject_symlinks(path).await?;
        let existing: AssetReceipt = serde_json::from_slice(
            &tokio::fs::read(path.join(RECEIPT))
                .await
                .map_err(|_| ModelAssetError::Conflict)?,
        )
        .map_err(|_| ModelAssetError::Conflict)?;
        if existing != *wanted {
            return Err(ModelAssetError::Conflict);
        }
        for asset in &existing.assets {
            let file = path.join(&asset.relative_path);
            reject_symlinks(&file).await?;
            if !tokio::fs::metadata(&file)
                .await
                .map_err(|_| ModelAssetError::Conflict)?
                .is_file()
                || !hash_file(&file, self.max_asset_bytes)
                    .await
                    .map_err(|_| ModelAssetError::Conflict)?
                    .eq_ignore_ascii_case(&asset.sha256)
            {
                return Err(ModelAssetError::Conflict);
            }
        }
        Ok(true)
    }

    pub async fn verified_revision(
        &self,
        plugin_id: &str,
        revision: &str,
    ) -> Result<PathBuf, ModelAssetError> {
        let path = self.revision_path(plugin_id, revision)?;
        reject_symlinks(&path).await?;
        let receipt: AssetReceipt = serde_json::from_slice(
            &tokio::fs::read(path.join(RECEIPT))
                .await
                .map_err(|_| ModelAssetError::Conflict)?,
        )
        .map_err(|_| ModelAssetError::Conflict)?;
        if receipt.plugin_id != plugin_id
            || receipt.revision != revision
            || receipt.assets.iter().any(|asset| {
                !valid_segment(&asset.role)
                    || !asset_path(&asset.relative_path)
                    || !valid_hash(&asset.sha256)
            })
            || !self.checked_existing(&path, &receipt).await?
        {
            return Err(ModelAssetError::Conflict);
        }
        Ok(path)
    }

    async fn transfer(
        &self,
        from: &AssetSource,
        destination: &Path,
    ) -> Result<String, ModelAssetError> {
        let mut dest = tokio::fs::File::create(destination).await?;
        let mut digest = Context::new(&SHA256);
        let mut total = 0;
        match from {
            AssetSource::LocalFile(path) => {
                let meta = tokio::fs::symlink_metadata(path).await?;
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(ModelAssetError::Invalid(
                        "local asset source must be a regular file".into(),
                    ));
                }
                let mut input = tokio::fs::File::open(path).await?;
                let mut buf = [0; 65536];
                loop {
                    let n = input.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    total += n as u64;
                    if total > self.max_asset_bytes {
                        return Err(ModelAssetError::TooLarge);
                    }
                    digest.update(&buf[..n]);
                    dest.write_all(&buf[..n]).await?;
                }
            }
            AssetSource::Https(url) => {
                if !url.starts_with("https://") {
                    return Err(ModelAssetError::Invalid(
                        "only HTTPS downloads are supported".into(),
                    ));
                }
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| ModelAssetError::Transport(error.to_string()))?;
                let mut response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|error| ModelAssetError::Transport(error.to_string()))?;
                if !response.status().is_success() {
                    return Err(ModelAssetError::Transport(format!(
                        "HTTP status {}",
                        response.status()
                    )));
                }
                while let Some(bytes) = response
                    .chunk()
                    .await
                    .map_err(|error| ModelAssetError::Transport(error.to_string()))?
                {
                    total += bytes.len() as u64;
                    if total > self.max_asset_bytes {
                        return Err(ModelAssetError::TooLarge);
                    }
                    digest.update(&bytes);
                    dest.write_all(&bytes).await?;
                }
            }
        }
        dest.flush().await?;
        Ok(digest
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Sources must be explicitly supplied by the caller; plugin metadata cannot authorize downloads.
    pub async fn publish(
        &self,
        plugin_id: &str,
        revision: &str,
        plan: &PluginInstallPlan,
        sources: &[AssetInput],
    ) -> Result<PathBuf, ModelAssetError> {
        let destination = self.revision_path(plugin_id, revision)?;
        reject_symlinks(&destination).await?;
        if sources.len() > 512 || plan.assets.len() > 512 {
            return Err(ModelAssetError::Invalid("too many assets".into()));
        }
        let mut by_key = HashMap::new();
        for source in sources {
            if !valid_segment(&source.role)
                || !asset_path(&source.relative_path)
                || !valid_hash(&source.sha256)
                || by_key
                    .insert(
                        (source.role.as_str(), source.relative_path.as_str()),
                        source,
                    )
                    .is_some()
            {
                return Err(ModelAssetError::Invalid(
                    "invalid or duplicated source".into(),
                ));
            }
        }
        let mut retained = Vec::new();
        for requirement in &plan.assets {
            self.validate_asset(requirement, &by_key)?;
            if let Some(source) = by_key.remove(&(
                requirement.role.as_str(),
                requirement.relative_path.as_str(),
            )) {
                retained.push((
                    source,
                    VerifiedAsset {
                        role: requirement.role.clone(),
                        relative_path: requirement.relative_path.clone(),
                        sha256: source.sha256.to_ascii_lowercase(),
                    },
                ));
            } else if requirement.required {
                return Err(ModelAssetError::Invalid(format!(
                    "missing source for {}",
                    requirement.relative_path
                )));
            }
        }
        if !by_key.is_empty() {
            return Err(ModelAssetError::Invalid(
                "source not present in install plan".into(),
            ));
        }
        let receipt = AssetReceipt {
            plugin_id: plugin_id.into(),
            revision: revision.into(),
            assets: retained.iter().map(|(_, data)| data.clone()).collect(),
        };
        if self.checked_existing(&destination, &receipt).await? {
            return Ok(destination);
        }
        let parent = destination
            .parent()
            .ok_or_else(|| ModelAssetError::Invalid("missing parent".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&staging).await?;
        let outcome = async {
            for (source, asset) in retained {
                let output = staging.join(&asset.relative_path);
                tokio::fs::create_dir_all(
                    output
                        .parent()
                        .ok_or_else(|| ModelAssetError::Invalid("missing parent".into()))?,
                )
                .await?;
                let actual = timeout(self.timeout, self.transfer(&source.source, &output))
                    .await
                    .map_err(|_| ModelAssetError::Timeout)??;
                if actual != asset.sha256 {
                    return Err(ModelAssetError::Checksum(asset.relative_path));
                }
            }
            tokio::fs::write(
                staging.join(RECEIPT),
                serde_json::to_vec_pretty(&receipt)
                    .map_err(|error| ModelAssetError::Invalid(error.to_string()))?,
            )
            .await?;
            if destination.exists() {
                return Err(ModelAssetError::Conflict);
            }
            tokio::fs::rename(&staging, &destination).await?;
            Ok(destination)
        }
        .await;
        if outcome.is_err() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
        outcome
    }

    fn validate_asset(
        &self,
        asset: &ModelAssetRequirement,
        sources: &HashMap<(&str, &str), &AssetInput>,
    ) -> Result<(), ModelAssetError> {
        if !valid_segment(&asset.role) || !asset_path(&asset.relative_path) {
            return Err(ModelAssetError::Invalid("invalid model asset path".into()));
        }
        if let Some(expected) = &asset.sha256 {
            if !valid_hash(expected) {
                return Err(ModelAssetError::Invalid("invalid pinned SHA-256".into()));
            }
            if let Some(source) = sources.get(&(asset.role.as_str(), asset.relative_path.as_str()))
                && !source.sha256.eq_ignore_ascii_case(expected)
            {
                return Err(ModelAssetError::Invalid(
                    "source SHA-256 differs from install plan".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::install::PluginInstallPlan;

    #[tokio::test]
    async fn stages_verified_revision_and_rejects_corrupt_or_wrong_hash() {
        let root = std::env::temp_dir().join(format!("nm-asset-store-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let input = root.join("source.pth");
        tokio::fs::write(&input, b"weights").await.unwrap();
        let sha = hash_file(&input, 128).await.unwrap();
        let store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 128,
            timeout: Duration::from_secs(5),
        };
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            assets: vec![ModelAssetRequirement {
                role: "rvc".into(),
                relative_path: "rvc/voice.pth".into(),
                required: true,
                sha256: Some(sha.clone()),
            }],
            packages: vec![],
            requirements: vec![],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
        };
        let asset = AssetInput {
            role: "rvc".into(),
            relative_path: "rvc/voice.pth".into(),
            sha256: sha.clone(),
            source: AssetSource::LocalFile(input.clone()),
        };
        let published = store
            .publish("silero", "v1", &plan, std::slice::from_ref(&asset))
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(published.join("rvc/voice.pth"))
                .await
                .unwrap(),
            b"weights"
        );
        assert_eq!(
            store
                .publish("silero", "v1", &plan, std::slice::from_ref(&asset))
                .await
                .unwrap(),
            published
        );
        tokio::fs::write(published.join("rvc/voice.pth"), b"tampered")
            .await
            .unwrap();
        assert!(matches!(
            store
                .publish("silero", "v1", &plan, std::slice::from_ref(&asset))
                .await,
            Err(ModelAssetError::Conflict)
        ));
        let mut mismatch = asset;
        mismatch.sha256 = "0".repeat(64);
        assert!(matches!(
            store.publish("silero", "v2", &plan, &[mismatch]).await,
            Err(ModelAssetError::Invalid(_))
        ));
        assert!(!root.join("models/silero/v2").exists());
        assert!(store.publish("../escape", "v1", &plan, &[]).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlinked_destination_and_oversized_asset() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("nm-asset-store-link-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("models"))
            .await
            .unwrap();
        symlink("/tmp", root.join("models/link")).unwrap();
        let store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 2,
            timeout: Duration::from_secs(5),
        };
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            packages: vec![],
            requirements: vec![],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
            assets: vec![],
        };
        assert!(store.publish("link", "v1", &plan, &[]).await.is_err());
        let input = root.join("big.wav");
        tokio::fs::write(&input, b"huge payload").await.unwrap();
        let sha = hash_file(&input, 1024).await.unwrap();
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            assets: vec![ModelAssetRequirement {
                role: "wav".into(),
                relative_path: "ref.wav".into(),
                required: true,
                sha256: Some(sha.clone()),
            }],
            ..plan
        };
        let source = AssetInput {
            role: "wav".into(),
            relative_path: "ref.wav".into(),
            sha256: sha,
            source: AssetSource::LocalFile(input),
        };
        assert!(matches!(
            store.publish("valid", "v1", &plan, &[source]).await,
            Err(ModelAssetError::TooLarge)
        ));
        assert!(!root.join("models/valid/v1").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
