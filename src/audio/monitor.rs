//! Core-owned, cancellable microphone loopback. Audio is bounded and never persisted.

use super::{
    device::{AudioCapture, DeviceError},
    native::NativeCapture,
    vad::SAMPLE_RATE_HZ,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorSnapshot {
    pub running: bool,
    pub level: u8,
    pub device: String,
    pub error: Option<String>,
}

pub struct MicrophoneMonitor {
    updates: watch::Sender<MonitorSnapshot>,
    active: Option<(CancellationToken, JoinHandle<()>)>,
}

impl Default for MicrophoneMonitor {
    fn default() -> Self {
        Self {
            updates: watch::channel(MonitorSnapshot::default()).0,
            active: None,
        }
    }
}

impl MicrophoneMonitor {
    pub fn observe(&self) -> watch::Receiver<MonitorSnapshot> {
        self.updates.subscribe()
    }

    pub async fn start(&mut self, backend: &str, identity: &str) -> Result<(), DeviceError> {
        self.stop().await;
        let setup = NativeCapture::start(backend, identity).and_then(|capture| {
            NativeOutput::start(SAMPLE_RATE_HZ).map(|output| (capture, output))
        });
        let (mut capture, output) = match setup {
            Ok(value) => value,
            Err(error) => {
                self.updates.send_replace(MonitorSnapshot {
                    error: Some(error.to_string()),
                    ..Default::default()
                });
                return Err(error);
            }
        };
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let updates = self.updates.clone();
        let device = identity.to_owned();
        updates.send_replace(MonitorSnapshot {
            running: true,
            device: device.clone(),
            ..Default::default()
        });
        let task = tokio::spawn(async move {
            let mut frames = 0;
            let result: Result<(), DeviceError> = async {
                loop {
                    tokio::select! {
                        _ = cancelled.cancelled() => return Ok(()),
                        frame = capture.next_frame() => {
                            let frame = frame?.ok_or_else(|| DeviceError::Stopped("microphone stream ended".into()))?;
                            output.check()?;
                            output.write(&frame);
                            let rms = (frame.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / frame.len() as f64).sqrt() / f64::from(i16::MAX);
                            frames += 1;
                            if frames % 6 == 0 {
                                updates.send_replace(MonitorSnapshot { running: true, device: device.clone(), level: (rms * 100.0).round().clamp(0.0, 100.0) as u8, error: None });
                            }
                        }
                    }
                }
            }.await;
            updates.send_replace(MonitorSnapshot {
                device,
                error: result.err().map(|error| error.to_string()),
                ..Default::default()
            });
        });
        self.active = Some((stop, task));
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some((stop, task)) = self.active.take() {
            stop.cancel();
            let _ = task.await;
        }
    }
}

impl Drop for MicrophoneMonitor {
    fn drop(&mut self) {
        if let Some((stop, task)) = self.active.take() {
            stop.cancel();
            task.abort();
        }
    }
}

pub struct NativeOutput {
    _stream: cpal::Stream,
    queue: Arc<Mutex<VecDeque<i16>>>,
    failure: Arc<Mutex<Option<String>>>,
    capacity: usize,
}

pub struct NativePlayback;

impl super::device::AudioPlayback for NativePlayback {
    fn play<'a>(
        &'a self,
        audio: crate::voice::AudioArtifact,
        stop: CancellationToken,
    ) -> super::device::DeviceFuture<'a, ()> {
        Box::pin(async move {
            let (rate, samples) = pcm_wave(&audio.bytes)?;
            let output = NativeOutput::start(rate)?;
            for chunk in samples.chunks(256) {
                loop {
                    if stop.is_cancelled() {
                        return Ok(());
                    }
                    output.check()?;
                    let queued = output
                        .queue
                        .lock()
                        .map_err(|_| DeviceError::Stopped("output state poisoned".into()))?
                        .len();
                    if queued + chunk.len() <= output.capacity {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                output.write(chunk);
            }
            loop {
                if stop.is_cancelled() {
                    return Ok(());
                }
                output.check()?;
                if output
                    .queue
                    .lock()
                    .map_err(|_| DeviceError::Stopped("output state poisoned".into()))?
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tokio::select! {
                _ = stop.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            }
            Ok(())
        })
    }
}

fn pcm_wave(bytes: &[u8]) -> Result<(u32, Vec<i16>), DeviceError> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(DeviceError::UnsupportedFormat);
    }
    let mut format = None;
    let mut data = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or(DeviceError::UnsupportedFormat)?;
        match &bytes[offset..offset + 4] {
            b"fmt " if length >= 16 => {
                let encoding = u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap());
                let channels =
                    u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()) as usize;
                let rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
                let bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap());
                if encoding != 1
                    || bits != 16
                    || channels == 0
                    || !(8_000..=192_000).contains(&rate)
                {
                    return Err(DeviceError::UnsupportedFormat);
                }
                format = Some((rate, channels));
            }
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        offset = end + length % 2;
    }
    let (rate, channels) = format.ok_or(DeviceError::UnsupportedFormat)?;
    let data = data.ok_or(DeviceError::UnsupportedFormat)?;
    if data.len() % (channels * 2) != 0 {
        return Err(DeviceError::UnsupportedFormat);
    }
    let samples = data
        .chunks_exact(channels * 2)
        .map(|frame| {
            (frame
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i64::from(i16::from_le_bytes([sample[0], sample[1]])))
                .sum::<i64>()
                / channels as i64) as i16
        })
        .collect();
    Ok((rate, samples))
}

