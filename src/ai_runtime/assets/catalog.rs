//! User-approved, versioned source manifests for immutable model assets.
//! The catalog never imports a plugin or executes download hooks.

use std::{collections::HashSet, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{AssetInput, AssetSource, asset_path, valid_hash};
use crate::ai_runtime::{install::PluginInstallPlan, layer::valid_segment};

const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("invalid model source catalog: {0}")]
    Invalid(String),
    #[error("no verified sources for {plugin_id}/{revision}")]
    Missing { plugin_id: String, revision: String },
    #[error("model source catalog filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("model source catalog JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSourceCatalog {
    pub schema_version: u32,
    pub entries: Vec<SourceRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRevision {
    pub plugin_id: String,
    pub revision: String,
    pub assets: Vec<SourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub role: String,
    pub relative_path: String,
    pub sha256: String,
    pub source: SourceLocation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceLocation {
    Https { url: String },
    LocalFile { path: String },
}

impl ModelSourceCatalog {
    pub async fn load(path: &Path) -> Result<Self, CatalogError> {
        if !path.is_absolute() {
            return Err(CatalogError::Invalid(
                "catalog path must be absolute".into(),
            ));
        }
        super::reject_symlinks(path)
            .await
            .map_err(|error| CatalogError::Invalid(error.to_string()))?;
        let info = tokio::fs::symlink_metadata(path).await?;
        if !info.is_file() || info.file_type().is_symlink() || info.len() > MAX_CATALOG_BYTES {
            return Err(CatalogError::Invalid(
                "catalog must be a bounded regular file".into(),
            ));
        }
        let catalog: Self = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema_version != 1 || self.entries.len() > 512 {
            return Err(CatalogError::Invalid(
                "unsupported schema or catalog size".into(),
            ));
        }
        let mut revisions = HashSet::new();
        for entry in &self.entries {
            if !valid_segment(&entry.plugin_id)
                || !valid_segment(&entry.revision)
                || !revisions.insert((entry.plugin_id.as_str(), entry.revision.as_str()))
                || entry.assets.len() > 512
            {
                return Err(CatalogError::Invalid(
                    "invalid or duplicated revision".into(),
                ));
            }
            let mut paths = HashSet::new();
            for asset in &entry.assets {
                if !valid_segment(&asset.role)
                    || !asset_path(&asset.relative_path)
                    || !valid_hash(&asset.sha256)
                    || !paths.insert((asset.role.as_str(), asset.relative_path.as_str()))
                {
                    return Err(CatalogError::Invalid(
                        "invalid or duplicated asset entry".into(),
                    ));
                }
                match &asset.source {
                    SourceLocation::Https { url } => {
                        let parsed = reqwest::Url::parse(url)
                            .map_err(|_| CatalogError::Invalid("invalid asset URL".into()))?;
                        if parsed.scheme() != "https"
                            || parsed.host_str().is_none()
                            || !parsed.username().is_empty()
                            || parsed.password().is_some()
                            || parsed.fragment().is_some()
                            || url.len() > 2048
                        {
                            return Err(CatalogError::Invalid(
                                "HTTPS URL must not contain credentials or fragments".into(),
                            ));
                        }
                    }
                    SourceLocation::LocalFile { path } => {
                        if !Path::new(path).is_absolute()
                            || path.contains('\0')
                            || path.len() > 4096
                        {
                            return Err(CatalogError::Invalid(
                                "local asset source must be absolute".into(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        plugin_id: &str,
        revision: &str,
        plan: &PluginInstallPlan,
    ) -> Result<Vec<AssetInput>, CatalogError> {
        self.validate()?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.plugin_id == plugin_id && entry.revision == revision)
            .ok_or_else(|| CatalogError::Missing {
                plugin_id: plugin_id.into(),
                revision: revision.into(),
            })?;
        for required in &plan.assets {
            let source = entry.assets.iter().find(|asset| {
                asset.role == required.role && asset.relative_path == required.relative_path
            });
            if required.required && source.is_none() {
                return Err(CatalogError::Invalid(format!(
                    "missing required source {}",
                    required.relative_path
                )));
            }
            if let (Some(expected), Some(source)) = (&required.sha256, source)
                && !source.sha256.eq_ignore_ascii_case(expected)
            {
                return Err(CatalogError::Invalid(format!(
                    "catalog checksum differs from install plan for {}",
                    required.relative_path
                )));
            }
        }
        if entry.assets.iter().any(|source| {
            !plan.assets.iter().any(|expected| {
                expected.role == source.role && expected.relative_path == source.relative_path
            })
        }) {
            return Err(CatalogError::Invalid(
                "catalog contains unrequested model assets".into(),
            ));
        }
        Ok(entry
            .assets
            .iter()
            .map(|record| AssetInput {
                role: record.role.clone(),
                relative_path: record.relative_path.clone(),
                sha256: record.sha256.clone(),
                source: match &record.source {
                    SourceLocation::Https { url } => AssetSource::Https(url.clone()),
                    SourceLocation::LocalFile { path } => AssetSource::LocalFile(path.into()),
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::install::ModelAssetRequirement;

    fn catalog() -> ModelSourceCatalog {
        ModelSourceCatalog {
            schema_version: 1,
            entries: vec![SourceRevision {
                plugin_id: "mock_tts".into(),
                revision: "v1".into(),
                assets: vec![SourceRecord {
                    role: "weights".into(),
                    relative_path: "weights.bin".into(),
                    sha256: "a".repeat(64),
                    source: SourceLocation::Https {
                        url: "https://example.org/weights.bin".into(),
                    },
                }],
            }],
        }
    }
    fn plan() -> PluginInstallPlan {
        PluginInstallPlan {
            packages: vec![],
            requirements: vec![],
            assets: vec![ModelAssetRequirement {
                role: "weights".into(),
                relative_path: "weights.bin".into(),
                required: true,
                sha256: None,
            }],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
            asset_hooks: vec![],
        }
    }

    #[test]
    fn catalog_requires_exact_revision_and_complete_manifest() {
        assert_eq!(
            catalog().resolve("mock_tts", "v1", &plan()).unwrap().len(),
            1
        );
        assert!(catalog().resolve("mock_tts", "v2", &plan()).is_err());
        let mut invalid = catalog();
        invalid.entries[0].assets.clear();
        assert!(invalid.resolve("mock_tts", "v1", &plan()).is_err());
    }

    #[tokio::test]
    async fn local_catalog_publishes_and_reverifies_exact_model_revision() {
        use crate::ai_runtime::assets::ModelAssetStore;
        use std::time::Duration;
        use uuid::Uuid;
        let root = std::env::temp_dir().join(format!("nm-catalog-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let weights = root.join("weights.bin");
        tokio::fs::write(&weights, b"verified weights")
            .await
            .unwrap();
        let hash = ring::digest::digest(&ring::digest::SHA256, b"verified weights")
            .as_ref()
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>();
        let mut sources = catalog();
        sources.entries[0].assets[0].sha256 = hash;
        sources.entries[0].assets[0].source = SourceLocation::LocalFile {
            path: weights.to_string_lossy().into_owned(),
        };
        let file = root.join("catalog.json");
        tokio::fs::write(&file, serde_json::to_vec(&sources).unwrap())
            .await
            .unwrap();
        let loaded = ModelSourceCatalog::load(&file).await.unwrap();
        let store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 1_000,
            timeout: Duration::from_secs(5),
        };
        let selected = loaded.resolve("mock_tts", "v1", &plan()).unwrap();
        store
            .publish("mock_tts", "v1", &plan(), &selected)
            .await
            .unwrap();
        store.verified_revision("mock_tts", "v1").await.unwrap();
        tokio::fs::write(root.join("models/mock_tts/v1/weights.bin"), b"tampered")
            .await
            .unwrap();
        assert!(store.verified_revision("mock_tts", "v1").await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn catalog_rejects_duplicates_credentials_and_hash_mismatch() {
        let mut duplicate = catalog();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert!(duplicate.validate().is_err());
        let mut credentials = catalog();
        credentials.entries[0].assets[0].source = SourceLocation::Https {
            url: "https://user:pass@example.com/a".into(),
        };
        assert!(credentials.validate().is_err());
        let mut wrong_hash = plan();
        wrong_hash.assets[0].sha256 = Some("b".repeat(64));
        assert!(catalog().resolve("mock_tts", "v1", &wrong_hash).is_err());
    }
}
