//! Bounded 16-kHz PCM16 segmentation. Device capture is a separate adapter.

use std::collections::VecDeque;

use thiserror::Error;

use super::score::{ScoreError, ScoreFrames, VadScoreProvider};
use super::vad::{
    EarshotVad, FRAME_SAMPLES, MonoPcm16Frame, SAMPLE_RATE_HZ, VoiceDecision, VoiceThreshold,
};

#[derive(Debug, Error)]
pub enum SegmentError {
    #[error("invalid speech segmentation limits")]
    InvalidConfiguration,
    #[error("speech segment exceeds the configured limit")]
    TooLong,
    #[error("voice activity provider failed")]
    Vad(#[from] ScoreError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechMode {
    Automatic,
    PushToTalk,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    pub mode: SpeechMode,
    /// Policy durations are expressed in mono PCM16 samples at 16 kHz so the same
    /// configuration keeps its timing when the score provider uses 256- or
    /// 512-sample frames.
    pub pre_roll_samples: usize,
    pub start_voice_samples: usize,
    pub end_silence_samples: usize,
    pub min_segment_samples: usize,
    pub max_segment_samples: usize,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            mode: SpeechMode::Automatic,
            pre_roll_samples: 12 * FRAME_SAMPLES,
            start_voice_samples: 3 * FRAME_SAMPLES,
            end_silence_samples: 28 * FRAME_SAMPLES,
            min_segment_samples: 10 * FRAME_SAMPLES,
            max_segment_samples: 625 * FRAME_SAMPLES,
        }
    }
}

/// Longest accepted segment: ten seconds at 16 kHz.
pub const MAX_SEGMENT_SAMPLES: usize = 625 * FRAME_SAMPLES;

pub trait VoiceGate: Send {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError>;

    /// Capture frames between a provider window completing and its score becoming
    /// observable. A wide provider defers its first decision by this many frames,
    /// so the pre-roll buffer must keep that many extra frames to preserve onset.
    fn decision_lag_frames(&self) -> usize {
        0
    }

    fn reset(&mut self);
}

impl VoiceGate for EarshotVad {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError> {
        Ok(self.process(frame))
    }

    fn reset(&mut self) {
        EarshotVad::reset(self);
    }
}

/// Adapts a 256- or 512-sample score provider to the 256-sample capture grid.
///
/// A provider score covers its whole sample span, so every capture frame keeps
/// policy durations in samples rather than provider frames. Because a 512-sample
/// provider cannot score a frame before its second half arrives, such a gate
/// reports the most recent completed score; the extra decision delay is at most
/// one capture frame and is visible in endpoint latency measurements.
pub struct ScoreGate<P: VadScoreProvider> {
    frames: ScoreFrames<P>,
    threshold: VoiceThreshold,
    unscored: Vec<i16>,
    latest: VoiceDecision,
}

impl<P: VadScoreProvider> ScoreGate<P> {
    pub fn new(provider: P, threshold: VoiceThreshold) -> Result<Self, ScoreError> {
        Ok(Self {
            frames: ScoreFrames::new(provider)?,
            threshold,
            unscored: Vec::with_capacity(512),
            latest: VoiceDecision {
                probability: 0.0,
                speech: false,
            },
        })
    }
}

impl<P: VadScoreProvider + Send> VoiceGate for ScoreGate<P> {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError> {
        self.unscored.extend_from_slice(frame);
        let pending = std::mem::take(&mut self.unscored);
        let scores = match self.frames.push(&pending) {
            Ok(scores) => scores,
            Err(error) => {
                self.unscored = pending;
                return Err(error);
            }
        };
        if let Some(score) = scores.last() {
            self.latest = VoiceDecision {
                probability: score.probability,
                speech: score.probability > self.threshold.value(),
            };
        }
        Ok(self.latest)
    }

    fn decision_lag_frames(&self) -> usize {
        self.frames
            .frame_samples()
            .div_ceil(FRAME_SAMPLES)
            .saturating_sub(1)
    }