impl NativeOutput {
    /// Loopback follows the OS default output; input selection is always explicit.
    pub fn start(source_rate: u32) -> Result<Self, DeviceError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| DeviceError::Invalid("no system output device".into()))?;
        let config = device
            .default_output_config()
            .map_err(|error| DeviceError::Invalid(error.to_string()))?;
        let capacity = source_rate as usize / 5;
        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
        let failure = Arc::new(Mutex::new(None));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => output::<f32>(
                &device,
                &config.into(),
                source_rate,
                queue.clone(),
                failure.clone(),
            ),
            cpal::SampleFormat::I16 => output::<i16>(
                &device,
                &config.into(),
                source_rate,
                queue.clone(),
                failure.clone(),
            ),
            cpal::SampleFormat::I32 => output::<i32>(
                &device,
                &config.into(),
                source_rate,
                queue.clone(),
                failure.clone(),
            ),
            cpal::SampleFormat::U16 => output::<u16>(
                &device,
                &config.into(),
                source_rate,
                queue.clone(),
                failure.clone(),
            ),
            format => {
                return Err(DeviceError::Invalid(format!(
                    "unsupported output sample format: {format}"
                )));
            }
        }
        .map_err(|error| DeviceError::Invalid(error.to_string()))?;
        stream
            .play()
            .map_err(|error| DeviceError::Invalid(error.to_string()))?;
        Ok(Self {
            _stream: stream,
            queue,
            failure,
            capacity,
        })
    }

    pub fn write(&self, samples: &[i16]) {
        if let Ok(mut queue) = self.queue.lock() {
            let excess = (queue.len() + samples.len()).saturating_sub(self.capacity);
            let remove = excess.min(queue.len());
            queue.drain(..remove);
            queue.extend(samples.iter().rev().take(self.capacity).rev());
        }
    }

    pub fn check(&self) -> Result<(), DeviceError> {
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| DeviceError::Stopped("output state poisoned".into()))?
            .clone()
        {
            return Err(DeviceError::Stopped(error));
        }
        Ok(())
    }
}

fn output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    source_rate: u32,
    queue: Arc<Mutex<VecDeque<i16>>>,
    failure: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let mut phase = rate;
    let mut current = 0.0f32;
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut queue = queue.try_lock().ok();
            for frame in data.chunks_exact_mut(channels) {
                while phase >= rate {
                    current = queue
                        .as_mut()
                        .and_then(|queue| queue.pop_front())
                        .map_or(0.0, |sample| f32::from(sample) / f32::from(i16::MAX));
                    phase -= rate;
                }
                frame.fill(T::from_sample(current));
                phase += source_rate;
            }
        },
        move |error| {
            if let Ok(mut failure) = failure.lock() {
                *failure = Some(error.to_string());
            }
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_selection_is_observable_and_does_not_start_recording() {
        let mut monitor = MicrophoneMonitor::default();
        let mut updates = monitor.observe();
        assert!(!updates.borrow().running);
        assert!(
            monitor
                .start("missing backend", "missing device")
                .await
                .is_err()
        );
        updates.changed().await.unwrap();
        assert!(!updates.borrow().running);
        assert!(updates.borrow().error.is_some());
        monitor.stop().await;
    }

    #[test]
    fn playback_reads_pcm_and_rejects_truncated_chunks() {
        let wave = crate::audio::segment::SpeechSegment {
            samples: vec![123; 512],
        }
        .as_wav()
        .unwrap();
        let (rate, samples) = pcm_wave(&wave).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples, vec![123; 512]);
        assert!(pcm_wave(&wave[..wave.len() - 1]).is_err());
        assert!(pcm_wave(b"invalid").is_err());
    }
}
