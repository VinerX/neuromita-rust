//! Read-only inventory of packaged Python speech plugins and verified asset revisions.
//! Discovery never imports or executes a plugin, and it never installs anything.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use crate::ai_runtime::{assets::ModelAssetStore, layer::valid_segment};
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/speech-plugin-manifests.rs"));

pub struct RemoteTtsTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub models: &'static [&'static str],
}

// Python parity: src/presets/remote_voice_templates.py.
pub const REMOTE_TTS_TEMPLATES: &[RemoteTtsTemplate] = &[RemoteTtsTemplate {
    id: "fish_audio",
    name: "Fish Audio",
    models: &["s2.1-pro-free", "s1", "s2-pro", "s2.1-pro"],
}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechModelOption {
    pub id: String,
    pub name: String,
    pub package_detected: bool,
    pub verified_assets: bool,
    pub installed_revisions: Vec<String>,
}

impl SpeechModelOption {
    pub fn installation_label(&self) -> &'static str {
        match (self.package_detected, self.verified_assets) {
            (true, true) => "окружение и файлы подтверждены",
            (true, false) => "окружение обнаружено; файлы не подтверждены",
            (false, true) => "файлы подтверждены; окружение отсутствует",
            (false, false) => "не установлено",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpeechInventory {
    pub tts: Vec<SpeechModelOption>,
    pub asr: Vec<SpeechModelOption>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SpeechModelCatalog {
    pub plugin_roots: Vec<PathBuf>,
    pub data_directory: PathBuf,
}

#[derive(Deserialize)]
struct PluginManifest {
    id: String,
    name: String,
    kind: String,
}

impl SpeechModelCatalog {
    pub fn for_desktop(data_directory: PathBuf) -> Self {
        let roots = vec![
            data_directory.join("AIData/Scripts"),
            data_directory.join("AIData/plugins"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python/plugins/builtin"),
        ];
        Self {
            plugin_roots: roots,
            data_directory,
        }
    }

    pub async fn snapshot(&self) -> SpeechInventory {
        let mut inventory = SpeechInventory::default();
        let mut models = BTreeMap::<(String, String), SpeechModelOption>::new();
        for root in &self.plugin_roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten().take(256) {
                let dir = entry.path();
                if !entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                {
                    continue;
                }
                let manifest_path = dir.join("plugin.json");
                let Ok(stat) = std::fs::symlink_metadata(&manifest_path) else {
                    continue;
                };
                if !stat.is_file() || stat.is_symlink() || stat.len() > 8192 {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&manifest_path) else {
                    continue;
                };
                let Ok(manifest) = serde_json::from_slice::<PluginManifest>(&bytes) else {
                    continue;
                };
                if !valid_segment(&manifest.id)
                    || entry.file_name().to_string_lossy() != manifest.id
                    || !matches!(manifest.kind.as_str(), "tts" | "asr")
                    || manifest.name.is_empty()
                    || manifest.name.len() > 128
                {
                    continue;
                }
                let key = (manifest.kind, manifest.id.clone());
                models.entry(key).or_insert(SpeechModelOption {
                    id: manifest.id,
                    name: manifest.name,
                    package_detected: false,
                    verified_assets: false,
                    installed_revisions: Vec::new(),
                });
            }
        }
        for text in BUNDLED_SPEECH_MANIFESTS {
            let Ok(manifest) = serde_json::from_str::<PluginManifest>(text) else {
                continue;
            };
            if !valid_segment(&manifest.id)
                || !matches!(manifest.kind.as_str(), "tts" | "asr")
                || manifest.name.is_empty()
            {
                continue;
            }
            let key = (manifest.kind, manifest.id.clone());
            models.entry(key).or_insert(SpeechModelOption {
                id: manifest.id,
                name: manifest.name,
                package_detected: false,
                verified_assets: false,
                installed_revisions: Vec::new(),
            });
        }
        for ((kind, id), option) in &mut models {
            let overlays = self.data_directory.join("AIData/overlays").join(id);
            if let Ok(entries) = std::fs::read_dir(&overlays) {
                for entry in entries.flatten().take(128) {
                    if !entry
                        .file_type()
                        .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                    {
                        continue;
                    }
                    let path = entry.path().join(".neuromita-overlay.json");
                    if let Ok(meta) = std::fs::symlink_metadata(&path)
                        && meta.is_file()
                        && !meta.is_symlink()
                        && meta.len() < 262_144
                        && let Ok(payload) = std::fs::read(&path)
                        && let Ok(locked) = serde_json::from_slice::<serde_json::Value>(&payload)
                        && locked.get("plugin_id").and_then(|v| v.as_str()) == Some(id)
                        && locked.get("revision").and_then(|v| v.as_str())
                            == entry.file_name().to_str()
                    {
                        option.package_detected = true;
                    }
                }
            }
            let store = ModelAssetStore {
                root: self.data_directory.join("AIData/models"),
                max_asset_bytes: 4 * 1024 * 1024 * 1024,
                timeout: Duration::from_secs(30),
            };
            let versions = store.root.join(id);
            if let Ok(entries) = std::fs::read_dir(&versions) {
                for entry in entries.flatten().take(128) {
                    if !entry
                        .file_type()
                        .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                    {
                        continue;
                    }
                    let revision = entry.file_name().to_string_lossy().to_string();
                    if valid_segment(&revision)
                        && store.verified_revision(id, &revision).await.is_ok()
                    {
                        option.verified_assets = true;
                        option.installed_revisions.push(revision);
                    }
                }
            }
            let list = if kind == "tts" {
                &mut inventory.tts
            } else {
                &mut inventory.asr
            };
            list.push(option.clone());
        }
        inventory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_manifests_without_treating_bare_directories_as_installs() {
        let root = std::env::temp_dir().join(format!("nm-speech-catalog-{}", uuid::Uuid::new_v4()));
        let scripts = root.join("plugins");
        std::fs::create_dir_all(scripts.join("gigaam_onnx")).unwrap();
        std::fs::write(
            scripts.join("gigaam_onnx/plugin.json"),
            r#"{"id":"gigaam_onnx","name":"GigaAM","kind":"asr"}"#,
        )
        .unwrap();
        let snapshot = SpeechModelCatalog {
            plugin_roots: vec![scripts],
            data_directory: root.clone(),
        }
        .snapshot()
        .await;
        let model = snapshot
            .asr
            .iter()
            .find(|item| item.id == "gigaam_onnx")
            .unwrap();
        assert!(!model.package_detected);
        assert!(!model.verified_assets);
        assert!(snapshot.tts.iter().any(|item| item.id == "fish_speech"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
