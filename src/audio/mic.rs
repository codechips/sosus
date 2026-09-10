//! Default-microphone capture behind a bounded real-time queue.

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
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
    name: String,
}

/// A microphone-related hardware change while a recording is in progress.
///
/// Sosus reports this separately from the capture stream so it never silently
/// changes a recording's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MicrophoneChange {
    DefaultInput {
        previous: Option<String>,
        current: Option<String>,
    },
    Devices {
        connected: Vec<String>,
        disconnected: Vec<String>,
    },
}

/// A Core Audio listener that does no work in its callback. The recorder
/// resolves microphone names on its regular, non-real-time thread.
pub(crate) struct DefaultInputMonitor {
    state: Arc<DefaultInputMonitorState>,
    default_input_address: AudioObjectPropertyAddress,
    devices_address: AudioObjectPropertyAddress,
    last_default: Option<String>,
    last_input_devices: Vec<InputDevice>,
}

#[derive(Default)]
struct DefaultInputMonitorState {
    changed: AtomicBool,
}

impl DefaultInputMonitor {
    pub(crate) fn start() -> Result<Self, i32> {
        let default_input_address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let devices_address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let state = Arc::new(DefaultInputMonitorState::default());
        let default_status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&default_input_address),
                Some(default_input_changed),
                Arc::as_ptr(&state).cast_mut().cast::<c_void>(),
            )
        };
        if default_status != 0 {
            return Err(default_status);
        }
        let devices_status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&devices_address),
                Some(default_input_changed),
                Arc::as_ptr(&state).cast_mut().cast::<c_void>(),
            )
        };
        if devices_status != 0 {
            unsafe {
                AudioObjectRemovePropertyListener(
                    kAudioObjectSystemObject as AudioObjectID,
                    NonNull::from(&default_input_address),
                    Some(default_input_changed),
                    Arc::as_ptr(&state).cast_mut().cast::<c_void>(),
                );
            }
            return Err(devices_status);
        }
        Ok(Self {
            state,
            default_input_address,
            devices_address,
            last_default: default_microphone_name(),
            last_input_devices: input_devices().unwrap_or_default(),
        })
    }

    pub(crate) fn take_change(&mut self) -> Option<MicrophoneChange> {
        if !self.state.changed.swap(false, Ordering::AcqRel) {
            return None;
        }
        let current = default_microphone_name();
        let current_input_devices =
            input_devices().unwrap_or_else(|_| self.last_input_devices.clone());
        if current != self.last_default {
            let previous = std::mem::replace(&mut self.last_default, current.clone());
            self.last_input_devices = current_input_devices;
            return Some(MicrophoneChange::DefaultInput { previous, current });
        }
        let connected = device_names_not_in(&current_input_devices, &self.last_input_devices);
        let disconnected = device_names_not_in(&self.last_input_devices, &current_input_devices);
        self.last_input_devices = current_input_devices;
        (!connected.is_empty() || !disconnected.is_empty()).then_some(MicrophoneChange::Devices {
            connected,
            disconnected,
        })
    }
}

impl Drop for DefaultInputMonitor {
    fn drop(&mut self) {
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&self.default_input_address),
                Some(default_input_changed),
                Arc::as_ptr(&self.state).cast_mut().cast::<c_void>(),
            )
        };
        if status != 0 {
            tracing::warn!(
                event = "default_microphone_monitor_cleanup",
                error_category = "core_audio_remove_listener",
                os_status = status
            );
        }
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&self.devices_address),
                Some(default_input_changed),
                Arc::as_ptr(&self.state).cast_mut().cast::<c_void>(),
            )
        };
        if status != 0 {
            tracing::warn!(
                event = "microphone_device_monitor_cleanup",
                error_category = "core_audio_remove_listener",
                os_status = status
            );
        }
    }
}

fn device_names_not_in(devices: &[InputDevice], other: &[InputDevice]) -> Vec<String> {
    devices
        .iter()
        .filter(|device| {
            !other
                .iter()
                .any(|other_device| other_device.id == device.id)
        })
        .map(|device| device.name.clone())
        .collect()
}

unsafe extern "C-unwind" fn default_input_changed(
    _object: AudioObjectID,
    _address_count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    // Core Audio supplies the pointer originally registered from the monitor's Arc.
    let Some(state) = (unsafe { (client_data as *const DefaultInputMonitorState).as_ref() }) else {
        return 0;
    };
    state.changed.store(true, Ordering::Release);
    0
}

impl MicrophoneCapture {
    /// Start the current default microphone with its default hardware format.
    pub fn start_default() -> Result<(Self, MicrophoneReader), MicrophoneCaptureError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(MicrophoneCaptureError::NoDefaultInputDevice)?;
        Self::start_device(device)
    }

    /// Start a named microphone, falling back to the current system default when it is gone.
    pub fn start_preferred(
        device_id: &str,
    ) -> Result<(Self, MicrophoneReader, Option<String>), MicrophoneCaptureError> {
        if device_id.is_empty() {
            let (capture, reader) = Self::start_default()?;
            return Ok((capture, reader, None));
        }
        let host = cpal::default_host();
        let device = find_input_device(&host, device_id)?;
        match device {
            Some(device) => match Self::start_device(device) {
                Ok((capture, reader)) => Ok((capture, reader, None)),
                Err(_) => Self::start_default_with_fallback_notice(),
            },
            None => Self::start_default_with_fallback_notice(),
        }
    }

    fn start_default_with_fallback_notice()
    -> Result<(Self, MicrophoneReader, Option<String>), MicrophoneCaptureError> {
        let (capture, reader) = Self::start_default()?;
        let message = format!(
            "Selected microphone is unavailable; using system default ({})",
            capture.name
        );
        Ok((capture, reader, Some(message)))
    }

    fn start_device(
        device: cpal::Device,
    ) -> Result<(Self, MicrophoneReader), MicrophoneCaptureError> {
        let name = device_name(&device);
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
            name,
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

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
}

pub fn input_devices() -> Result<Vec<InputDevice>, MicrophoneCaptureError> {
    let host = cpal::default_host();
    let mut devices = host
        .input_devices()
        .map_err(|source| MicrophoneCaptureError::EnumerateDevices { source })?
        .filter_map(|device| {
            device.id().ok().map(|id| InputDevice {
                id: id.id().to_owned(),
                name: device_name(&device),
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(devices)
}

/// Return the name of the input device macOS currently uses by default.
pub fn default_microphone_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .map(|device| device.to_string())
        .filter(|name| !name.trim().is_empty())
}

fn find_input_device(
    host: &cpal::Host,
    device_id: &str,
) -> Result<Option<cpal::Device>, MicrophoneCaptureError> {
    host.input_devices()
        .map_err(|source| MicrophoneCaptureError::EnumerateDevices { source })
        .map(|mut devices| devices.find(|device| device.id().is_ok_and(|id| id.id() == device_id)))
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| "Unknown microphone".to_owned())
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
    #[error("could not enumerate microphone devices")]
    EnumerateDevices {
        #[source]
        source: cpal::Error,
    },
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

    #[test]
    fn identifies_connected_input_devices_by_stable_identifier() {
        let before = vec![InputDevice {
            id: "built-in".to_owned(),
            name: "MacBook Pro Microphone".to_owned(),
        }];
        let after = vec![
            before[0].clone(),
            InputDevice {
                id: "earpods".to_owned(),
                name: "EarPods Microphone".to_owned(),
            },
        ];

        assert_eq!(device_names_not_in(&after, &before), ["EarPods Microphone"]);
    }
}
