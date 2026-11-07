//! Explicit FFmpeg device adapters. The binaries are supplied by the host application;
//! neither shell commands nor system-default device names are guessed by the core.

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

use crate::voice::{AudioArtifact, AudioFormat};

use super::vad::{FRAME_SAMPLES, MonoPcm16Frame};

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("invalid audio device configuration: {0}")]
    Invalid(String),
    #[error("audio device I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("audio device exited unexpectedly: {0}")]
    Stopped(String),
    #[error("unsupported audio format")]
    UnsupportedFormat,
}

pub type DeviceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DeviceError>> + Send + 'a>>;

pub trait AudioCapture: Send {
    /// Cancellation must preserve any partial PCM frame for the next call.
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>>;
}

impl AudioCapture for Box<dyn AudioCapture> {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
        self.as_mut().next_frame()
    }
}

pub trait AudioPlayback: Send + Sync {
    fn play<'a>(&'a self, audio: AudioArtifact, stop: CancellationToken) -> DeviceFuture<'a, ()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Native,
    Pulse,
    Alsa,
    Dshow,
    Avfoundation,
    Lavfi,
}

impl CaptureBackend {
    fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Pulse => "pulse",
            Self::Alsa => "alsa",
            Self::Dshow => "dshow",
            Self::Avfoundation => "avfoundation",
            Self::Lavfi => "lavfi",
        }
    }
}

pub struct FfmpegCapture {
    child: Child,
    stdout: Pcm16Frames<ChildStdout>,
}

struct Pcm16Frames<R> {
    reader: R,
    bytes: [u8; FRAME_SAMPLES * 2],
    filled: usize,
}

impl<R: AsyncRead + Unpin> Pcm16Frames<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            bytes: [0; FRAME_SAMPLES * 2],
            filled: 0,
        }
    }

    async fn next(&mut self) -> Result<Option<MonoPcm16Frame>, DeviceError> {
        while self.filled < self.bytes.len() {
            let count = self.reader.read(&mut self.bytes[self.filled..]).await?;
            if count == 0 {
                return if self.filled == 0 {
                    Ok(None)
                } else {
                    Err(DeviceError::Invalid("truncated PCM16 frame".into()))
                };
            }
            self.filled += count;
        }
        let frame =
            std::array::from_fn(|i| i16::from_le_bytes([self.bytes[2 * i], self.bytes[2 * i + 1]]));
        self.filled = 0;
        Ok(Some(frame))
    }
}

impl FfmpegCapture {
    pub fn start(
        binary: PathBuf,
        backend: CaptureBackend,
        device: &str,
    ) -> Result<Self, DeviceError> {
        if !binary.is_absolute() || device.is_empty() || device.len() > 512 || device.contains('\0')
        {
            return Err(DeviceError::Invalid(
                "explicit binary and device identity required".into(),
            ));
        }
        let mut child = Command::new(binary)
            .args([
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "error",
                "-f",
                backend.ffmpeg_name(),
                "-i",
                device,
                "-ac",
                "1",
                "-ar",
                "16000",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DeviceError::Invalid("no capture pipe".into()))?;
        Ok(Self {
            child,
            stdout: Pcm16Frames::new(stdout),
        })
    }
}

impl AudioCapture for FfmpegCapture {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
        Box::pin(async move {
            match self.stdout.next().await? {
                Some(frame) => Ok(Some(frame)),
                None => {
                    let status = self.child.wait().await?;
                    if status.success() {
                        Ok(None)
                    } else {
                        Err(DeviceError::Stopped(format!("capture ended: {status}")))
                    }
                }
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct FfplayOutput {
    pub binary: PathBuf,
}

impl AudioPlayback for FfplayOutput {
    fn play<'a>(&'a self, audio: AudioArtifact, stop: CancellationToken) -> DeviceFuture<'a, ()> {
        Box::pin(async move {
            if !self.binary.is_absolute() {
                return Err(DeviceError::Invalid(
                    "FFplay binary must be absolute".into(),
                ));
            }
            if audio.format != AudioFormat::Wave
                || audio.bytes.len() < 44
                || audio.bytes.len() > 64 * 1024 * 1024
                || &audio.bytes[..4] != b"RIFF"
                || &audio.bytes[8..12] != b"WAVE"
            {
                return Err(DeviceError::UnsupportedFormat);
            }
            let mut child = Command::new(&self.binary)
                .args([
                    "-hide_banner",
                    "-nodisp",
                    "-autoexit",
                    "-loglevel",
                    "error",
                    "-i",
                    "pipe:0",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| DeviceError::Invalid("no playback pipe".into()))?;
            tokio::select! {
                biased;
                _ = stop.cancelled() => { let _ = child.kill().await; return Ok(()); }
                write = stdin.write_all(&audio.bytes) => write?,
            }
            drop(stdin);
            let status = tokio::select! {
                biased;
                _ = stop.cancelled() => { let _ = child.kill().await; return Ok(()); }
                result = child.wait() => result?,
            };
            if status.success() {
                Ok(())
            } else {
                Err(DeviceError::Stopped(format!("playback exited {status}")))
            }
        })
    }
}

pub struct SilentPlayback;
impl AudioPlayback for SilentPlayback {
    fn play<'a>(&'a self, _audio: AudioArtifact, _stop: CancellationToken) -> DeviceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub type SharedPlayback = Arc<dyn AudioPlayback>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn interrupted_pcm_read_preserves_partial_samples() {
        let frame = std::array::from_fn::<_, FRAME_SAMPLES, _>(|i| (i as i16 - 128) * 73);
        let bytes: Vec<_> = frame
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let (mut sender, receiver) = tokio::io::duplex(bytes.len());
        let mut frames = Pcm16Frames::new(receiver);
        sender.write_all(&bytes[..3]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), frames.next())
                .await
                .is_err()
        );
        sender.write_all(&bytes[3..]).await.unwrap();
        sender.shutdown().await.unwrap();
        assert_eq!(frames.next().await.unwrap(), Some(frame));
        assert_eq!(frames.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn truncated_pcm_is_an_error() {
        let (mut sender, receiver) = tokio::io::duplex(8);
        sender.write_all(&[1, 2, 3]).await.unwrap();
        sender.shutdown().await.unwrap();
        assert!(matches!(
            Pcm16Frames::new(receiver).next().await,
            Err(DeviceError::Invalid(_))
        ));
    }
}
