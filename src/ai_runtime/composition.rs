//! Verified, selected package layers used to construct each Python worker import path.

use super::{
    AiService, ProcessTopology,
    backend::{BackendInstaller, BackendKind, LockedBackend},
    overlay::{LockedOverlay, LockedPackage, OverlayInstaller},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
use thiserror::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceSelection {
    pub service: AiService,
    pub backends: Vec<LockedBackend>,
    pub overlays: Vec<LockedOverlay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeComposition {
    python_version: String,
    paths: Vec<PathBuf>,
    services: Vec<AiService>,
}

#[derive(Debug, Error)]
pub enum CompositionError {
    #[error("invalid runtime selection: {0}")]
    Invalid(String),
    #[error("selected layer has not been installed: {0}")]
    NotInstalled(String),
    #[error("conflicting selected Python distributions: {0}")]
    Conflict(String),
    #[error("package layer inspection failed: {0}")]
    Layer(#[from] super::layer::LayerError),
    #[error("package layer filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

impl RuntimeComposition {
    pub fn python_version(&self) -> &str {
        &self.python_version
    }
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    pub fn services(&self) -> &[AiService] {
        &self.services
    }

    pub async fn resolve(
        topology: ProcessTopology,
        requested_service: AiService,
        selections: &[ServiceSelection],
        backend_installer: &BackendInstaller,
        overlay_installer: &OverlayInstaller,
    ) -> Result<Self, CompositionError> {
        let mut unique = HashSet::new();
        for selection in selections {
            if !unique.insert(selection.service) {
                return Err(CompositionError::Invalid(
                    "duplicate service selection".into(),
                ));
            }
        }
        if !unique.contains(&requested_service) {
            return Err(CompositionError::Invalid(
                "requested service not selected".into(),
            ));
        }
        let mut active: Vec<_> = selections
            .iter()
            .filter(|selected| {
                topology == ProcessTopology::Shared || selected.service == requested_service
            })
            .collect();
        active.sort_by_key(|selection| order(selection.service));
        let kinds: HashSet<_> = active
            .iter()
            .flat_map(|selection| selection.backends.iter().map(|backend| backend.kind))
            .collect();
        if kinds.contains(&BackendKind::TorchCpu) && kinds.contains(&BackendKind::TorchCuda) {
            return Err(CompositionError::Conflict(
                "Torch CPU and CUDA backend layers cannot coexist in one Python worker".into(),
            ));
        }
        if [
            BackendKind::OnnxCpu,
            BackendKind::OnnxCuda,
            BackendKind::OnnxDirectml,
        ]
        .iter()
        .filter(|kind| kinds.contains(kind))
        .count()
            > 1
        {
            return Err(CompositionError::Conflict(
                "multiple ONNXRuntime variants cannot coexist in one Python worker".into(),
            ));
        }
        let mut versions = HashMap::<String, String>::new();
        let mut python_version: Option<String> = None;
        let mut seen_paths = HashSet::new();
        let mut backend_paths = Vec::new();
        let mut overlay_paths = Vec::new();
        for selection in &active {
            for backend in &selection.backends {
                check_python(&mut python_version, &backend.python_version)?;
                check_pins(&mut versions, &backend.packages)?;
                let path = backend_installer.installed_path(backend)?;
                if !backend_installer.installed(backend).await? {
                    return Err(CompositionError::NotInstalled(path.display().to_string()));
                }
                add_path(&mut backend_paths, &mut seen_paths, path)?;
            }
            for overlay in &selection.overlays {
                check_python(&mut python_version, &overlay.python_version)?;
                check_pins(&mut versions, &overlay.packages)?;
                let path = overlay_installer.installed_path(overlay)?;
                if !overlay_installer.installed(overlay).await? {
                    return Err(CompositionError::NotInstalled(path.display().to_string()));
                }
                add_path(&mut overlay_paths, &mut seen_paths, path)?;
            }
        }
        let python_version = python_version
            .ok_or_else(|| CompositionError::Invalid("no installed layers selected".into()))?;
        backend_paths.extend(overlay_paths);
        Ok(Self {
            python_version,
            paths: backend_paths,
            services: active.iter().map(|item| item.service).collect(),
        })
    }
}

fn order(service: AiService) -> u8 {
    match service {
        AiService::Tts => 0,
        AiService::Asr => 1,
        AiService::Embedding => 2,
        AiService::Reranker => 3,
        AiService::Beats => 4,
    }
}

fn check_python(current: &mut Option<String>, required: &str) -> Result<(), CompositionError> {
    match current {
        Some(existing) if existing != required => Err(CompositionError::Conflict(format!(
            "Python {existing} vs {required}"
        ))),
        Some(_) => Ok(()),
        None => {
            *current = Some(required.into());
            Ok(())
        }
    }
}

fn check_pins(
    known: &mut HashMap<String, String>,
    packages: &[LockedPackage],
) -> Result<(), CompositionError> {
    for package in packages {
        let name = package
            .name
            .to_ascii_lowercase()
            .replace(['-', '_', '.'], "-");
        match known.get(&name) {
            Some(version) if version != &package.version => {
                return Err(CompositionError::Conflict(format!(
                    "{name}: {version} vs {}",
                    package.version
                )));
            }
            Some(_) => (),
            None => {
                known.insert(name, package.version.clone());
            }
        }
    }
    Ok(())
}

fn add_path(
    target: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    path: PathBuf,
) -> Result<(), CompositionError> {
    if !path.is_absolute() || !path.is_dir() || path.is_symlink() {
        return Err(CompositionError::Invalid(
            "layer path must be a real absolute directory".into(),
        ));
    }
    let canonical = std::fs::canonicalize(&path)?;
    if seen.insert(canonical.clone()) {
        target.push(canonical);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{backend::BackendKind, overlay::LockedPackage};
    use super::*;
    use std::time::Duration;
    use uuid::Uuid;

    #[tokio::test]
    async fn shared_worker_rejects_conflicting_compute_backend_variants() {
        let root = std::env::temp_dir().join(format!("nm-composition-{}", Uuid::new_v4()));
        let b = BackendInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let o = OverlayInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let layer = |kind| LockedBackend {
            kind,
            revision: "v1".into(),
            python_version: "3.12.10".into(),
            packages: vec![],
        };
        for (first, second) in [
            (BackendKind::TorchCpu, BackendKind::TorchCuda),
            (BackendKind::OnnxCpu, BackendKind::OnnxDirectml),
            (BackendKind::OnnxCuda, BackendKind::OnnxDirectml),
        ] {
            let selections = [
                ServiceSelection {
                    service: AiService::Tts,
                    backends: vec![layer(first)],
                    overlays: vec![],
                },
                ServiceSelection {
                    service: AiService::Asr,
                    backends: vec![layer(second)],
                    overlays: vec![],
                },
            ];
            assert!(matches!(
                RuntimeComposition::resolve(
                    ProcessTopology::Shared,
                    AiService::Tts,
                    &selections,
                    &b,
                    &o
                )
                .await,
                Err(CompositionError::Conflict(_))
            ));
        }
    }

    #[tokio::test]
    async fn split_excludes_other_services_while_shared_rejects_collisions() {
        let root = std::env::temp_dir().join(format!("nm-composition-{}", Uuid::new_v4()));
        let b = BackendInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let o = OverlayInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let backend = LockedBackend {
            kind: BackendKind::TorchCpu,
            revision: "base1".into(),
            python_version: "3.13.5".into(),
            packages: vec![LockedPackage {
                name: "torch".into(),
                version: "2.8.0".into(),
            }],
        };
        let f5 = LockedOverlay {
            plugin_id: "f5".into(),
            revision: "rev1".into(),
            python_version: "3.13.5".into(),
            packages: vec![LockedPackage {
                name: "transformers".into(),
                version: "4.45.0".into(),
            }],
            actions: vec![],
        };
        let asr = LockedOverlay {
            plugin_id: "gigaam".into(),
            revision: "rev1".into(),
            python_version: "3.13.5".into(),
            packages: vec![LockedPackage {
                name: "transformers".into(),
                version: "4.50.0".into(),
            }],
            actions: vec![],
        };
        for (path, marker, bytes) in [
            (
                b.installed_path(&backend).unwrap(),
                ".neuromita-backend.json",
                serde_json::to_vec(&backend).unwrap(),
            ),
            (
                o.installed_path(&f5).unwrap(),
                ".neuromita-overlay.json",
                serde_json::to_vec(&f5).unwrap(),
            ),
            (
                o.installed_path(&asr).unwrap(),
                ".neuromita-overlay.json",
                serde_json::to_vec(&asr).unwrap(),
            ),
        ] {
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(path.join(marker), bytes).await.unwrap();
            let pins = if marker == ".neuromita-backend.json" {
                vec![("torch", "2.8.0")]
            } else if path.to_string_lossy().contains("gigaam") {
                vec![("transformers", "4.50.0")]
            } else {
                vec![("transformers", "4.45.0")]
            };
            for (name, version) in pins {
                let dist = path.join(format!("{name}-{version}.dist-info"));
                tokio::fs::create_dir_all(&dist).await.unwrap();
                tokio::fs::write(
                    dist.join("METADATA"),
                    format!("Name: {name}\nVersion: {version}\n"),
                )
                .await
                .unwrap();
            }
        }
        let selected = [
            ServiceSelection {
                service: AiService::Tts,
                backends: vec![backend.clone()],
                overlays: vec![f5],
            },
            ServiceSelection {
                service: AiService::Asr,
                backends: vec![backend],
                overlays: vec![asr],
            },
        ];
        let split =
            RuntimeComposition::resolve(ProcessTopology::Split, AiService::Tts, &selected, &b, &o)
                .await
                .unwrap();
        assert_eq!(split.paths.len(), 2);
        assert_eq!(split.services, vec![AiService::Tts]);
        assert!(matches!(
            RuntimeComposition::resolve(ProcessTopology::Shared, AiService::Tts, &selected, &b, &o)
                .await,
            Err(CompositionError::Conflict(_))
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn absent_or_duplicate_selection_fails_closed() {
        let root = std::env::temp_dir().join(format!("nm-select-{}", Uuid::new_v4()));
        let b = BackendInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            backends_directory: root.join("backend"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let o = OverlayInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            overlays_directory: root.join("overlay"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(1),
        };
        let selection = ServiceSelection {
            service: AiService::Tts,
            backends: vec![],
            overlays: vec![],
        };
        assert!(matches!(
            RuntimeComposition::resolve(
                ProcessTopology::Split,
                AiService::Tts,
                std::slice::from_ref(&selection),
                &b,
                &o
            )
            .await,
            Err(CompositionError::Invalid(_))
        ));
        assert!(matches!(
            RuntimeComposition::resolve(
                ProcessTopology::Shared,
                AiService::Tts,
                &[selection.clone(), selection],
                &b,
                &o
            )
            .await,
            Err(CompositionError::Invalid(_))
        ));
    }
}
