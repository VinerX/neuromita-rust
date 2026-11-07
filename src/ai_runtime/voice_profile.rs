//! Persistent, validated voice profiles independent of model weights and plugin code.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use super::layer::valid_segment;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VoiceProfile {
    pub id: String,
    pub plugin_id: String,
    pub model_revision: String,
    #[serde(default)]
    pub settings: Map<String, Value>,
    #[serde(default)]
    pub reference_audio: Option<VoiceReference>,
    #[serde(default)]
    pub reference_text: Option<VoiceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VoiceReference {
    pub relative_path: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct VoiceProfileStore {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedVoiceProfile {
    pub plugin_id: String,
    pub model_revision: String,
    pub settings: Map<String, Value>,
    pub reference_audio: Option<PathBuf>,
    pub reference_text: Option<String>,
}

#[derive(Debug, Error)]
pub enum VoiceProfileError {
    #[error("invalid voice profile: {0}")]
    Invalid(String),
    #[error("voice profile reference checksum mismatch: {0}")]
    Checksum(String),
    #[error("voice profile filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.bytes().any(|b| b.is_ascii_control())
        })
}

async fn reject_symlinks(path: &Path) -> Result<(), VoiceProfileError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(VoiceProfileError::Invalid(
                    "symlinks are forbidden in profile paths".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
    }
    Ok(())
}

async fn verified_file(path: &Path, hash: &str, limit: u64) -> Result<(), VoiceProfileError> {
    reject_symlinks(path).await?;
    let meta = tokio::fs::metadata(path).await?;
    if !meta.is_file() || meta.len() > limit {
        return Err(VoiceProfileError::Invalid(
            "invalid or oversized voice reference".into(),
        ));
    }
    let mut file = tokio::fs::File::open(path).await?;
    let mut digest = Context::new(&SHA256);
    let mut buf = [0u8; 65536];
    loop {
        let size = file.read(&mut buf).await?;
        if size == 0 {
            break;
        }
        digest.update(&buf[..size]);
    }
    let result: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !result.eq_ignore_ascii_case(hash) {
        return Err(VoiceProfileError::Checksum(path.display().to_string()));
    }
    Ok(())
}

impl VoiceProfileStore {
    pub async fn load(&self, id: &str) -> Result<ResolvedVoiceProfile, VoiceProfileError> {
        if !self.root.is_absolute() || !valid_segment(id) {
            return Err(VoiceProfileError::Invalid(
                "absolute root and valid profile ID required".into(),
            ));
        }
        let directory = self.root.join(id);
        let manifest = directory.join("voice.json");
        reject_symlinks(&manifest).await?;
        if tokio::fs::metadata(&manifest).await?.len() > 256 * 1024 {
            return Err(VoiceProfileError::Invalid(
                "voice profile exceeds size limit".into(),
            ));
        }
        let profile: VoiceProfile = serde_json::from_slice(&tokio::fs::read(&manifest).await?)
            .map_err(|error| VoiceProfileError::Invalid(error.to_string()))?;
        if profile.id != id
            || !valid_segment(&profile.plugin_id)
            || !valid_segment(&profile.model_revision)
        {
            return Err(VoiceProfileError::Invalid(
                "voice identity or model revision mismatch".into(),
            ));
        }
        if profile.settings.len() > 128
            || profile
                .settings
                .keys()
                .any(|key| key.len() > 128 || key.is_empty())
        {
            return Err(VoiceProfileError::Invalid("invalid settings object".into()));
        }
        let mut seen = HashSet::new();
        for reference in [&profile.reference_audio, &profile.reference_text]
            .into_iter()
            .flatten()
        {
            if !valid_path(&reference.relative_path)
                || reference.sha256.len() != 64
                || !reference.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || !seen.insert(reference.relative_path.clone())
            {
                return Err(VoiceProfileError::Invalid(
                    "unsafe or duplicated voice asset".into(),
                ));
            }
            let file = directory.join(&reference.relative_path);
            let limit = if profile.reference_text.as_ref() == Some(reference) {
                128 * 1024
            } else {
                50 * 1024 * 1024
            };
            verified_file(&file, &reference.sha256, limit).await?;
        }
        let reference_audio = profile
            .reference_audio
            .as_ref()
            .map(|reference| directory.join(&reference.relative_path));
        let reference_text = if let Some(reference) = &profile.reference_text {
            let text = tokio::fs::read_to_string(directory.join(&reference.relative_path)).await?;
            if text.trim().is_empty() {
                return Err(VoiceProfileError::Invalid(
                    "empty reference transcript".into(),
                ));
            }
            Some(text)
        } else {
            None
        };
        Ok(ResolvedVoiceProfile {
            plugin_id: profile.plugin_id,
            model_revision: profile.model_revision,
            settings: profile.settings,
            reference_audio,
            reference_text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn reads_verified_reference_files_without_untrusted_paths() {
        let root = std::env::temp_dir().join(format!("nm-voice-profile-{}", Uuid::new_v4()));
        let directory = root.join("voices/test");
        tokio::fs::create_dir_all(directory.join("references"))
            .await
            .unwrap();
        tokio::fs::write(directory.join("references/prompt.txt"), b"hello")
            .await
            .unwrap();
        let sha = ring::digest::digest(&SHA256, b"hello");
        let hex: String = sha.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let profile = VoiceProfile {
            id: "test".into(),
            plugin_id: "f5_tts_ru".into(),
            model_revision: "rev1".into(),
            settings: Map::from_iter([("speed".into(), Value::from(1.2))]),
            reference_audio: None,
            reference_text: Some(VoiceReference {
                relative_path: "references/prompt.txt".into(),
                sha256: hex.clone(),
            }),
        };
        tokio::fs::write(
            directory.join("voice.json"),
            serde_json::to_vec(&profile).unwrap(),
        )
        .await
        .unwrap();
        let store = VoiceProfileStore {
            root: root.join("voices"),
        };
        let result = store.load("test").await.unwrap();
        assert_eq!(result.reference_text.as_deref(), Some("hello"));
        tokio::fs::write(directory.join("references/prompt.txt"), b"tampered")
            .await
            .unwrap();
        assert!(matches!(
            store.load("test").await,
            Err(VoiceProfileError::Checksum(_))
        ));
        assert!(store.load("../test").await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
