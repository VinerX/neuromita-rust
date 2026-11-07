//! Resolve and publish plugin packages, then assess concrete model asset readiness.
//! Runtime model initialization remains a separate, explicit worker operation.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use super::{
    layer::{valid_segment, validate_lock},
    overlay::{InstallerAction, LockedOverlay, LockedPackage, OverlayError, OverlayInstaller},
    resolver::{DependencyResolver, ResolveError},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginInstallPlan {
    #[serde(default)]
    pub packages: Vec<LockedPackage>,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub assets: Vec<ModelAssetRequirement>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub supported_backends: Vec<String>,
    #[serde(default)]
    pub required_backends: Vec<String>,
    #[serde(default)]
    pub supported_python_minors: Vec<String>,
    #[serde(default)]
    pub actions: Vec<InstallerAction>,
    #[serde(default)]
    pub asset_hooks: Vec<AssetHook>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AssetHook {
    pub id: String,
    pub source_plugin_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelAssetRequirement {
    pub role: String,
    pub relative_path: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
    #[serde(default)]
    pub sha256: Option<String>,
}

fn required_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetState {
    Present,
    Verified,
    Missing,
    HashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAssetStatus {
    pub role: String,
    pub relative_path: String,
    pub required: bool,
    pub state: AssetState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallResult {
    pub overlay: LockedOverlay,
    pub overlay_path: PathBuf,
    pub assets: Vec<ModelAssetStatus>,
}

impl PluginInstallResult {
    pub fn ready_for_initialization(&self) -> bool {
        self.assets.iter().all(|asset| {
            !asset.required || matches!(asset.state, AssetState::Present | AssetState::Verified)
        })
    }
}

#[derive(Debug, Error)]
pub enum PluginInstallError {
    #[error("invalid Python plugin install plan: {0}")]
    Invalid(String),
    #[error("selected backend does not match plugin requirement: {0}")]
    Backend(String),
    #[error("dependency resolution failed: {0}")]
    Resolve(#[from] ResolveError),
    #[error("overlay publication failed: {0}")]
    Overlay(#[from] OverlayError),
    #[error("asset inspection failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct PluginInstallHandler {
    pub resolver: DependencyResolver,
    pub overlays: OverlayInstaller,
    pub assets_directory: PathBuf,
}

pub struct PluginInstallRequest<'a> {
    pub plugin_id: &'a str,
    pub revision: &'a str,
    pub python_version: &'a str,
    pub selected_backend: Option<&'a str>,
    pub plan: &'a PluginInstallPlan,
    pub backend_packages: &'a [LockedPackage],
}

impl PluginInstallPlan {
    pub fn validate(&self, python_version: &str) -> Result<(), PluginInstallError> {
        validate_lock(&self.packages, python_version)
            .map_err(|error| PluginInstallError::Invalid(error.to_string()))?;
        let minor = python_version
            .rsplit_once('.')
            .map(|(minor, _)| minor)
            .unwrap_or("");
        if self.supported_python_minors.len() > 8
            || self.supported_python_minors.iter().any(|version| {
                let mut parts = version.split('.');
                !matches!((parts.next(), parts.next(), parts.next()),
                    (Some(major), Some(minor), None) if !major.is_empty() && !minor.is_empty()
                        && major.bytes().all(|b| b.is_ascii_digit())
                        && minor.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            return Err(PluginInstallError::Invalid(
                "invalid supported Python versions".into(),
            ));
        }
        if !self.supported_python_minors.is_empty()
            && !self
                .supported_python_minors
                .iter()
                .any(|version| version == minor)
        {
            return Err(PluginInstallError::Invalid(format!(
                "Python {python_version} is incompatible with plugin (supported: {})",
                self.supported_python_minors.join(", ")
            )));
        }
        if !self.packages.is_empty() && !self.requirements.is_empty() {
            return Err(PluginInstallError::Invalid(
                "use either exact packages or requirements".into(),
            ));
        }
        for requirement in &self.requirements {
            super::resolver::validate_requirement(requirement)
                .map_err(|error| PluginInstallError::Invalid(error.to_string()))?;
        }
        if let Some(backend) = &self.backend
            && !valid_segment(backend)
        {
            return Err(PluginInstallError::Invalid("unsafe backend name".into()));
        }
        if self.supported_backends.len() > 8
            || self.backend.is_some() && !self.supported_backends.is_empty()
            || self.supported_backends.iter().any(|backend| {
                !matches!(
                    backend.as_str(),
                    "torch-cpu"
                        | "torch-cuda"
                        | "onnx-cpu"
                        | "onnx-cuda"
                        | "onnx-directml"
                        | "ctranslate2-cpu"
                        | "ctranslate2-cuda"
                        | "remote-api"
                )
            })
        {
            return Err(PluginInstallError::Invalid(
                "invalid supported backends".into(),
            ));
        }
        if self.required_backends.len() > 4
            || self.required_backends.iter().any(|backend| {
                !matches!(
                    backend.as_str(),
                    "torch-cpu"
                        | "torch-cuda"
                        | "onnx-cpu"
                        | "onnx-cuda"
                        | "onnx-directml"
                        | "ctranslate2-cpu"
                        | "ctranslate2-cuda"
                        | "remote-api"
                )
            })
            || self.required_backends.iter().collect::<HashSet<_>>().len()
                != self.required_backends.len()
        {
            return Err(PluginInstallError::Invalid(
                "invalid supplemental backends".into(),
            ));
        }
        if self.asset_hooks.len() > 16
            || self
                .asset_hooks
                .iter()
                .any(|hook| !valid_segment(&hook.id) || !valid_segment(&hook.source_plugin_id))
            || self
                .asset_hooks
                .iter()
                .map(|hook| &hook.id)
                .collect::<HashSet<_>>()
                .len()
                != self.asset_hooks.len()
        {
            return Err(PluginInstallError::Invalid("invalid asset hooks".into()));
        }
        if self.assets.len() > 512 {
            return Err(PluginInstallError::Invalid("too many assets".into()));
        }
        let mut seen = HashSet::new();
        for asset in &self.assets {
            if !valid_segment(&asset.role) || !valid_relative_asset(&asset.relative_path) {
                return Err(PluginInstallError::Invalid(format!(
                    "invalid asset role or path: {}",
                    asset.role
                )));
            }
            if !seen.insert((asset.role.as_str(), asset.relative_path.as_str())) {
                return Err(PluginInstallError::Invalid(
                    "duplicate asset role and path".into(),
                ));
            }
            if let Some(hash) = &asset.sha256
                && (hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(PluginInstallError::Invalid("invalid asset SHA-256".into()));
            }
        }
        Ok(())
    }
}

fn valid_relative_asset(text: &str) -> bool {
    !text.is_empty()
        && !text.contains('\\')
        && !text.contains(':')
        && !text.chars().any(char::is_control)
        && Path::new(text).components().all(|part| match part {
            Component::Normal(segment) => !segment.is_empty() && segment != "." && segment != "..",
            _ => false,
        })
}

impl PluginInstallHandler {
    pub async fn install(
        &self,
        plugin_id: &str,
        revision: &str,
        python_version: &str,
        selected_backend: Option<&str>,
        plan: &PluginInstallPlan,
        backend_packages: &[LockedPackage],
    ) -> Result<PluginInstallResult, PluginInstallError> {
        self.install_with_approval(
            PluginInstallRequest {
                plugin_id,
                revision,
                python_version,
                selected_backend,
                plan,
                backend_packages,
            },
            &HashSet::new(),
        )
        .await
    }

    pub async fn install_with_approval(
        &self,
        request: PluginInstallRequest<'_>,
        approved_actions: &HashSet<String>,
    ) -> Result<PluginInstallResult, PluginInstallError> {
        let PluginInstallRequest {
            plugin_id,
            revision,
            python_version,
            selected_backend,
            plan,
            backend_packages,
        } = request;
        plan.validate(python_version)?;
        if !valid_segment(plugin_id) || !valid_segment(revision) {
            return Err(PluginInstallError::Invalid(
                "unsafe plugin id or revision".into(),
            ));
        }
        if let Some(required) = &plan.backend
            && selected_backend != Some(required.as_str())
        {
            return Err(PluginInstallError::Backend(required.clone()));
        }
        if !plan.supported_backends.is_empty()
            && !selected_backend
                .is_some_and(|backend| plan.supported_backends.iter().any(|x| x == backend))
        {
            return Err(PluginInstallError::Backend(format!(
                "supported: {}",
                plan.supported_backends.join(", ")
            )));
        }
        if !self.assets_directory.is_absolute() {
            return Err(PluginInstallError::Invalid(
                "absolute model asset root required".into(),
            ));
        }
        let resolved = if plan.requirements.is_empty() {
            self.resolver
                .resolve(&plan.packages, backend_packages, python_version)
                .await?
        } else {
            self.resolver
                .resolve_requirements(&plan.requirements, backend_packages, python_version)
                .await?
        };
        let overlay = LockedOverlay {
            plugin_id: plugin_id.into(),
            revision: revision.into(),
            python_version: python_version.into(),
            packages: resolved,
            actions: plan.actions.clone(),
        };
        let overlay_path = self
            .overlays
            .install_with_approval(&overlay, approved_actions)
            .await?;
        let assets = self
            .inspect_revision_assets(plugin_id, revision, plan)
            .await?;
        Ok(PluginInstallResult {
            overlay,
            overlay_path,
            assets,
        })
    }

    pub async fn inspect_revision_assets(
        &self,
        plugin_id: &str,
        revision: &str,
        plan: &PluginInstallPlan,
    ) -> Result<Vec<ModelAssetStatus>, PluginInstallError> {
        if !valid_segment(plugin_id)
            || !valid_segment(revision)
            || !self.assets_directory.is_absolute()
        {
            return Err(PluginInstallError::Invalid("unsafe model revision".into()));
        }
        let root = self.assets_directory.join(plugin_id).join(revision);
        if root.exists() {
            let store = super::assets::ModelAssetStore {
                root: self.assets_directory.clone(),
                max_asset_bytes: 4 * 1024 * 1024 * 1024,
                timeout: self.resolver.timeout,
            };
            store
                .verified_revision(plugin_id, revision)
                .await
                .map_err(|error| {
                    PluginInstallError::Invalid(format!("unverified model asset revision: {error}"))
                })?;
        }
        self.inspect_at(&root, plan).await
    }

    pub async fn inspect_assets(
        &self,
        plugin_id: &str,
        plan: &PluginInstallPlan,
    ) -> Result<Vec<ModelAssetStatus>, PluginInstallError> {
        if !valid_segment(plugin_id) || !self.assets_directory.is_absolute() {
            return Err(PluginInstallError::Invalid(
                "unsafe model asset root".into(),
            ));
        }
        let root = self.assets_directory.join(plugin_id);
        self.inspect_at(&root, plan).await
    }

    async fn inspect_at(
        &self,
        root: &Path,
        plan: &PluginInstallPlan,
    ) -> Result<Vec<ModelAssetStatus>, PluginInstallError> {
        if !safe_path_prefix(&self.assets_directory).await? || !safe_path_prefix(root).await? {
            return Err(PluginInstallError::Invalid(
                "asset directory contains symlinks".into(),
            ));
        }
        let mut result = Vec::with_capacity(plan.assets.len());
        for asset in &plan.assets {
            if !valid_segment(&asset.role) || !valid_relative_asset(&asset.relative_path) {
                return Err(PluginInstallError::Invalid("unsafe asset path".into()));
            }
            let path = root.join(&asset.relative_path);
            if !safe_path_prefix(&path).await? {
                return Err(PluginInstallError::Invalid(
                    "asset path contains symlinks".into(),
                ));
            }
            let state = match tokio::fs::metadata(&path).await {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => AssetState::Missing,
                Err(error) => return Err(error.into()),
                Ok(metadata) if !metadata.is_file() => {
                    return Err(PluginInstallError::Invalid(
                        "asset must be a regular file".into(),
                    ));
                }
                Ok(_) => {
                    if let Some(expected) = &asset.sha256 {
                        if hash_file(&path).await?.eq_ignore_ascii_case(expected) {
                            AssetState::Verified
                        } else {
                            AssetState::HashMismatch
                        }
                    } else {
                        AssetState::Present
                    }
                }
            };
            result.push(ModelAssetStatus {
                role: asset.role.clone(),
                relative_path: asset.relative_path.clone(),
                required: asset.required,
                state,
            });
        }
        Ok(result)
    }
}

async fn safe_path_prefix(path: &Path) -> Result<bool, std::io::Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
            Ok(_) => (),
        }
    }
    Ok(true)
}

async fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let mut context = Context::new(&SHA256);
    let mut file = tokio::fs::File::open(path).await?;
    let mut block = [0u8; 65536];
    loop {
        let len = file.read(&mut block).await?;
        if len == 0 {
            break;
        }
        context.update(&block[..len]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn asset(role: &str, path: &str, required: bool) -> ModelAssetRequirement {
        ModelAssetRequirement {
            role: role.into(),
            relative_path: path.into(),
            required,
            sha256: None,
        }
    }

    #[tokio::test]
    async fn builtin_tts_plan_rejects_incompatible_python_before_any_install() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let host = crate::ai_runtime::PythonHostConfig {
            python_executable: std::env::var("NEUROMITA_TEST_PYTHON")
                .unwrap_or_else(|_| "python3".into())
                .into(),
            sdk_root: root.clone(),
            plugins_directory: root.join("plugins/builtin"),
            models_directory: None,
            request_timeout: std::time::Duration::from_secs(6),
        };
        let plan = crate::ai_runtime::fetch_install_plan(&host, "silero_rvc_cuda")
            .await
            .unwrap();
        assert!(plan.validate("3.12.10").is_ok());
        assert!(matches!(
            plan.validate("3.13.5"),
            Err(PluginInstallError::Invalid(_))
        ));
        assert_eq!(plan.supported_backends, vec!["torch-cuda"]);
    }

    #[test]
    fn rejects_path_traversal_and_duplicate_assets() {
        let mut plan = PluginInstallPlan {
            asset_hooks: vec![],
            packages: vec![],
            requirements: vec![],
            assets: vec![asset("rvc", "../voice.pth", true)],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
        };
        assert!(plan.validate("3.13.5").is_err());
        plan.assets = vec![asset("rvc", r"C:\windows\system.ini", true)];
        assert!(plan.validate("3.13.5").is_err());
        plan.assets = vec![
            asset("rvc", "voice.pth", true),
            asset("rvc", "voice.pth", true),
        ];
        assert!(plan.validate("3.13.5").is_err());
        plan.assets.pop();
        assert!(plan.validate("3.13.5").is_ok());
    }

    #[tokio::test]
    async fn asset_probe_distinguishes_optional_missing_and_corrupt_hash() {
        let root = std::env::temp_dir().join(format!("nm-assets-{}", Uuid::new_v4()));
        let plugin = root.join("mock_tts");
        tokio::fs::create_dir_all(plugin.join("rvc")).await.unwrap();
        tokio::fs::write(plugin.join("rvc/voice.pth"), b"test-weights")
            .await
            .unwrap();
        let mut present = asset("rvc_weights", "rvc/voice.pth", true);
        present.sha256 = Some(hash_file(&plugin.join("rvc/voice.pth")).await.unwrap());
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            packages: vec![],
            requirements: vec![],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            assets: vec![
                present.clone(),
                asset("reference_audio", "ref/voice.wav", false),
                asset("voice_index", "rvc/voice.index", true),
            ],
            supported_python_minors: vec![],
            actions: vec![],
        };
        let handler = PluginInstallHandler {
            resolver: DependencyResolver {
                uv_executable: root.join("uv"),
                python_executable: root.join("python"),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(2),
            },
            overlays: OverlayInstaller {
                uv_executable: root.join("uv"),
                python_executable: root.join("python"),
                overlays_directory: root.join("overlays"),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(2),
            },
            assets_directory: root.clone(),
        };
        let first = handler.inspect_assets("mock_tts", &plan).await.unwrap();
        assert_eq!(first[0].state, AssetState::Verified);
        assert_eq!(first[1].state, AssetState::Missing);
        assert_eq!(first[2].state, AssetState::Missing);
        present.sha256 = Some("0".repeat(64));
        let mismatched = handler
            .inspect_assets(
                "mock_tts",
                &PluginInstallPlan {
                    asset_hooks: vec![],
                    assets: vec![present],
                    packages: vec![],
                    requirements: vec![],
                    backend: None,
                    supported_backends: vec![],
                    required_backends: vec![],
                    supported_python_minors: vec![],
                    actions: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(mismatched[0].state, AssetState::HashMismatch);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn reads_a_plugin_install_contract_without_inference_initialization() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let python = std::env::var("NEUROMITA_TEST_PYTHON")
            .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.to_owned());
        let host = crate::ai_runtime::PythonHostConfig {
            python_executable: python.into(),
            sdk_root: root.clone(),
            plugins_directory: root.join("plugins/builtin"),
            models_directory: None,
            request_timeout: std::time::Duration::from_secs(10),
        };
        let plan = crate::ai_runtime::fetch_install_plan(&host, "mock_tts")
            .await
            .unwrap();
        assert!(plan.packages.is_empty());
        assert!(plan.assets.is_empty());
        assert!(
            crate::ai_runtime::fetch_install_plan(&host, "../mock_tts")
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolves_model_dependencies_and_keeps_missing_assets_unready() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("nm-plan-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(
            &uv,
            r#"#!/bin/sh
if [ "$1" != pip ]; then exit 2; fi
if [ "$2" = compile ]; then
    while [ $# -gt 0 ]; do
        if [ "$1" = --output-file ]; then shift; out="$1"; fi
        shift
    done
    printf 'torch==2.8.0\ntransformers==4.45.0\ntokenizers==0.21.0\n' > "$out"
    exit 0
fi
if [ "$2" = install ]; then
    target=''
    for arg do
        case "$prev" in --target) target="$arg";; esac
        prev="$arg"
    done
    for arg do
        case "$arg" in
            transformers==*) mkdir -p "$target/transformers-4.45.0.dist-info"; printf 'Name: transformers\nVersion: 4.45.0\n' > "$target/transformers-4.45.0.dist-info/METADATA";;
            tokenizers==*) mkdir -p "$target/tokenizers-0.21.0.dist-info"; printf 'Name: tokenizers\nVersion: 0.21.0\n' > "$target/tokenizers-0.21.0.dist-info/METADATA";;
        esac
    done
    exit 0
fi
exit 3
"#,
        )
        .await
        .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let python = root.join("python");
        tokio::fs::write(&python, "fake").await.unwrap();
        let handler = PluginInstallHandler {
            resolver: DependencyResolver {
                uv_executable: uv.clone(),
                python_executable: python.clone(),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(10),
            },
            overlays: OverlayInstaller {
                uv_executable: uv,
                python_executable: python,
                overlays_directory: root.join("overlays"),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(10),
            },
            assets_directory: root.join("models"),
        };
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            backend: Some("torch-cpu".into()),
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec!["3.12".into(), "3.13".into()],
            actions: vec![],
            packages: vec![LockedPackage {
                name: "transformers".into(),
                version: "4.45.0".into(),
            }],
            requirements: vec![],
            assets: vec![asset("weights", "model.safetensors", true)],
        };
        let constraints = [LockedPackage {
            name: "torch".into(),
            version: "2.8.0".into(),
        }];
        assert!(matches!(
            handler
                .install("f5", "v1", "3.13.5", Some("onnx-cpu"), &plan, &constraints)
                .await,
            Err(PluginInstallError::Backend(_))
        ));
        let result = handler
            .install("f5", "v1", "3.13.5", Some("torch-cpu"), &plan, &constraints)
            .await
            .unwrap();
        assert_eq!(result.overlay.packages.len(), 2);
        assert!(!result.overlay.packages.iter().any(|p| p.name == "torch"));
        assert!(
            result
                .overlay_path
                .join(".neuromita-overlay.json")
                .is_file()
        );
        assert!(!result.ready_for_initialization());
        let source = root.join("model.safetensors");
        tokio::fs::write(&source, b"weights").await.unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, b"weights");
        let sha: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        super::super::assets::ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 1024,
            timeout: std::time::Duration::from_secs(5),
        }
        .publish(
            "f5",
            "v1",
            &plan,
            &[super::super::assets::AssetInput {
                role: "weights".into(),
                relative_path: "model.safetensors".into(),
                sha256: sha,
                source: super::super::assets::AssetSource::LocalFile(source),
            }],
        )
        .await
        .unwrap();
        let result = handler
            .install("f5", "v1", "3.13.5", Some("torch-cpu"), &plan, &constraints)
            .await
            .unwrap();
        assert!(result.ready_for_initialization());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_symlinked_assets() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("nm-asset-link-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("mock_tts"))
            .await
            .unwrap();
        symlink("/etc/passwd", root.join("mock_tts/voice.wav")).unwrap();
        let handler = PluginInstallHandler {
            resolver: DependencyResolver {
                uv_executable: root.join("uv"),
                python_executable: root.join("python"),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(2),
            },
            overlays: OverlayInstaller {
                uv_executable: root.join("uv"),
                python_executable: root.join("python"),
                overlays_directory: root.join("overlays"),
                cache_directory: root.join("cache"),
                timeout: std::time::Duration::from_secs(2),
            },
            assets_directory: root.clone(),
        };
        let plan = PluginInstallPlan {
            asset_hooks: vec![],
            packages: vec![],
            requirements: vec![],
            assets: vec![asset("reference", "voice.wav", true)],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
        };
        assert!(matches!(
            handler.inspect_assets("mock_tts", &plan).await,
            Err(PluginInstallError::Invalid(_))
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
