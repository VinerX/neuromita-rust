//! Bounded 16-kHz PCM16 segmentation. Device capture is a separate adapter.

use std::collections::VecDeque;

use thiserror::Error;

use super::vad::{EarshotVad, FRAME_SAMPLES, MonoPcm16Frame, SAMPLE_RATE_HZ, VoiceDecision};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SegmentError {
    #[error("invalid speech segmentation limits")]
    InvalidConfiguration,
    #[error("speech segment exceeds the configured limit")]
    TooLong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechMode {
    Automatic,
    PushToTalk,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    pub mode: SpeechMode,
    pub pre_roll_frames: usize,
    pub start_voice_frames: usize,
    pub end_silence_frames: usize,
    pub min_segment_frames: usize,
    pub max_segment_frames: usize,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            mode: SpeechMode::Automatic,
            pre_roll_frames: 12,
            start_voice_frames: 3,
            end_silence_frames: 28,
            min_segment_frames: 10,
            max_segment_frames: 625,
        }
    }
}

pub trait VoiceGate: Send {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> VoiceDecision;
    fn reset(&mut self);
}

impl VoiceGate for EarshotVad {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> VoiceDecision {
        self.process(frame)
    }

    fn reset(&mut self) {
        EarshotVad::reset(self);
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
    pre_roll: VecDeque<MonoPcm16Frame>,
    active: Vec<i16>,
    voice_run: usize,
    silence_run: usize,
    pressed: bool,
}

impl<G: VoiceGate> SpeechSegmenter<G> {
    pub fn new(gate: G, config: SegmentConfig) -> Result<Self, SegmentError> {
        if config.pre_roll_frames > config.max_segment_frames
            || config.start_voice_frames == 0
            || config.end_silence_frames == 0
            || config.min_segment_frames == 0
            || config.max_segment_frames < config.min_segment_frames
            || config.max_segment_frames > 37_500
        {
            return Err(SegmentError::InvalidConfiguration);
        }
        Ok(Self {
            gate,
            config,
            pre_roll: VecDeque::with_capacity(config.pre_roll_frames),
            active: Vec::new(),
            voice_run: 0,
            silence_run: 0,
            pressed: false,
        })
    }

    pub fn process(&mut self, frame: MonoPcm16Frame) -> Option<SpeechSegment> {
        match self.config.mode {
            SpeechMode::PushToTalk => {
                if !self.pressed {
                    return None;
                }
                self.active.extend_from_slice(&frame);
            }
            SpeechMode::Automatic => {
                let voice = self.gate.inspect(&frame).speech;
                if self.active.is_empty() {
                    if self.config.pre_roll_frames > 0 {
                        if self.pre_roll.len() == self.config.pre_roll_frames {
                            self.pre_roll.pop_front();
                        }
                        self.pre_roll.push_back(frame);
                    }
                    self.voice_run = if voice { self.voice_run + 1 } else { 0 };
                    if self.voice_run < self.config.start_voice_frames {
                        return None;
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
                    self.silence_run = if voice { 0 } else { self.silence_run + 1 };
                    if self.silence_run >= self.config.end_silence_frames {
                        return self.finish();
                    }
                }
            }
        }
        if self.active.len() >= self.config.max_segment_frames * FRAME_SAMPLES {
            return self.finish();
        }
        None
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
        let enough = self.active.len() >= self.config.min_segment_frames * FRAME_SAMPLES;
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
        fn inspect(&mut self, frame: &MonoPcm16Frame) -> VoiceDecision {
            VoiceDecision {
                probability: if frame[0] != 0 { 1.0 } else { 0.0 },
                speech: frame[0] != 0,
            }
        }
        fn reset(&mut self) {}
    }

    fn config(mode: SpeechMode) -> SegmentConfig {
        SegmentConfig {
            mode,
            pre_roll_frames: 3,
            start_voice_frames: 2,
            end_silence_frames: 3,
            min_segment_frames: 2,
            max_segment_frames: 30,
        }
    }

    #[test]
    fn speech_longer_than_ten_seconds_is_not_forcibly_cut() {
        let mut config = config(SpeechMode::PushToTalk);
        config.max_segment_frames = 2_000;
        let mut segmenter = SpeechSegmenter::new(DeterministicGate, config).unwrap();
        segmenter.set_pressed(true);
        for _ in 0..1_500 {
            assert!(segmenter.process([1; FRAME_SAMPLES]).is_none());
        }
        let segment = segmenter.set_pressed(false).unwrap();
        assert_eq!(segment.samples.len(), 1_500 * FRAME_SAMPLES);
        assert_eq!(
            segment.as_wav().unwrap().len(),
            44 + 1_500 * FRAME_SAMPLES * 2
        );
    }

    #[test]
    fn playback_suppression_preserves_ptt_button_state() {
        let config = SegmentConfig {
            mode: SpeechMode::PushToTalk,
            min_segment_frames: 2,
            ..SegmentConfig::default()
        };
        let mut segmenter = SpeechSegmenter::new(DeterministicGate, config).unwrap();
        assert!(segmenter.set_pressed(true).is_none());
        segmenter.suppress_playback_echo();
        assert!(segmenter.process([5; FRAME_SAMPLES]).is_none());
        assert!(segmenter.process([6; FRAME_SAMPLES]).is_none());
        let segment = segmenter.set_pressed(false).unwrap();
        assert_eq!(segment.samples.len(), 2 * FRAME_SAMPLES);
    }

    #[test]
    fn vad_does_not_publish_silence_and_preserves_audio() {
        let mut segmenter =
            SpeechSegmenter::new(DeterministicGate, config(SpeechMode::Automatic)).unwrap();
        for _ in 0..10 {
            assert!(segmenter.process([0; FRAME_SAMPLES]).is_none());
        }
        assert!(segmenter.process([300; FRAME_SAMPLES]).is_none());
        assert!(segmenter.process([301; FRAME_SAMPLES]).is_none());
        for _ in 0..2 {
            assert!(segmenter.process([0; FRAME_SAMPLES]).is_none());
        }
        let result = segmenter.process([0; FRAME_SAMPLES]).unwrap();
        assert!(result.samples.contains(&300));
        assert!(result.samples.contains(&301));
        assert_eq!(&result.as_wav().unwrap()[..4], b"RIFF");
        assert!(segmenter.flush().is_none());
    }

    #[test]
    fn ptt_requires_press_and_release_and_caps_duration() {
        let mut segmenter =
            SpeechSegmenter::new(DeterministicGate, config(SpeechMode::PushToTalk)).unwrap();
        assert!(segmenter.process([20; FRAME_SAMPLES]).is_none());
        segmenter.set_pressed(true);
        for _ in 0..3 {
            assert!(segmenter.process([42; FRAME_SAMPLES]).is_none());
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
            result = segmenter.process([1; FRAME_SAMPLES]);
        }
        assert_eq!(result.unwrap().samples.len(), 30 * FRAME_SAMPLES);
    }
}
