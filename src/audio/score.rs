//! Stateful score providers and bounded framing; endpoint policy belongs to the segmenter.

use std::{fs::File, io::Read, path::Path, sync::Arc};

use ring::digest::{SHA256, digest};
use thiserror::Error;

use super::vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ};

pub const SILERO_MODEL_BYTES: usize = 2_327_524;
pub const SILERO_MODEL_SHA256: &str =
    "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3";

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("invalid VAD framing: {0}")]
    Framing(&'static str),
    #[error("VAD provider failed: {0}")]
    Provider(String),
    #[error("VAD provider returned an invalid probability")]
    Probability,
    #[error("VAD score stream failed; reset is required before continuing")]
    ResetRequired,
    #[error("local Silero asset must be an absolute path")]
    AssetPath,
    #[error("local Silero asset does not match the pinned size/SHA-256")]
    AssetMismatch,
    #[error("cannot read local VAD asset: {0}")]
    AssetIo(#[from] std::io::Error),
}

pub trait VadScoreProvider: Send {
    fn frame_samples(&self) -> usize;
    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError>;
    fn reset(&mut self);
}

#[derive(Default)]
pub struct DirectEarshotScores(earshot::Detector);

impl VadScoreProvider for DirectEarshotScores {
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        let frame: &[i16; FRAME_SAMPLES] = frame
            .try_into()
            .map_err(|_| ScoreError::Framing("Earshot requires 256 samples"))?;
        Ok(self.0.predict_i16(frame))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ScoredFrame {
    pub start_sample: u64,
    pub end_sample: u64,
    pub probability: f32,
}

pub struct ScoreFrames<P> {
    provider: P,
    pending: Vec<i16>,
    frame_samples: usize,
    scored_samples: u64,
    poisoned: bool,
}

impl<P: VadScoreProvider> ScoreFrames<P> {
    pub fn new(provider: P) -> Result<Self, ScoreError> {
        let frame_samples = provider.frame_samples();
        if ![256, 512].contains(&frame_samples) {
            return Err(ScoreError::Framing("unsupported provider frame size"));
        }
        Ok(Self {
            provider,
            pending: Vec::with_capacity(frame_samples),
            frame_samples,
            scored_samples: 0,
            poisoned: false,
        })
    }

    /// Accept at most one second of mono PCM16 at 16 kHz; never pad a partial frame.
    pub fn push(&mut self, samples: &[i16]) -> Result<Vec<ScoredFrame>, ScoreError> {
        if self.poisoned {
            return Err(ScoreError::ResetRequired);
        }
        if samples.len() > SAMPLE_RATE_HZ as usize {
            return Err(ScoreError::Framing("input chunk exceeds one second"));
        }
        let mut scores =
            Vec::with_capacity((self.pending.len() + samples.len()) / self.frame_samples);
        let mut remaining = samples;
        while !remaining.is_empty() {
            let take = (self.frame_samples - self.pending.len()).min(remaining.len());
            self.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending.len() == self.frame_samples {
                self.poisoned = true;
                let probability = self.provider.score(&self.pending)?;
                if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
                    return Err(ScoreError::Probability);
                }
                let end_sample = self
                    .scored_samples
                    .checked_add(self.frame_samples as u64)
                    .ok_or(ScoreError::Framing("sample offset overflow"))?;
                scores.push(ScoredFrame {
                    start_sample: self.scored_samples,
                    end_sample,
                    probability,
                });
                self.scored_samples = end_sample;
                self.pending.clear();
                self.poisoned = false;
            }
        }
        Ok(scores)
    }

    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }

    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.scored_samples = 0;
        self.provider.reset();
        self.poisoned = false;
    }
}

pub struct VerifiedSileroAsset(Arc<[u8]>);

