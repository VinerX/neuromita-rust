//! Manual spike: real Silero inference through the production score pipeline.
//!
//! Usage:
//!   cargo run --example silero_spike --features vad-silero -- <model.onnx> <onnxruntime.dll>

use std::{error::Error, time::Instant};

use neuromita::audio::{
    score::{ScoreError, ScoreFrames, VadScoreProvider, VerifiedSileroAsset},
    segment::{SegmentConfig, SpeechMode, SpeechSegmenter, ScoreGate},
    vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold},
};
use wavekat_vad::{VoiceActivityDetector, backends::silero::SileroVad};

struct SileroScores(SileroVad);

impl VadScoreProvider for SileroScores {
    fn frame_samples(&self) -> usize {
        512
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        self.0
            .process(frame, SAMPLE_RATE_HZ)
            .map_err(|error| ScoreError::Provider(error.to_string()))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

fn synthetic_clip() -> (Vec<i16>, usize, usize) {
    let silence = SAMPLE_RATE_HZ as usize;
    let voiced_len = (SAMPLE_RATE_HZ as f64 * 1.2) as usize;
    let mut samples = vec![0i16; silence + voiced_len + silence];
    for index in 0..voiced_len {
        let t = index as f64 / SAMPLE_RATE_HZ as f64;
        let value = (2.0 * std::f64::consts::PI * 180.0 * t).sin()
            + 0.3 * (2.0 * std::f64::consts::PI * 540.0 * t).sin();
        let edge = (index.min(voiced_len - index) as f64 / 240.0).min(1.0);
        samples[silence + index] = (value * 6000.0 * edge) as i16;
    }
    (samples, silence, silence + voiced_len)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model, dll] = args.as_slice() else {
        return Err("usage: silero_spike <model.onnx> <onnxruntime.dll>".into());
    };

    ort::init_from(dll)?.with_name("neuromita-spike").commit();
    let asset = VerifiedSileroAsset::open(std::path::Path::new(model))?;
    println!("model verified: {} bytes", asset.bytes().len());

    let vad = SileroVad::from_memory(asset.bytes(), SAMPLE_RATE_HZ)?;
    let (pcm, voiced_start, voiced_end) = synthetic_clip();
    println!(
        "clip: {} samples, voiced {}..{}",
        pcm.len(),
        voiced_start,
        voiced_end
    );

    let started = Instant::now();
    let mut framing = ScoreFrames::new(SileroScores(vad))?;
    let mut voiced = Vec::new();
    let mut quiet = Vec::new();
    for chunk in pcm.chunks(512) {
        if chunk.len() < 512 {
            break;
        }
        for score in framing.push(chunk)? {
            let midpoint = (score.start_sample + score.end_sample) / 2;
            if (voiced_start as u64..voiced_end as u64).contains(&midpoint) {
                voiced.push(score.probability);
            } else {
                quiet.push(score.probability);
            }
        }
    }
    let elapsed = started.elapsed();
    println!(
        "windows: voiced {} quiet {} processing {:?}",
        voiced.len(),
        quiet.len(),
        elapsed
    );
    if let (Some(min), Some(max)) = (
        voiced.iter().cloned().reduce(f32::min),
        voiced.iter().cloned().reduce(f32::max),
    ) {
        println!("voiced probability min {min:.3} max {max:.3}");
    }
    if let (Some(min), Some(max)) = (
        quiet.iter().cloned().reduce(f32::min),
        quiet.iter().cloned().reduce(f32::max),
    ) {
        println!("quiet probability min {min:.3} max {max:.3}");
    }

    let threshold = VoiceThreshold::new(0.5)?;
    let gate = ScoreGate::new(
        SileroScores(SileroVad::from_memory(asset.bytes(), SAMPLE_RATE_HZ)?),
        threshold,
    )?;
    let mut segmenter = SpeechSegmenter::new(
        gate,
        SegmentConfig {
            mode: SpeechMode::Automatic,
            ..SegmentConfig::default()
        },
    )?;
    let mut segments = 0;
    let mut lengths = Vec::new();
    for frame in pcm.chunks_exact(FRAME_SAMPLES) {
        let frame: [i16; FRAME_SAMPLES] = frame.try_into()?;
        if let Some(segment) = segmenter.process(frame)? {
            segments += 1;
            lengths.push(segment.samples.len());
        }
    }
    if let Some(segment) = segmenter.flush() {
        segments += 1;
        lengths.push(segment.samples.len());
    }
    println!("segments: {segments} lengths {lengths:?}");
    Ok(())
}
