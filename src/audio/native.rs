//! Native device identities and bounded PCM16 capture. OS streams own their callbacks.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use super::{
    device::{AudioCapture, DeviceError, DeviceFuture},
    input_catalog::AudioInputDevice,
    vad::{FRAME_SAMPLES, MonoPcm16Frame, SAMPLE_RATE_HZ},
};

fn error(value: impl std::fmt::Display) -> DeviceError {
    DeviceError::Invalid(value.to_string())
}

pub fn discover() -> Result<Vec<AudioInputDevice>, DeviceError> {
    let mut devices = Vec::new();
    for id in cpal::available_hosts() {
        let host = cpal::host_from_id(id).map_err(error)?;
        for device in host.input_devices().map_err(error)? {
            let description = device.description().map_err(error)?;
            devices.push(AudioInputDevice {
                id: device.id().map_err(error)?.to_string(),
                name: description
                    .extended()
                    .first()
                    .map_or_else(|| description.name().to_owned(), Clone::clone),
                backend: id.name().to_owned(),
            });
        }
    }
    Ok(devices)
}

fn selected(backend: &str, identity: &str) -> Result<cpal::Device, DeviceError> {
    let id = cpal::available_hosts()
        .into_iter()
        .find(|id| id.name() == backend)
        .ok_or_else(|| error(format!("audio backend unavailable: {backend}")))?;
    cpal::host_from_id(id)
        .map_err(error)?
        .input_devices()
        .map_err(error)?
        .find(|device| device.id().is_ok_and(|id| id.to_string() == identity))
        .ok_or_else(|| error("selected microphone is no longer available"))
}

pub struct NativeCapture {
    _stream: cpal::Stream,
    receive: mpsc::Receiver<Result<MonoPcm16Frame, String>>,
}

impl NativeCapture {
    pub fn start(backend: &str, identity: &str) -> Result<Self, DeviceError> {
        let device = selected(backend, identity)?;
        let config = device.default_input_config().map_err(error)?;
        let (send, receive) = mpsc::channel(64);
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => input::<f32>(&device, &config.into(), send),
            cpal::SampleFormat::I16 => input::<i16>(&device, &config.into(), send),
            cpal::SampleFormat::I32 => input::<i32>(&device, &config.into(), send),
            cpal::SampleFormat::U16 => input::<u16>(&device, &config.into(), send),
            format => {
                return Err(error(format!(
                    "unsupported capture sample format: {format}"
                )));
            }
        }?;
        stream.play().map_err(error)?;
        Ok(Self {
            _stream: stream,
            receive,
        })
    }
}

impl AudioCapture for NativeCapture {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
        Box::pin(async move {
            self.receive
                .recv()
                .await
                .transpose()
                .map_err(DeviceError::Stopped)
        })
    }
}

struct Converter {
    rate: u32,
    weight: u32,
    sum: f64,
    frame: MonoPcm16Frame,
    filled: usize,
}

impl Converter {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            weight: 0,
            sum: 0.0,
            frame: [0; FRAME_SAMPLES],
            filled: 0,
        }
    }

    fn push(&mut self, mono: f32, mut emit: impl FnMut(MonoPcm16Frame)) {
        let mut remaining = SAMPLE_RATE_HZ;
        while remaining > 0 {
            let part = remaining.min(self.rate - self.weight);
            self.sum += f64::from(mono) * f64::from(part);
            self.weight += part;
            remaining -= part;
            if self.weight == self.rate {
                self.frame[self.filled] = ((self.sum / f64::from(self.rate)).clamp(-1.0, 1.0)
                    * f64::from(i16::MAX))
                .round() as i16;
                self.sum = 0.0;
                self.weight = 0;
                self.filled += 1;
                if self.filled == FRAME_SAMPLES {
                    emit(self.frame);
                    self.filled = 0;
                }
            }
        }
    }
}

fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    send: mpsc::Sender<Result<MonoPcm16Frame, String>>,
) -> Result<cpal::Stream, DeviceError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels);
    let mut converter = Converter::new(config.sample_rate);
    let failure = send.clone();
    let mut overloaded = false;
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                for samples in data.chunks_exact(channels) {
                    let mono = samples
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    converter.push(mono, |frame| {
                        if !overloaded && send.try_send(Ok(frame)).is_err() {
                            overloaded = true;
                        }
                    });
                }
                if overloaded {
                    let _ = send
                        .try_send(Err("microphone consumer is too slow; audio was lost".into()));
                }
            },
            move |problem| {
                let _ = failure.try_send(Err(problem.to_string()));
            },
            None,
        )
        .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversion_preserves_duration_and_dc_at_device_rates() {
        for rate in [16_000, 44_100, 48_000, 96_000] {
            let mut converter = Converter::new(rate);
            let mut frames = Vec::new();
            for _ in 0..rate {
                converter.push(0.25, |frame| frames.push(frame));
            }
            assert_eq!(frames.len(), 62);
            assert!(
                frames
                    .iter()
                    .flatten()
                    .all(|sample| (*sample - 8192).abs() <= 1)
            );
            assert_eq!(converter.filled, 128);
        }
    }
    #[test]
    fn missing_backend_is_an_error() {
        assert!(NativeCapture::start("nonexistent backend", "missing").is_err());
    }
}
