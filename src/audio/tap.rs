//! All-system output capture behind a bounded real-time queue.

use std::{
    ffi::{CStr, c_void},
    mem::size_of,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use cpal::{
    ErrorKind, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use objc2::AnyThread;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePermissionsError,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioSubDeviceInputChannelsKey, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey,
};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, kCFAllocatorDefault,
    kCFTypeArrayCallBacks, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use objc2_foundation::{NSArray, NSNumber, NSString};
use rtrb::{Consumer, Producer, RingBuffer};
use thiserror::Error;

const QUEUE_SECONDS: usize = 2;
const AGGREGATE_LOOKUP_ATTEMPTS: usize = 50;
const AGGREGATE_LOOKUP_DELAY: Duration = Duration::from_millis(10);
static NEXT_INSTANCE: AtomicU32 = AtomicU32::new(1);

/// A running Core Audio process-tap stream for the default output device.
pub struct SystemAudioCapture {
    stream: Option<Stream>,
    hardware: Option<OwnedTapAggregate>,
    sample_rate: u32,
    channels: u16,
}

impl SystemAudioCapture {
    /// Start loopback capture for the current default output device.
    ///
    /// Sosus owns the process tap and private aggregate device. The tap excludes
    /// this process, and the aggregate is destroyed before the tap on drop.
    pub fn start_default() -> Result<(Self, SystemAudioReader), SystemAudioCaptureError> {
        let host = cpal::default_host();
        let output = host
            .default_output_device()
            .ok_or(SystemAudioCaptureError::NoDefaultOutputDevice)?;
        let output_uid = output
            .id()
            .map_err(|source| SystemAudioCaptureError::DefaultOutputIdentifier { source })?
            .id()
            .to_owned();

        let hardware = OwnedTapAggregate::create(&output_uid)?;
        let device = find_aggregate_device(&host, hardware.aggregate_uid()).ok_or_else(|| {
            SystemAudioCaptureError::AggregateNotEnumerated {
                uid: hardware.aggregate_uid().to_owned(),
            }
        })?;
        let supported = device
            .default_input_config()
            .map_err(|source| SystemAudioCaptureError::DefaultInputConfig { source })?;
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
            stream: Some(stream),
            hardware: Some(hardware),
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

impl Drop for SystemAudioCapture {
    fn drop(&mut self) {
        drop(self.stream.take());
        drop(self.hardware.take());
    }
}

fn find_aggregate_device(host: &cpal::Host, aggregate_uid: &str) -> Option<cpal::Device> {
    let id = cpal::DeviceId::new(cpal::HostId::CoreAudio, aggregate_uid);

    for attempt in 0..AGGREGATE_LOOKUP_ATTEMPTS {
        if let Some(device) = host.device_by_id(&id) {
            return Some(device);
        }
        if attempt + 1 < AGGREGATE_LOOKUP_ATTEMPTS {
            thread::sleep(AGGREGATE_LOOKUP_DELAY);
        }
    }

    None
}

struct OwnedTapAggregate {
    tap_id: Option<AudioObjectID>,
    aggregate_id: Option<AudioObjectID>,
    aggregate_uid: String,
}

impl OwnedTapAggregate {
    fn create(output_uid: &str) -> Result<Self, SystemAudioCaptureError> {
        let instance = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let aggregate_uid = format!("dev.sosus.cli.system-audio.aggregate.{pid}.{instance}");
        let aggregate_name = format!("sosus system audio {pid}.{instance}");
        let tap_name = format!("sosus system audio tap {pid}.{instance}");

        let process_id = current_process_object()?;
        let process = NSNumber::new_u32(process_id);
        let excluded_processes = [&*process];
        let excluded_processes = NSArray::from_slice(&excluded_processes);
        let output_uid_ns = NSString::from_str(output_uid);
        let description = unsafe {
            CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &excluded_processes,
                &output_uid_ns,
                0,
            )
        };
        unsafe {
            description.setExclusive(true);
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
            description.setName(&NSString::from_str(&tap_name));
        }

        let mut tap_id = kAudioObjectUnknown;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap_id) };
        check_core_audio_status("create process tap", status)?;
        if tap_id == kAudioObjectUnknown {
            return Err(SystemAudioCaptureError::CoreAudioObjectUnavailable {
                operation: "create process tap",
            });
        }

        let mut owner = Self {
            tap_id: Some(tap_id),
            aggregate_id: None,
            aggregate_uid,
        };
        let tap_uid = unsafe { description.UUID().UUIDString() };
        let properties = create_aggregate_properties(
            output_uid,
            &tap_uid,
            &owner.aggregate_uid,
            &aggregate_name,
        )?;
        let mut aggregate_id = kAudioObjectUnknown;
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(&properties, NonNull::from(&mut aggregate_id))
        };
        check_core_audio_status("create private aggregate device", status)?;
        if aggregate_id == kAudioObjectUnknown {
            return Err(SystemAudioCaptureError::CoreAudioObjectUnavailable {
                operation: "create private aggregate device",
            });
        }
        owner.aggregate_id = Some(aggregate_id);

        Ok(owner)
    }

    fn aggregate_uid(&self) -> &str {
        &self.aggregate_uid
    }
}