impl VerifiedSileroAsset {
    pub fn open(path: &Path) -> Result<Self, ScoreError> {
        if !path.is_absolute() {
            return Err(ScoreError::AssetPath);
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(SILERO_MODEL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != SILERO_MODEL_BYTES || sha256(&bytes) != SILERO_MODEL_SHA256 {
            return Err(ScoreError::AssetMismatch);
        }
        Ok(Self(Arc::from(bytes)))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Frames {
        count: usize,
        probability: f32,
    }

    impl VadScoreProvider for Frames {
        fn frame_samples(&self) -> usize {
            512
        }
        fn score(&mut self, samples: &[i16]) -> Result<f32, ScoreError> {
            assert_eq!(samples.len(), 512);
            assert!(samples.iter().all(|value| *value == self.count as i16));
            self.count += 1;
            Ok(self.probability)
        }
        fn reset(&mut self) {
            self.count = 0;
        }
    }

    #[test]
    fn partial_frames_wait_and_reset_never_splices_streams() {
        let mut frames = ScoreFrames::new(Frames {
            count: 0,
            probability: 0.5,
        })
        .unwrap();
        assert!(frames.push(&[0; 256]).unwrap().is_empty());
        let first = frames.push(&[0; 256]).unwrap();
        assert_eq!((first[0].start_sample, first[0].end_sample), (0, 512));
        assert!(frames.push(&[1; 7]).unwrap().is_empty());
        frames.reset();
        let next = frames.push(&[0; 512]).unwrap();
        assert_eq!(first, next);
        assert_eq!(frames.pending_samples(), 0);
    }

    #[test]
    fn bad_scores_and_unbounded_chunks_are_errors() {
        let mut frames = ScoreFrames::new(Frames {
            count: 0,
            probability: f32::NAN,
        })
        .unwrap();
        assert!(matches!(
            frames.push(&[0; 512]),
            Err(ScoreError::Probability)
        ));
        assert!(matches!(
            frames.push(&vec![0; 16_001]),
            Err(ScoreError::ResetRequired)
        ));
    }

    #[test]
    fn actual_direct_earshot_is_chunk_partition_invariant_and_resets() {
        let samples: Vec<_> = (0..2048)
            .map(|index| ((index * 97) % 16000) as i16)
            .collect();
        let mut whole = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        let expected = whole.push(&samples).unwrap();
        let mut split = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        let mut actual = Vec::new();
        for chunk in samples.chunks(77) {
            actual.extend(split.push(chunk).unwrap());
        }
        assert_eq!(actual, expected);
        split.reset();
        assert_eq!(split.push(&samples).unwrap(), expected);
    }

    struct FailsSecond {
        calls: usize,
        provider_error: bool,
    }

    impl VadScoreProvider for FailsSecond {
        fn frame_samples(&self) -> usize {
            256
        }
        fn score(&mut self, _: &[i16]) -> Result<f32, ScoreError> {
            self.calls += 1;
            if self.calls != 2 {
                return Ok(0.25);
            }
            if self.provider_error {
                Err(ScoreError::Provider("controlled failure".into()))
            } else {
                Ok(f32::NAN)
            }
        }
        fn reset(&mut self) {
            self.calls = 0;
        }
    }

    #[test]
    fn partial_chunk_failure_requires_reset_and_never_returns_partial_metrics() {
        for provider_error in [false, true] {
            let mut frames = ScoreFrames::new(FailsSecond {
                calls: 0,
                provider_error,
            })
            .unwrap();
            assert!(frames.push(&[0; 512]).is_err());
            assert_eq!(frames.provider.calls, 2);
            assert!(matches!(
                frames.push(&[0; 256]),
                Err(ScoreError::ResetRequired)
            ));
            assert!(matches!(frames.push(&[]), Err(ScoreError::ResetRequired)));
            assert_eq!(frames.provider.calls, 2);
            frames.reset();
            let recovered = frames.push(&[0; 256]).unwrap();
            assert_eq!(
                recovered,
                vec![ScoredFrame {
                    start_sample: 0,
                    end_sample: 256,
                    probability: 0.25
                }]
            );
        }
    }

    #[test]
    fn sample_offset_failure_also_requires_reset() {
        let mut frames = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        frames.scored_samples = u64::MAX;
        assert!(matches!(
            frames.push(&[0; 256]),
            Err(ScoreError::Framing(_))
        ));
        assert!(matches!(
            frames.push(&[0; 256]),
            Err(ScoreError::ResetRequired)
        ));
        frames.reset();
        assert_eq!(frames.push(&[0; 256]).unwrap()[0].start_sample, 0);
    }

    #[test]
    fn oversize_input_rejection_does_not_consume_or_poison() {
        let mut frames = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        assert!(frames.push(&vec![0; 16_001]).is_err());
        assert_eq!(frames.push(&[0; 256]).unwrap()[0].start_sample, 0);
    }

    #[test]
    fn direct_earshot_rejects_short_and_long_frames() {
        let mut provider = DirectEarshotScores::default();
        assert!(matches!(
            provider.score(&[0; 255]),
            Err(ScoreError::Framing(_))
        ));
        assert!(matches!(
            provider.score(&[0; 257]),
            Err(ScoreError::Framing(_))
        ));
        assert!(provider.score(&[0; 256]).is_ok());
    }
}
