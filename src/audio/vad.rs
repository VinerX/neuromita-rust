use thiserror::Error;

pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 256;
pub type MonoPcm16Frame = [i16; FRAME_SAMPLES];

#[derive(Debug, Error)]
#[error("voice threshold must be finite and between 0 and 1")]
pub struct InvalidThreshold;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceThreshold(f32);

impl VoiceThreshold {
    pub fn new(value: f32) -> Result<Self, InvalidThreshold> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(InvalidThreshold);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VoiceDecision {
    pub probability: f32,
    pub speech: bool,
}

/// One detector per 16 kHz mono PCM16 stream. Capture/resampling belongs to the
/// audio input adapter; no device or Python worker is opened by this component.
pub struct EarshotVad {
    detector: earshot::Detector,
    threshold: VoiceThreshold,
}

impl EarshotVad {
    pub fn new(threshold: VoiceThreshold) -> Self {
        Self {
            detector: earshot::Detector::default(),
            threshold,
        }
    }

    pub fn process(&mut self, frame: &MonoPcm16Frame) -> VoiceDecision {
        let probability = self.detector.predict_i16(frame);
        VoiceDecision {
            probability,
            speech: probability > self.threshold.0,
        }
    }

    pub fn reset(&mut self) {
        self.detector.reset();
    }
}