impl Drop for OwnedTapAggregate {
    fn drop(&mut self) {
        let mut cleanup_failed = false;
        if let Some(aggregate_id) = self.aggregate_id.take() {
            let status = unsafe { AudioHardwareDestroyAggregateDevice(aggregate_id) };
            cleanup_failed |= status != 0;
        }
        if let Some(tap_id) = self.tap_id.take() {
            let status = unsafe { AudioHardwareDestroyProcessTap(tap_id) };
            cleanup_failed |= status != 0;
        }

        if cleanup_failed {
            tracing::warn!(
                event = "system_audio_cleanup",
                status = "failed",
                error_category = "core_audio_destroy"
            );
        } else {
            tracing::info!(event = "system_audio_cleanup", status = "complete");
        }
    }
}

fn current_process_object() -> Result<AudioObjectID, SystemAudioCaptureError> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let pid: libc::pid_t = unsafe { libc::getpid() };
    let mut process_id = kAudioObjectUnknown;
    let mut data_size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            size_of::<libc::pid_t>() as u32,
            (&pid as *const libc::pid_t).cast(),
            NonNull::from(&mut data_size),
            NonNull::from(&mut process_id).cast(),
        )
    };
    check_core_audio_status("translate current process", status)?;
    if process_id == kAudioObjectUnknown {
        return Err(SystemAudioCaptureError::CurrentProcessUnavailable);
    }

    Ok(process_id)
}

fn create_aggregate_properties(
    output_uid: &str,
    tap_uid: &NSString,
    aggregate_uid: &str,
    aggregate_name: &str,
) -> Result<CFRetained<CFDictionary>, SystemAudioCaptureError> {
    let subdevice = new_dictionary(2, "subdevice dictionary")?;
    let output_uid_cf = CFString::from_str(output_uid);
    let zero = NSNumber::new_u32(0);
    unsafe {
        set_dictionary_value(&subdevice, kAudioSubDeviceUIDKey, &*output_uid_cf);
        set_dictionary_value(&subdevice, kAudioSubDeviceInputChannelsKey, &*zero);
    }
    let subdevices = new_array(&[subdevice], "subdevice array")?;

    let tap = new_dictionary(2, "tap dictionary")?;
    let yes = NSNumber::new_bool(true);
    unsafe {
        set_dictionary_value(&tap, kAudioSubTapUIDKey, tap_uid);
        set_dictionary_value(&tap, kAudioSubTapDriftCompensationKey, &*yes);
    }
    let taps = new_array(&[tap], "tap array")?;

    let properties = new_dictionary(7, "aggregate dictionary")?;
    let aggregate_uid_cf = CFString::from_str(aggregate_uid);
    let aggregate_name_cf = CFString::from_str(aggregate_name);
    unsafe {
        set_dictionary_value(
            &properties,
            kAudioAggregateDeviceNameKey,
            &*aggregate_name_cf,
        );
        set_dictionary_value(&properties, kAudioAggregateDeviceUIDKey, &*aggregate_uid_cf);
        set_dictionary_value(
            &properties,
            kAudioAggregateDeviceSubDeviceListKey,
            &*subdevices,
        );
        set_dictionary_value(
            &properties,
            kAudioAggregateDeviceMainSubDeviceKey,
            &*output_uid_cf,
        );
        set_dictionary_value(&properties, kAudioAggregateDeviceTapListKey, &*taps);
        set_dictionary_value(&properties, kAudioAggregateDeviceTapAutoStartKey, &*yes);
        set_dictionary_value(&properties, kAudioAggregateDeviceIsPrivateKey, &*yes);
    }

    Ok(unsafe { CFRetained::cast_unchecked::<CFDictionary>(properties) })
}

