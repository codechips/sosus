//! Default-microphone capture behind a bounded real-time queue.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use thiserror::Error;

use super::health::{StreamEvents, StreamFailure, StreamHealth};

/// Length of the preallocated callback-to-writer queue.
const QUEUE_SECONDS: usize = 2;

/// A running default-input stream.
///
/// Keeping this value alive keeps capture running. Dropping it stops capture.
pub struct MicrophoneCapture {
    _stream: Stream,
    sample_rate: u32,
    channels: u16,
}

impl MicrophoneCapture {
    /// Start the current default microphone with its default hardware format.
    pub fn start_default() -> Result<(Self, MicrophoneReader), MicrophoneCaptureError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(MicrophoneCaptureError::NoDefaultInputDevice)?;
        let supported = device
            .default_input_config()
            .map_err(|source| MicrophoneCaptureError::DefaultInputConfig { source })?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let capacity = (sample_rate as usize)
            .checked_mul(QUEUE_SECONDS)
            .ok_or(MicrophoneCaptureError::QueueCapacityOverflow { sample_rate })?;
        let (producer, consumer) = RingBuffer::new(capacity);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let stream_health = Arc::new(StreamHealth::default());
        let sample_format = supported.sample_format();
        let config = supported.into();

        let stream = match sample_format {
            SampleFormat::I8 => build_stream::<i8>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::I16 => build_stream::<i16>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::I32 => build_stream::<i32>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::I64 => build_stream::<i64>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::U8 => build_stream::<u8>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::U16 => build_stream::<u16>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::U32 => build_stream::<u32>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::U64 => build_stream::<u64>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::F32 => build_stream::<f32>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            SampleFormat::F64 => build_stream::<f64>(
                &device,
                config,
                channels,
                producer,
                Arc::clone(&dropped_frames),
                Arc::clone(&stream_health),
            ),
            unsupported => {
                return Err(MicrophoneCaptureError::UnsupportedSampleFormat {
                    format: unsupported,
                });
            }
        }
        .map_err(|source| MicrophoneCaptureError::BuildStream { source })?;

        stream
            .play()
            .map_err(|source| MicrophoneCaptureError::StartStream { source })?;

        let capture = Self {
            _stream: stream,
            sample_rate,
            channels,
        };
        let reader = MicrophoneReader {
            consumer,
            dropped_frames,
            stream_health,
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

/// Non-real-time read side of the microphone queue.
pub struct MicrophoneReader {
    consumer: Consumer<f32>,
    dropped_frames: Arc<AtomicU64>,
    stream_health: Arc<StreamHealth>,
}

impl MicrophoneReader {
    /// Copy as many queued mono frames as fit in `output`.
    pub fn read_available(&mut self, output: &mut [f32]) -> usize {
        let (filled, _) = self.consumer.pop_partial_slice(output);
        filled.len()
    }

    /// Return and reset the count of frames discarded because the queue was full.
    pub fn take_dropped_frames(&self) -> u64 {
        self.dropped_frames.swap(0, Ordering::Relaxed)
    }

    /// Whether CPAL has reported a stream error since capture started.
    pub fn stream_failure(&self) -> Option<StreamFailure> {
        self.stream_health.failure()
    }

    pub fn take_stream_events(&self) -> StreamEvents {
        self.stream_health.take_events()
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: u16,
    mut producer: Producer<f32>,
    dropped_frames: Arc<AtomicU64>,
    stream_health: Arc<StreamHealth>,
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
        move |error| {
            stream_health.report(error.kind());
        },
        None,
    )
}

/// Downmix and enqueue one callback buffer without allocating or blocking.
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
pub enum MicrophoneCaptureError {
    #[error("no default microphone is available")]
    NoDefaultInputDevice,
    #[error("could not read the default microphone format")]
    DefaultInputConfig {
        #[source]
        source: cpal::Error,
    },
    #[error("microphone queue capacity overflowed for sample rate {sample_rate}")]
    QueueCapacityOverflow { sample_rate: u32 },
    #[error("microphone sample format {format} is not supported")]
    UnsupportedSampleFormat { format: SampleFormat },
    #[error("could not create the microphone stream")]
    BuildStream {
        #[source]
        source: cpal::Error,
    },
    #[error("could not start the microphone stream")]
    StartStream {
        #[source]
        source: cpal::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_interleaved_stereo_to_mono() {
        let (mut producer, mut consumer) = RingBuffer::new(4);
        let dropped = AtomicU64::new(0);

        push_interleaved_mono(&[0.25_f32, 0.75, -0.5, 0.5], 2, &mut producer, &dropped);

        assert_eq!(consumer.pop().unwrap(), 0.5);
        assert_eq!(consumer.pop().unwrap(), 0.0);
        assert!(consumer.pop().is_err());
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn full_queue_drops_new_frames_and_counts_them() {
        let (mut producer, mut consumer) = RingBuffer::new(2);
        let dropped = AtomicU64::new(0);

        push_interleaved_mono(&[0.1_f32, 0.2, 0.3, 0.4], 1, &mut producer, &dropped);

        assert_eq!(consumer.pop().unwrap(), 0.1);
        assert_eq!(consumer.pop().unwrap(), 0.2);
        assert!(consumer.pop().is_err());
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn reader_reports_queue_and_stream_health() {
        let (mut producer, consumer) = RingBuffer::new(4);
        producer.push(0.25).unwrap();
        producer.push(0.5).unwrap();
        let dropped_frames = Arc::new(AtomicU64::new(3));
        let stream_health = Arc::new(StreamHealth::default());
        let mut reader = MicrophoneReader {
            consumer,
            dropped_frames: Arc::clone(&dropped_frames),
            stream_health: Arc::clone(&stream_health),
        };

        let mut output = [0.0; 4];
        assert_eq!(reader.read_available(&mut output), 2);
        assert_eq!(&output[..2], &[0.25, 0.5]);
        assert_eq!(reader.take_dropped_frames(), 3);
        assert_eq!(reader.take_dropped_frames(), 0);
        assert_eq!(reader.stream_failure(), None);
        stream_health.report(cpal::ErrorKind::DeviceNotAvailable);
        assert_eq!(
            reader.stream_failure(),
            Some(StreamFailure::DeviceNotAvailable)
        );
    }
}
