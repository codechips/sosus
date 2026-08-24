//! Lock-free stream health reported by CPAL callbacks and consumed by the recorder.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use cpal::ErrorKind;

const NO_FAILURE: u8 = 0;

/// A CPAL error that requires the current capture stream to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamFailure {
    DeviceBusy,
    DeviceNotAvailable,
    HostUnavailable,
    InvalidInput,
    PermissionDenied,
    ResourceExhausted,
    StreamInvalidated,
    UnsupportedConfig,
    UnsupportedOperation,
    BackendError,
    Other,
}

impl StreamFailure {
    fn code(self) -> u8 {
        match self {
            Self::DeviceBusy => 1,
            Self::DeviceNotAvailable => 2,
            Self::HostUnavailable => 3,
            Self::InvalidInput => 4,
            Self::PermissionDenied => 5,
            Self::ResourceExhausted => 6,
            Self::StreamInvalidated => 7,
            Self::UnsupportedConfig => 8,
            Self::UnsupportedOperation => 9,
            Self::BackendError => 10,
            Self::Other => 11,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::DeviceBusy,
            2 => Self::DeviceNotAvailable,
            3 => Self::HostUnavailable,
            4 => Self::InvalidInput,
            5 => Self::PermissionDenied,
            6 => Self::ResourceExhausted,
            7 => Self::StreamInvalidated,
            8 => Self::UnsupportedConfig,
            9 => Self::UnsupportedOperation,
            10 => Self::BackendError,
            11 => Self::Other,
            _ => return None,
        })
    }

    pub(crate) fn category(self) -> &'static str {
        match self {
            Self::DeviceBusy => "device_busy",
            Self::DeviceNotAvailable => "device_not_available",
            Self::HostUnavailable => "host_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::ResourceExhausted => "resource_exhausted",
            Self::StreamInvalidated => "stream_invalidated",
            Self::UnsupportedConfig => "unsupported_config",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::BackendError => "backend_error",
            Self::Other => "other",
        }
    }

    pub(crate) fn recovery_hint(self) -> &'static str {
        match self {
            Self::DeviceNotAvailable | Self::StreamInvalidated => {
                "check the output device, then start a new recording"
            }
            Self::PermissionDenied => {
                "check Screen & System Audio Recording permission, then retry"
            }
            Self::DeviceBusy => "close the app using the device, then start a new recording",
            Self::HostUnavailable | Self::BackendError | Self::Other => {
                "restart Sosus; if it repeats, include the Sosus log when reporting it"
            }
            Self::ResourceExhausted => "free system resources, then start a new recording",
            Self::InvalidInput | Self::UnsupportedConfig | Self::UnsupportedOperation => {
                "restart Sosus after checking the selected audio device"
            }
        }
    }
}

/// Non-fatal conditions observed while a stream remains usable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamEvents {
    pub(crate) device_changes: u64,
    pub(crate) xruns: u64,
    pub(crate) realtime_denied: u64,
}

impl StreamEvents {
    pub(crate) fn is_empty(self) -> bool {
        self.device_changes == 0 && self.xruns == 0 && self.realtime_denied == 0
    }
}

/// Shared callback-to-recorder state. Callback reporting does not allocate or block.
#[derive(Debug, Default)]
pub(crate) struct StreamHealth {
    fatal_failure: AtomicU8,
    device_changes: AtomicU64,
    xruns: AtomicU64,
    realtime_denied: AtomicU64,
}

impl StreamHealth {
    pub(crate) fn report(&self, kind: ErrorKind) {
        match kind {
            // CPAL documents these as notifications where the stream remains usable.
            ErrorKind::DeviceChanged => {
                self.device_changes.fetch_add(1, Ordering::Relaxed);
            }
            ErrorKind::Xrun => {
                self.xruns.fetch_add(1, Ordering::Relaxed);
            }
            ErrorKind::RealtimeDenied => {
                self.realtime_denied.fetch_add(1, Ordering::Relaxed);
            }
            ErrorKind::DeviceBusy => self.set_failure(StreamFailure::DeviceBusy),
            ErrorKind::DeviceNotAvailable => self.set_failure(StreamFailure::DeviceNotAvailable),
            ErrorKind::HostUnavailable => self.set_failure(StreamFailure::HostUnavailable),
            ErrorKind::InvalidInput => self.set_failure(StreamFailure::InvalidInput),
            ErrorKind::PermissionDenied => self.set_failure(StreamFailure::PermissionDenied),
            ErrorKind::ResourceExhausted => self.set_failure(StreamFailure::ResourceExhausted),
            ErrorKind::StreamInvalidated => self.set_failure(StreamFailure::StreamInvalidated),
            ErrorKind::UnsupportedConfig => self.set_failure(StreamFailure::UnsupportedConfig),
            ErrorKind::UnsupportedOperation => {
                self.set_failure(StreamFailure::UnsupportedOperation)
            }
            ErrorKind::BackendError => self.set_failure(StreamFailure::BackendError),
            ErrorKind::Other => self.set_failure(StreamFailure::Other),
            _ => self.set_failure(StreamFailure::Other),
        }
    }

    pub(crate) fn failure(&self) -> Option<StreamFailure> {
        StreamFailure::from_code(self.fatal_failure.load(Ordering::Acquire))
    }

    pub(crate) fn take_events(&self) -> StreamEvents {
        StreamEvents {
            device_changes: self.device_changes.swap(0, Ordering::Relaxed),
            xruns: self.xruns.swap(0, Ordering::Relaxed),
            realtime_denied: self.realtime_denied.swap(0, Ordering::Relaxed),
        }
    }

    fn set_failure(&self, failure: StreamFailure) {
        let _ = self.fatal_failure.compare_exchange(
            NO_FAILURE,
            failure.code(),
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoverable_events_do_not_fail_the_stream() {
        let health = StreamHealth::default();
        health.report(ErrorKind::DeviceChanged);
        health.report(ErrorKind::Xrun);
        health.report(ErrorKind::RealtimeDenied);

        assert_eq!(health.failure(), None);
        assert_eq!(
            health.take_events(),
            StreamEvents {
                device_changes: 1,
                xruns: 1,
                realtime_denied: 1,
            }
        );
        assert!(health.take_events().is_empty());
    }

    #[test]
    fn first_fatal_error_is_retained() {
        let health = StreamHealth::default();
        health.report(ErrorKind::DeviceNotAvailable);
        health.report(ErrorKind::PermissionDenied);

        assert_eq!(health.failure(), Some(StreamFailure::DeviceNotAvailable));
    }
}