fn new_dictionary(
    capacity: isize,
    kind: &'static str,
) -> Result<CFRetained<CFMutableDictionary>, SystemAudioCaptureError> {
    unsafe {
        CFMutableDictionary::new(
            kCFAllocatorDefault,
            capacity,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    }
    .ok_or(SystemAudioCaptureError::CoreFoundationAllocation { kind })
}

fn new_array<T: objc2_core_foundation::Type>(
    values: &[CFRetained<T>],
    kind: &'static str,
) -> Result<CFRetained<CFArray>, SystemAudioCaptureError> {
    unsafe {
        CFArray::new(
            kCFAllocatorDefault,
            values.as_ptr() as *mut *const c_void,
            values.len() as isize,
            &kCFTypeArrayCallBacks,
        )
    }
    .ok_or(SystemAudioCaptureError::CoreFoundationAllocation { kind })
}

/// Add a Core Foundation or toll-free-bridged Objective-C object to a CF dictionary.
///
/// # Safety
///
/// `value` must point to an object that supports `CFRetain` and `CFRelease`.
unsafe fn set_dictionary_value<T>(dictionary: &CFMutableDictionary, key: &'static CStr, value: &T) {
    let key = to_cfstring(key);
    unsafe {
        CFMutableDictionary::set_value(
            Some(dictionary),
            (key.as_ref() as *const CFString).cast(),
            (value as *const T).cast(),
        );
    }
}

fn to_cfstring(value: &'static CStr) -> CFRetained<CFString> {
    unsafe { CFString::with_c_string(kCFAllocatorDefault, value.as_ptr(), 0x0800_0100) }
        .expect("Core Audio dictionary keys are valid UTF-8")
}

fn check_core_audio_status(
    operation: &'static str,
    status: i32,
) -> Result<(), SystemAudioCaptureError> {
    if status == 0 {
        Ok(())
    } else if status == kAudioDevicePermissionsError {
        Err(SystemAudioCaptureError::PermissionDenied)
    } else {
        Err(SystemAudioCaptureError::CoreAudio { operation, status })
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
    #[error("could not identify the default system output device")]
    DefaultOutputIdentifier {
        #[source]
        source: cpal::Error,
    },
    #[error("sosus created Core Audio aggregate {uid}, but it was not enumerated in time")]
    AggregateNotEnumerated { uid: String },
    #[error("could not read the sosus system-audio input format")]
    DefaultInputConfig {
        #[source]
        source: cpal::Error,
    },
    #[error("Core Audio could not resolve the current sosus process for self-exclusion")]
    CurrentProcessUnavailable,
    #[error("Core Audio did not return an object for operation: {operation}")]
    CoreAudioObjectUnavailable { operation: &'static str },
    #[error("Core Foundation could not allocate the {kind}")]
    CoreFoundationAllocation { kind: &'static str },
    #[error("Core Audio operation '{operation}' failed with OSStatus {status}")]
    CoreAudio {
        operation: &'static str,
        status: i32,
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
        assert!(matches!(
            check_core_audio_status("create process tap", kAudioDevicePermissionsError),
            Err(SystemAudioCaptureError::PermissionDenied)
        ));
    }
}
