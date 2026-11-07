//! Immutable model-specific dependency overlays (never contain shared backend distributions).

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use super::layer::{LayerError, LayerInstaller, valid_segment, validate_lock};

pub type OverlayError = LayerError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedOverlay {
    pub plugin_id: String,
    pub revision: String,
    pub python_version: String,
    pub packages: Vec<LockedPackage>,
    #[serde(default)]
    pub actions: Vec<InstallerAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstallerAction {
    pub id: String,
    pub kind: String,
    pub relative_path: String,
}

#[derive(Clone)]
pub struct OverlayInstaller {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub overlays_directory: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl LockedOverlay {
    fn validate(&self) -> Result<(), OverlayError> {
        if !valid_segment(&self.plugin_id) || !valid_segment(&self.revision) {
            return Err(LayerError::Invalid("invalid plugin ID or revision".into()));
        }
        validate_lock(&self.packages, &self.python_version)?;
        let mut seen = std::collections::HashSet::new();
        for action in &self.actions {
            if !valid_segment(&action.id)
                || action.kind != "create_empty_marker"
                || action.relative_path.is_empty()
                || action.relative_path.contains(['\\', ':'])
                || !action.relative_path.split('/').all(|segment| {
                    valid_segment(segment)
                        || segment.starts_with('.')
                            && segment.len() > 1
                            && valid_segment(&segment[1..])
                })
                || !seen.insert(action.id.clone())
            {
                return Err(LayerError::Invalid(
                    "unsafe or duplicate installer action".into(),
                ));
            }
        }

        for package in &self.packages {
            let name = package
                .name
                .to_ascii_lowercase()
                .replace(['-', '_', '.'], "-");
            if [
                "torch",
                "torchaudio",
                "torchvision",
                "onnxruntime",
                "onnxruntime-gpu",
                "onnxruntime-directml",
            ]
            .contains(&name.as_str())
            {
                return Err(LayerError::Invalid(
                    "backend packages belong to a separate layer".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Read the pinned overlay receipt without installing or rewriting the layer.
pub fn load_installed_overlay(
    overlays_directory: &std::path::Path,
    plugin_id: &str,
    revision: &str,
) -> Result<(LockedOverlay, PathBuf), OverlayError> {
    if !overlays_directory.is_absolute() || !valid_segment(plugin_id) || !valid_segment(revision) {
        return Err(LayerError::Invalid(
            "invalid installed overlay location".into(),
        ));
    }
    let path = overlays_directory.join(plugin_id).join(revision);
    let marker = path.join(".neuromita-overlay.json");
    if path.is_symlink() || marker.is_symlink() {
        return Err(LayerError::Conflict);
    }
    if let Ok(stat) = std::fs::metadata(&marker)
        && (!stat.is_file() || stat.len() > 131_072)
    {
        return Err(LayerError::Conflict);
    }
    let bytes = std::fs::read(&marker).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LayerError::NotInstalled(path.display().to_string())
        } else {
            LayerError::Io(error)
        }
    })?;
    let locked: LockedOverlay = serde_json::from_slice(&bytes).map_err(|_| LayerError::Conflict)?;
    locked.validate()?;
    if locked.plugin_id != plugin_id || locked.revision != revision {
        return Err(LayerError::Conflict);
    }
    Ok((locked, path))
}

impl OverlayInstaller {
    fn layer(&self) -> LayerInstaller<'_> {
        LayerInstaller {
            uv_executable: &self.uv_executable,
            python_executable: &self.python_executable,
            cache_directory: &self.cache_directory,
            timeout: self.timeout,
        }
    }

    pub fn installed_path(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        plan.validate()?;
        if !self.overlays_directory.is_absolute() {
            return Err(LayerError::Invalid("overlay root must be absolute".into()));
        }
        Ok(self
            .overlays_directory
            .join(&plan.plugin_id)
            .join(&plan.revision))
    }

    pub async fn installed(&self, plan: &LockedOverlay) -> Result<bool, OverlayError> {
        self.layer()
            .installed(
                &self.installed_path(plan)?,
                ".neuromita-overlay.json",
                plan,
                &plan.packages,
                &plan.actions,
            )
            .await
    }

    pub async fn install(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        self.install_with_approval(plan, &std::collections::HashSet::new())
            .await
    }

    pub async fn install_with_approval(
        &self,
        plan: &LockedOverlay,
        approved: &std::collections::HashSet<String>,
    ) -> Result<PathBuf, OverlayError> {
        let expected: std::collections::HashSet<String> = plan
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect();
        if !expected.is_subset(approved) || !approved.is_subset(&expected) {
            return Err(LayerError::Invalid(
                "installer actions require explicit, exact approval".into(),
            ));
        }
        self.layer()
            .publish(
                &self.installed_path(plan)?,
                ".neuromita-overlay.json",
                plan,
                &plan.packages,
                &plan.actions,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn plan() -> LockedOverlay {
        LockedOverlay {
            plugin_id: "mock_tts".into(),
            revision: "rev1".into(),
            python_version: "3.13.5".into(),
            packages: vec![],
            actions: vec![],
        }
    }

    #[tokio::test]
    async fn installs_empty_lock_and_rejects_mutated_revision() {
        let root = std::env::temp_dir().join(format!("nm-overlay-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        let python = root.join("python");
        tokio::fs::write(&uv, "fake").await.unwrap();
        tokio::fs::write(&python, "fake").await.unwrap();
        let installer = OverlayInstaller {
            uv_executable: uv,
            python_executable: python,
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let spec = plan();
        let first = installer.install(&spec).await.unwrap();
        assert!(installer.installed(&spec).await.unwrap());
        assert_eq!(installer.install(&spec).await.unwrap(), first);
        let mut changed = spec;
        changed.python_version = "3.13.6".into();
        assert!(matches!(
            installer.installed(&changed).await,
            Err(LayerError::Conflict)
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicitly_approved_install_action_runs_only_in_staging_and_is_verified() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("nm-overlay-action-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(
            &uv,
            r#"#!/bin/sh
[ "$1" = pip ] || exit 2
target=''
prev=''
for arg do
    if [ "$prev" = --target ]; then target="$arg"; fi
    prev="$arg"
done
mkdir -p "$target/fish_speech_lib" "$target/fish_speech_lib-1.0.dist-info"
printf 'Name: fish-speech-lib\nVersion: 1.0\n' > "$target/fish_speech_lib-1.0.dist-info/METADATA"
"#,
        )
        .await
        .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let python = root.join("python");
        tokio::fs::write(&python, "fake").await.unwrap();
        let installer = OverlayInstaller {
            uv_executable: uv,
            python_executable: python,
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let spec = LockedOverlay {
            plugin_id: "fish_speech".into(),
            revision: "rev1".into(),
            python_version: "3.12.10".into(),
            packages: vec![LockedPackage {
                name: "fish-speech-lib".into(),
                version: "1.0".into(),
            }],
            actions: vec![InstallerAction {
                id: "fish_project_root".into(),
                kind: "create_empty_marker".into(),
                relative_path: "fish_speech_lib/.project-root".into(),
            }],
        };
        assert!(installer.install(&spec).await.is_err());
        let approval = std::collections::HashSet::from(["fish_project_root".into()]);
        let installed = installer
            .install_with_approval(&spec, &approval)
            .await
            .unwrap();
        assert!(installed.join("fish_speech_lib/.project-root").is_file());
        assert!(installer.installed(&spec).await.unwrap());
        assert_eq!(
            installer
                .install_with_approval(&spec, &approval)
                .await
                .unwrap(),
            installed
        );
        tokio::fs::remove_file(installed.join("fish_speech_lib/.project-root"))
            .await
            .unwrap();
        assert!(matches!(
            installer.installed(&spec).await,
            Err(LayerError::Corrupted(_))
        ));
        assert!(
            installer
                .install_with_approval(&spec, &approval)
                .await
                .is_err()
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn rejects_unsafe_id_and_backend_package_in_overlay() {
        let mut spec = plan();
        spec.plugin_id = "../escapes".into();
        assert!(spec.validate().is_err());
        spec.plugin_id = "mock_tts".into();
        spec.packages.push(LockedPackage {
            name: "torch".into(),
            version: "2.8.0".into(),
        });
        assert!(spec.validate().is_err());
    }
}
