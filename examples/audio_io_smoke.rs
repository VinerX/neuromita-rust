use std::{path::PathBuf, sync::Arc};

use neuromita::{
    audio::{
        device::{AudioCapture, AudioPlayback, CaptureBackend, FfmpegCapture, FfplayOutput},
        segment::SpeechSegment,
    },
    voice::{AudioArtifact, AudioFormat},
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var("FFMPEG_BINARY")?);
    let ffplay = PathBuf::from(std::env::var("FFPLAY_BINARY")?);
    let mut capture = FfmpegCapture::start(
        ffmpeg,
        CaptureBackend::Lavfi,
        "sine=frequency=440:sample_rate=16000:duration=0.1",
    )?;
    let mut samples = Vec::new();
    while let Some(frame) = capture.next_frame().await? {
        samples.extend_from_slice(&frame);
        if samples.len() > 16_384 {
            return Err("unbounded FFmpeg test source".into());
        }
    }
    if samples.len() < 1_536 {
        return Err("synthetic capture returned too little audio".into());
    }
    let bytes = SpeechSegment { samples }.as_wav()?;
    FfplayOutput { binary: ffplay }
        .play(
            AudioArtifact {
                format: AudioFormat::Wave,
                bytes: Arc::from(bytes),
            },
            CancellationToken::new(),
        )
        .await?;
    println!("capture and playback adapter smoke passed (synthetic input)");
    Ok(())
}
