//! Lossless, provider-independent beat timelines and a bounded derived cache.
//!
//! This module does not infer beat timing or start an AI worker. Producers own
//! analysis; callers must derive the source hash from the *same immutable input*
//! they pass to the selected analyzer.
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const CACHE_FORMAT_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BEATS: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeatBackend {
    Auto,
    BeatThis,
    Librosa,
    Dsp,
}

/// Requested analysis identity, rather than the backend that eventually won
/// fallback. Changing a model, decoder or analysis settings must change its
/// corresponding revision field; confidence filtering happens *after* lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeatCacheKey {
    pub source_sha256: String,
    pub backend: BeatBackend,
    pub backend_revision: String,
    pub decoder_revision: String,
    pub analysis_settings_revision: String,
}

impl BeatCacheKey {
    pub fn for_audio_bytes(
        audio: &[u8],
        backend: BeatBackend,
        backend_revision: impl Into<String>,
        decoder_revision: impl Into<String>,
        analysis_settings_revision: impl Into<String>,
    ) -> Self {
        Self {
            source_sha256: sha256_hex(audio),
            backend,
            backend_revision: backend_revision.into(),
            decoder_revision: decoder_revision.into(),
            analysis_settings_revision: analysis_settings_revision.into(),
        }
    }

    pub fn validate(&self) -> Result<(), BeatCacheError> {
        if self.source_sha256.len() != 64
            || !self.source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !valid_revision(&self.backend_revision)
            || !valid_revision(&self.decoder_revision)
            || !valid_revision(&self.analysis_settings_revision)
        {
            return Err(BeatCacheError::InvalidKey);
        }
        Ok(())
    }
}

fn valid_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn sha256_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeatPoint {
    pub time_seconds: f64,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeatTimeline {
    pub beats: Vec<BeatPoint>,
    pub duration_seconds: f64,
    pub sample_rate_hz: u32,
    /// Actual winning method, never inferred from installed/selected backend.
    pub method: String,
    pub bpm: Option<f64>,
}

impl BeatTimeline {
    pub fn validate(&self) -> Result<(), BeatCacheError> {
        if !self.duration_seconds.is_finite()
            || self.duration_seconds < 0.0
            || self.sample_rate_hz == 0
            || self.method.is_empty()
            || self.method.len() > 128
            || self.method.chars().any(char::is_control)
            || self.beats.len() > MAX_BEATS
            || self.bpm.is_some_and(|bpm| !bpm.is_finite() || bpm <= 0.0)
        {
            return Err(BeatCacheError::InvalidTimeline);
        }
        let mut previous = None;
        for beat in &self.beats {
            if !beat.time_seconds.is_finite()
                || beat.time_seconds < 0.0
                || beat.time_seconds > self.duration_seconds
                || !beat.confidence.is_finite()
                || !(0.0..=1.0).contains(&beat.confidence)
                || previous.is_some_and(|before| beat.time_seconds <= before)
            {
                return Err(BeatCacheError::InvalidTimeline);
            }
            previous = Some(beat.time_seconds);
        }
        Ok(())
    }

    /// Filter raw cached confidences on the requested turn without polluting
    /// the cache identity. Do not compare confidence across different methods.
    pub fn with_min_confidence(&self, threshold: f32) -> Result<Self, BeatCacheError> {
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(BeatCacheError::InvalidThreshold);
        }
        self.validate()?;
        let mut selected = self.clone();
        selected.beats.retain(|beat| beat.confidence >= threshold);
        Ok(selected)
    }
}

#[derive(Debug, Error)]
pub enum BeatCacheError {
    #[error("beat cache identity is invalid")]
    InvalidKey,
    #[error("beat timeline contains invalid or unbounded data")]
    InvalidTimeline,
    #[error("confidence threshold must be finite and between zero and one")]
    InvalidThreshold,
    #[error("beat cache I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("serialized beat cache entry exceeds the limit")]
    TooLarge,
}

#[derive(Debug, PartialEq)]
pub enum CacheLookup {
    Hit(BeatTimeline),
    Miss,
    /// Existing cache entry is malformed, oversized, or has mismatched identity.
    /// Analysis may proceed as a miss; the original entry is not trusted.
    Corrupt,
}

#[derive(Debug, Clone)]
pub struct BeatCache {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEnvelope {
    version: u32,
    key: BeatCacheKey,
    timeline: BeatTimeline,
}

impl BeatCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn cache_path(&self, key: &BeatCacheKey) -> Result<PathBuf, BeatCacheError> {
        key.validate()?;
        let identity = serde_json::to_vec(key).map_err(|_| BeatCacheError::InvalidKey)?;
        Ok(self.root.join(format!("beat-v{CACHE_FORMAT_VERSION}-{}.json", sha256_hex(&identity))))
    }

    pub fn lookup(&self, key: &BeatCacheKey) -> Result<CacheLookup, BeatCacheError> {
        let path = self.cache_path(key)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(CacheLookup::Miss),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_CACHE_BYTES {
            return Ok(CacheLookup::Corrupt);
        }
        let mut content = Vec::new();
        fs::File::open(&path)?.take(MAX_CACHE_BYTES + 1).read_to_end(&mut content)?;
        if content.len() as u64 > MAX_CACHE_BYTES {
            return Ok(CacheLookup::Corrupt);
        }
        let Ok(envelope) = serde_json::from_slice::<CacheEnvelope>(&content) else {
            return Ok(CacheLookup::Corrupt);
        };
        if envelope.version != CACHE_FORMAT_VERSION
            || envelope.key != *key
            || envelope.timeline.validate().is_err()
        {
            return Ok(CacheLookup::Corrupt);
        }
        Ok(CacheLookup::Hit(envelope.timeline))
    }

    /// Atomic write using a unique staging file in the same directory. A
    /// failed cache write must be handled separately from successful analysis.
    pub fn store(&self, key: &BeatCacheKey, timeline: &BeatTimeline) -> Result<(), BeatCacheError> {
        let target = self.cache_path(key)?;
        timeline.validate()?;
        let payload = serde_json::to_vec(&CacheEnvelope {
            version: CACHE_FORMAT_VERSION,
            key: key.clone(),
            timeline: timeline.clone(),
        })
        .map_err(|_| BeatCacheError::InvalidTimeline)?;
        if payload.len() as u64 > MAX_CACHE_BYTES {
            return Err(BeatCacheError::TooLarge);
        }
        fs::create_dir_all(&self.root)?;
        let staging = target.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&staging)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            fs::rename(&staging, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result.map_err(BeatCacheError::Io)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
