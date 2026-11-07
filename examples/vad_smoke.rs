use neuromita::audio::vad::{EarshotVad, FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut vad = EarshotVad::new(VoiceThreshold::new(0.5)?);
    let first = vad.process(&[0; FRAME_SAMPLES]);
    for _ in 0..128 {
        let result = vad.process(&[0; FRAME_SAMPLES]);
        assert!(result.probability.is_finite());
        assert!((0.0..=1.0).contains(&result.probability));
    }
    vad.reset();
    assert_eq!(
        first.probability,
        vad.process(&[0; FRAME_SAMPLES]).probability
    );
    let mut peak = 0.0_f32;
    for frame in 0..128 {
        let samples = std::array::from_fn(|index| {
            let time = (frame * FRAME_SAMPLES + index) as f32 / SAMPLE_RATE_HZ as f32;
            (12_000.0 * (time * 220.0 * std::f32::consts::TAU).sin()) as i16
        });
        let result = vad.process(&samples);
        assert!(result.probability.is_finite());
        assert!((0.0..=1.0).contains(&result.probability));
        peak = peak.max(result.probability);
    }
    assert!(VoiceThreshold::new(f32::NAN).is_err());
    println!(
        "Earshot smoke passed: synthetic PCM16, 16 kHz, 256-sample frames; sine peak={peak:.4}. Speech accuracy was not measured."
    );
    Ok(())
}
