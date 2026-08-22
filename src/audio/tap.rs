//! All-system output capture behind a bounded real-time queue.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use cpal::{
    ErrorKind, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use thiserror::Error;

const QUEUE_SECONDS: usize = 2;

/// A running Core Audio process-tap stream for the default output device.
pub struct SystemAudioCapture {
    _stream: Stream,
    sample_rate: u32,
    channels: u16,
}

impl SystemAudioCapture {
    /// Start loopback capture for the current default output device.
    ///
    /// On macOS, CPAL implements this input stream with a private Core Audio
    /// process tap and aggregate device, both owned by the returned stream.
    pub fn start_default() -> Result<(Self, SystemAudioReader), SystemAudioCaptureError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(SystemAudioCaptureError::NoDefaultOutputDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|source| SystemAudioCaptureError::DefaultOutputConfig { source })?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let capacity = (sample_rate as usize)
            .checked_mul(QUEUE_SECONDS)
            .ok_or(SystemAudioCaptureError::QueueCapacityOverflow { sample_rate })?;
        let (producer, consumer) = RingBuffer::new(capacity);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let stream_failed = Arc::new(AtomicBool::new(false));
        let sample_format = supported.sample_format();
        let config = supported.into();

        macro_rules! build {
            ($sample:ty) => {
                build_stream::<$sample>(
                    &device,
                    config,
                    channels,
                    producer,
                    Arc::clone(&dropped_frames),
                    Arc::clone(&stream_failed),
                )
            };
        }

        let stream = match sample_format {
            SampleFormat::I8 => build!(i8),
            SampleFormat::I16 => build!(i16),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I64 => build!(i64),
            SampleFormat::U8 => build!(u8),
            SampleFormat::U16 => build!(u16),
            SampleFormat::U32 => build!(u32),
            SampleFormat::U64 => build!(u64),
            SampleFormat::F32 => build!(f32),
            SampleFormat::F64 => build!(f64),
            unsupported => {
                return Err(SystemAudioCaptureError::UnsupportedSampleFormat {
                    format: unsupported,
                });
            }
        }
        .map_err(map_build_error)?;

        stream
            .play()
            .map_err(|source| SystemAudioCaptureError::StartStream { source })?;

        let capture = Self {
            _stream: stream,
            sample_rate,
            channels,
        };
        let reader = SystemAudioReader {
            consumer,
            dropped_frames,
            stream_failed,
        };
        Ok((capture, reader))
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }
}

fn map_build_error(source: cpal::Error) -> SystemAudioCaptureError {
    if source.kind() == ErrorKind::PermissionDenied {
        SystemAudioCaptureError::PermissionDenied
    } else {
        SystemAudioCaptureError::BuildStream { source }
    }
}

pub struct SystemAudioReader {
    consumer: Consumer<f32>,
    dropped_frames: Arc<AtomicU64>,
    stream_failed: Arc<AtomicBool>,
}

impl SystemAudioReader {
    pub fn read_available(&mut self, output: &mut [f32]) -> usize {
        let (filled, _) = self.consumer.pop_partial_slice(output);
        filled.len()
    }

    pub fn take_dropped_frames(&self) -> u64 {
        self.dropped_frames.swap(0, Ordering::Relaxed)
    }

    pub fn stream_failed(&self) -> bool {
        self.stream_failed.load(Ordering::Relaxed)
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: u16,
    mut producer: Producer<f32>,
    dropped_frames: Arc<AtomicU64>,
    stream_failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |input: &[T], _| {
            push_interleaved_mono(input, channels, &mut producer, &dropped_frames);
        },
        move |_| {
            stream_failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

fn push_interleaved_mono<T>(
    input: &[T],
    channels: u16,
    producer: &mut Producer<f32>,
    dropped_frames: &AtomicU64,
) where
    T: Sample,
    f32: FromSample<T>,
{
    let channels = usize::from(channels);
    if channels == 0 {
        return;
    }

    for frame in input.chunks_exact(channels) {
        let sum = frame
            .iter()
            .copied()
            .map(f32::from_sample)
            .fold(0.0, |sum, sample| sum + sample);
        let mono = sum / channels as f32;
        if producer.push(mono).is_err() {
            dropped_frames.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Error)]
pub enum SystemAudioCaptureError {
    #[error("no default system output device is available")]
    NoDefaultOutputDevice,
    #[error("could not read the default system output format")]
    DefaultOutputConfig {
        #[source]
        source: cpal::Error,
    },
    #[error("system-audio queue capacity overflowed for sample rate {sample_rate}")]
    QueueCapacityOverflow { sample_rate: u32 },
    #[error("system-audio sample format {format} is not supported")]
    UnsupportedSampleFormat { format: SampleFormat },
    #[error(
        "system audio recording is not allowed; enable sosus in System Settings > Privacy & Security > Screen & System Audio Recording, then run the command again"
    )]
    PermissionDenied,
    #[error("could not create the system-audio stream")]
    BuildStream {
        #[source]
        source: cpal::Error,
    },
    #[error("could not start the system-audio stream")]
    StartStream {
        #[source]
        source: cpal::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_output_and_accounts_for_overflow() {
        let (mut producer, mut consumer) = RingBuffer::new(1);
        let dropped = AtomicU64::new(0);

        push_interleaved_mono(&[0.25_f32, 0.75, -1.0, 1.0], 2, &mut producer, &dropped);

        assert_eq!(consumer.pop().unwrap(), 0.5);
        assert!(consumer.pop().is_err());
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn permission_denial_is_actionable() {
        assert!(matches!(
            map_build_error(cpal::Error::new(ErrorKind::PermissionDenied)),
            SystemAudioCaptureError::PermissionDenied
        ));
    }
}
