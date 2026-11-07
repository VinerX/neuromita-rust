//! Read-only native audio inventory. Does not open recording or playback streams.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for device in neuromita::audio::native::discover()? {
        println!("{}\t{}\t{}", device.backend, device.name, device.id);
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if !arguments.is_empty() {
        if arguments.len() != 3 || arguments[0] != "--capture" {
            return Err("usage: audio_devices [--capture <backend> <stable-device-id>]".into());
        }
        use neuromita::audio::{device::AudioCapture, native::NativeCapture};
        let mut capture = NativeCapture::start(&arguments[1], &arguments[2])?;
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(3));
        tokio::pin!(deadline);
        let mut frames = 0;
        let mut peak = 0u16;
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                frame = capture.next_frame() => {
                    let frame = frame?.ok_or("input stream ended")?;
                    frames += 1;
                    peak = peak.max(frame.iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0));
                }
            }
        }
        if frames == 0 {
            return Err("no audio frames received".into());
        }
        println!("Capture check: {frames} PCM16 frames, peak={peak}; no audio saved");
    }
    Ok(())
}