    fn reset(&mut self) {
        self.frames.reset();
        self.unscored.clear();
        self.latest = VoiceDecision {
            probability: 0.0,
            speech: false,
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSegment {
    pub samples: Vec<i16>,
}

impl SpeechSegment {
    pub fn as_wav(&self) -> Result<Vec<u8>, SegmentError> {
        let byte_len = self
            .samples
            .len()
            .checked_mul(2)
            .ok_or(SegmentError::TooLong)?;
        let data_bytes = u32::try_from(byte_len).map_err(|_| SegmentError::TooLong)?;
        let chunk_size = data_bytes.checked_add(36).ok_or(SegmentError::TooLong)?;
        let mut wav = Vec::with_capacity(44 + byte_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&chunk_size.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE_HZ * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in &self.samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(wav)
    }
}

pub struct SpeechSegmenter<G: VoiceGate> {
    gate: G,
    config: SegmentConfig,
    retained_frames: usize,
    pre_roll: VecDeque<MonoPcm16Frame>,
    active: Vec<i16>,
    voice_run: usize,
    silence_run: usize,
    pressed: bool,
}

impl<G: VoiceGate> SpeechSegmenter<G> {
    pub fn new(gate: G, config: SegmentConfig) -> Result<Self, SegmentError> {
        if config.pre_roll_samples > config.max_segment_samples
            || config.start_voice_samples == 0
            || config.start_voice_samples > config.max_segment_samples
            || config.end_silence_samples == 0
            || config.min_segment_samples == 0
            || config.max_segment_samples < config.min_segment_samples
            || config.max_segment_samples > MAX_SEGMENT_SAMPLES
        {
            return Err(SegmentError::InvalidConfiguration);
        }
        let retained_frames = config
            .pre_roll_samples
            .max(config.start_voice_samples)
            .div_ceil(FRAME_SAMPLES)
            .saturating_add(gate.decision_lag_frames());
        Ok(Self {
            gate,
            config,
            retained_frames,
            pre_roll: VecDeque::with_capacity(retained_frames),
            active: Vec::new(),
            voice_run: 0,
            silence_run: 0,
            pressed: false,
        })
    }

    pub fn process(
        &mut self,
        frame: MonoPcm16Frame,
    ) -> Result<Option<SpeechSegment>, SegmentError> {
        match self.config.mode {
            SpeechMode::PushToTalk => {
                if !self.pressed {
                    return Ok(None);
                }
                self.active.extend_from_slice(&frame);
            }
            SpeechMode::Automatic => {
                let voice = self.gate.inspect(&frame)?.speech;
                if self.active.is_empty() {
                    {
                        if self.pre_roll.len() == self.retained_frames {
                            self.pre_roll.pop_front();
                        }
                        self.pre_roll.push_back(frame);
                    }
                    self.voice_run = if voice {
                        self.voice_run + FRAME_SAMPLES
                    } else {
                        0
                    };
                    if self.voice_run < self.config.start_voice_samples {
                        return Ok(None);
                    }
                    for buffered in self.pre_roll.drain(..) {
                        self.active.extend_from_slice(&buffered);
                    }
                    if self.active.is_empty() {
                        self.active.extend_from_slice(&frame);
                    }
                    self.silence_run = 0;
                } else {
                    self.active.extend_from_slice(&frame);
                    self.silence_run = if voice {
                        0
                    } else {
                        self.silence_run + FRAME_SAMPLES
                    };
                    if self.silence_run >= self.config.end_silence_samples {
                        return Ok(self.finish());
                    }
                }
            }
        }
        if self.active.len() >= self.config.max_segment_samples {
            return Ok(self.finish());
        }
        Ok(None)
    }

    pub fn set_pressed(&mut self, pressed: bool) -> Option<SpeechSegment> {
        if self.config.mode != SpeechMode::PushToTalk || pressed == self.pressed {
            return None;
        }
        self.pressed = pressed;
        if !pressed {
            self.finish()
        } else {
            self.reset_audio();
            None
        }
    }

    pub fn flush(&mut self) -> Option<SpeechSegment> {
        self.finish()
    }

    pub fn reset(&mut self) {
        self.reset_audio();
        self.pressed = false;
        self.gate.reset();
    }

    pub fn suppress_playback_echo(&mut self) {
        self.reset_audio();
        self.gate.reset();
    }

    fn finish(&mut self) -> Option<SpeechSegment> {
        let enough = self.active.len() >= self.config.min_segment_samples;
        let samples = std::mem::take(&mut self.active);
        self.pre_roll.clear();
        self.voice_run = 0;
        self.silence_run = 0;
        enough.then_some(SpeechSegment { samples })
    }

    fn reset_audio(&mut self) {
        self.active.clear();
        self.pre_roll.clear();
        self.voice_run = 0;
        self.silence_run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DeterministicGate;
    impl VoiceGate for DeterministicGate {
        fn inspect(&mut self, frame: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError> {
            Ok(VoiceDecision {
                probability: if frame[0] != 0 { 1.0 } else { 0.0 },
                speech: frame[0] != 0,
            })
        }
        fn reset(&mut self) {}
    }

    /// Provider that scores each 512-sample window by whether it holds a marker.
    struct WideProvider {
        window: usize,
        marker: i16,
    }

    impl VadScoreProvider for WideProvider {
        fn frame_samples(&self) -> usize {
            self.window
        }

        fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
            Ok(f32::from(frame.contains(&self.marker)))
        }

        fn reset(&mut self) {}
    }

    #[test]
    fn wide_score_provider_keeps_policy_durations_in_real_time() {
        let threshold = VoiceThreshold::new(0.5).unwrap();
        let gate = ScoreGate::new(
            WideProvider {
                window: 512,
                marker: 7,
            },
            threshold,
        )
        .unwrap();
        let mut segmenter = SpeechSegmenter::new(
            gate,
            SegmentConfig {
                pre_roll_samples: 0,
                start_voice_samples: 3 * FRAME_SAMPLES,
                end_silence_samples: 2 * FRAME_SAMPLES,
                min_segment_samples: FRAME_SAMPLES,
                max_segment_samples: 100 * FRAME_SAMPLES,
                ..config(SpeechMode::Automatic)
            },
        )
        .unwrap();

        let mut published = None;
        let mut voiced_frames = 0;
        for frame in 0..20 {
            let samples = if (3..12).contains(&frame) {
                voiced_frames += 1;
                [7; FRAME_SAMPLES]
            } else {
                [0; FRAME_SAMPLES]
            };
            if let Some(segment) = segmenter.process(samples).unwrap() {
                published = Some(segment);
            }
        }
        let segment = published.expect("wide provider must still segment speech");
        assert_eq!(voiced_frames, 9);
        // A 512-sample provider can only report after its second capture frame,
        // so the trail keeps at most the 32 ms provider window, never 2x policy.
        assert!(segment.samples.len() >= voiced_frames * FRAME_SAMPLES / 2);
        assert!(segment.samples.len() <= (voiced_frames + 4) * FRAME_SAMPLES);
    }

    #[test]
    fn wide_provider_onset_survives_disabled_pre_roll() {
        let threshold = VoiceThreshold::new(0.5).unwrap();
        let gate = ScoreGate::new(
            WideProvider {
                window: 512,
                marker: 5,
            },
            threshold,
        )
        .unwrap();
        assert_eq!(gate.decision_lag_frames(), 1);
        let mut segmenter = SpeechSegmenter::new(
            gate,
            SegmentConfig {
                pre_roll_samples: 0,
                start_voice_samples: 3 * FRAME_SAMPLES,
                end_silence_samples: 2 * FRAME_SAMPLES,
                min_segment_samples: FRAME_SAMPLES,
                max_segment_samples: 100 * FRAME_SAMPLES,
                ..config(SpeechMode::Automatic)
            },
        )
        .unwrap();

        // Speech starts on the first half of a 512-sample window and is only
        // scored once its second half arrives.
        let mut published = None;
        for frame in 0..10 {
            let samples = if frame < 4 {
                [5; FRAME_SAMPLES]
            } else {
                [0; FRAME_SAMPLES]
            };
            if let Some(segment) = segmenter.process(samples).unwrap() {
                published = Some(segment);
            }
        }
        let segment = published.expect("onset must still publish a segment");
        let leading = segment
            .samples
            .iter()
            .take_while(|sample| **sample == 5)
            .count();
        assert_eq!(leading, 4 * FRAME_SAMPLES);
    }

    fn config(mode: SpeechMode) -> SegmentConfig {
        SegmentConfig {
            mode,
            pre_roll_samples: 3 * FRAME_SAMPLES,
            start_voice_samples: 2 * FRAME_SAMPLES,
            end_silence_samples: 3 * FRAME_SAMPLES,
            min_segment_samples: 2 * FRAME_SAMPLES,
            max_segment_samples: 30 * FRAME_SAMPLES,
        }
    }

    #[test]
    fn playback_suppression_preserves_ptt_button_state() {
        let config = SegmentConfig {
            mode: SpeechMode::PushToTalk,
            min_segment_samples: 2 * FRAME_SAMPLES,
            ..SegmentConfig::default()
        };
        let mut segmenter = SpeechSegmenter::new(DeterministicGate, config).unwrap();
        assert!(segmenter.set_pressed(true).is_none());
        segmenter.suppress_playback_echo();
        assert!(segmenter.process([5; FRAME_SAMPLES]).unwrap().is_none());
        assert!(segmenter.process([6; FRAME_SAMPLES]).unwrap().is_none());
        let segment = segmenter.set_pressed(false).unwrap();
        assert_eq!(segment.samples.len(), 2 * FRAME_SAMPLES);
    }

    #[test]
    fn vad_does_not_publish_silence_and_preserves_audio() {
        let mut segmenter =
            SpeechSegmenter::new(DeterministicGate, config(SpeechMode::Automatic)).unwrap();
        for _ in 0..10 {
            assert!(segmenter.process([0; FRAME_SAMPLES]).unwrap().is_none());
        }
        assert!(segmenter.process([300; FRAME_SAMPLES]).unwrap().is_none());
        assert!(segmenter.process([301; FRAME_SAMPLES]).unwrap().is_none());
        for _ in 0..2 {
            assert!(segmenter.process([0; FRAME_SAMPLES]).unwrap().is_none());
        }
        let result = segmenter.process([0; FRAME_SAMPLES]).unwrap().unwrap();
        assert!(result.samples.contains(&300));
        assert!(result.samples.contains(&301));
        assert_eq!(&result.as_wav().unwrap()[..4], b"RIFF");
        assert!(segmenter.flush().is_none());
    }

    #[test]
    fn ptt_requires_press_and_release_and_caps_duration() {
        let mut segmenter =
            SpeechSegmenter::new(DeterministicGate, config(SpeechMode::PushToTalk)).unwrap();
        assert!(segmenter.process([20; FRAME_SAMPLES]).unwrap().is_none());
        segmenter.set_pressed(true);
        for _ in 0..3 {
            assert!(segmenter.process([42; FRAME_SAMPLES]).unwrap().is_none());
        }
        let result = segmenter.set_pressed(false).unwrap();
        assert_eq!(result.samples.len(), 3 * FRAME_SAMPLES);
        assert!(segmenter.set_pressed(false).is_none());
    }

    #[test]
    fn max_duration_forces_segment() {
        let mut segmenter =
            SpeechSegmenter::new(DeterministicGate, config(SpeechMode::PushToTalk)).unwrap();
        segmenter.set_pressed(true);
        let mut result = None;
        for _ in 0..30 {
            result = segmenter.process([1; FRAME_SAMPLES]).unwrap();
        }
        assert_eq!(result.unwrap().samples.len(), 30 * FRAME_SAMPLES);
    }

    #[test]
    fn onset_frames_survive_short_or_disabled_pre_roll() {
        for pre_roll_samples in [0, FRAME_SAMPLES, 2 * FRAME_SAMPLES] {
            let mut segmenter = SpeechSegmenter::new(
                DeterministicGate,
                SegmentConfig {
                    pre_roll_samples,
                    start_voice_samples: 4 * FRAME_SAMPLES,
                    ..config(SpeechMode::Automatic)
                },
            )
            .unwrap();
            for _ in 0..8 {
                assert!(segmenter.process([0; FRAME_SAMPLES]).unwrap().is_none());
            }
            for marker in 1..=4 {
                assert!(
                    segmenter
                        .process([marker; FRAME_SAMPLES])
                        .unwrap()
                        .is_none()
                );
            }
            let segment = segmenter.flush().unwrap();
            assert_eq!(segment.samples.len(), 4 * FRAME_SAMPLES);
            for (frame, marker) in segment
                .samples
                .as_chunks::<FRAME_SAMPLES>()
                .0
                .iter()
                .zip(1..=4)
            {
                assert!(frame.iter().all(|sample| *sample == marker));
            }
        }
    }

    #[test]
    fn onset_cannot_exceed_bounded_segment_capacity() {
        assert!(matches!(
            SpeechSegmenter::new(
                DeterministicGate,
                SegmentConfig {
                    start_voice_samples: 31 * FRAME_SAMPLES,
                    ..config(SpeechMode::Automatic)
                },
            ),
            Err(SegmentError::InvalidConfiguration)
        ));
    }

    #[test]
    fn continuous_voice_across_maximum_preserves_each_frame_once() {
        let mut segmenter = SpeechSegmenter::new(
            DeterministicGate,
            SegmentConfig {
                pre_roll_samples: 0,
                start_voice_samples: 3 * FRAME_SAMPLES,
                min_segment_samples: FRAME_SAMPLES,
                max_segment_samples: 5 * FRAME_SAMPLES,
                ..config(SpeechMode::Automatic)
            },
        )
        .unwrap();
        let mut samples = Vec::new();
        for marker in 1..=13 {
            if let Some(segment) = segmenter.process([marker; FRAME_SAMPLES]).unwrap() {
                assert!(segment.samples.len() <= 5 * FRAME_SAMPLES);
                samples.extend(segment.samples);
            }
        }
        samples.extend(segmenter.flush().unwrap().samples);
        let expected: Vec<_> = (1..=13)
            .flat_map(|marker| [marker; FRAME_SAMPLES])
            .collect();
        assert_eq!(samples, expected);
    }
}
