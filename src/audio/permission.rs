//! System-audio and microphone permission checks.

use block2::RcBlock;
use objc2_avf_audio::{
    AVAudioApplication, AVAudioApplicationRecordPermission as RawMicrophonePermission,
};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MicrophonePermission {
    Undetermined,
    Denied,
    Granted,
    Unknown(isize),
}

/// Ensure the permission with an explicit status API is granted.
///
/// Core Audio exposes no separate AudioCapture preflight call. Creating the
/// process tap is its authoritative permission check, handled by the system
/// capture source with an actionable error.
pub async fn ensure_capture_permissions() -> Result<(), PermissionError> {
    ensure_microphone_permission().await
}

async fn ensure_microphone_permission() -> Result<(), PermissionError> {
    match microphone_permission() {
        MicrophonePermission::Granted => Ok(()),
        MicrophonePermission::Denied => Err(PermissionError::MicrophoneDenied),
        MicrophonePermission::Unknown(raw) => Err(PermissionError::UnknownMicrophoneStatus(raw)),
        MicrophonePermission::Undetermined => request_microphone_permission().await,
    }
}

fn microphone_permission() -> MicrophonePermission {
    // SAFETY: AVAudioApplication and recordPermission are available from macOS 14.0;
    // sosus requires macOS 14.4 or newer.
    let application = unsafe { AVAudioApplication::sharedInstance() };
    let raw = unsafe { application.recordPermission() };
    map_microphone_permission(raw)
}

fn map_microphone_permission(raw: RawMicrophonePermission) -> MicrophonePermission {
    if raw == RawMicrophonePermission::Undetermined {
        MicrophonePermission::Undetermined
    } else if raw == RawMicrophonePermission::Denied {
        MicrophonePermission::Denied
    } else if raw == RawMicrophonePermission::Granted {
        MicrophonePermission::Granted
    } else {
        MicrophonePermission::Unknown(raw.0)
    }
}

async fn request_microphone_permission() -> Result<(), PermissionError> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let completion = RcBlock::new(move |granted| {
        let _ = sender.send(bool::from(granted));
    });

    // SAFETY: The heap-backed block matches Apple's escaping completion-handler
    // signature and only performs a non-blocking channel send.
    unsafe {
        AVAudioApplication::requestRecordPermissionWithCompletionHandler(&completion);
    }

    match receiver.recv().await {
        Some(true) => Ok(()),
        Some(false) => Err(PermissionError::MicrophoneDenied),
        None => Err(PermissionError::MicrophoneRequestFailed),
    }
}

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error(
        "microphone recording is not allowed; enable sosus in System Settings > Privacy & Security > Microphone, then run the command again"
    )]
    MicrophoneDenied,
    #[error("macOS returned an unknown microphone permission status ({0})")]
    UnknownMicrophoneStatus(isize),
    #[error("macOS did not complete the microphone permission request")]
    MicrophoneRequestFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_documented_microphone_permission_and_fails_closed() {
        assert_eq!(
            map_microphone_permission(RawMicrophonePermission::Undetermined),
            MicrophonePermission::Undetermined
        );
        assert_eq!(
            map_microphone_permission(RawMicrophonePermission::Denied),
            MicrophonePermission::Denied
        );
        assert_eq!(
            map_microphone_permission(RawMicrophonePermission::Granted),
            MicrophonePermission::Granted
        );
        assert_eq!(
            map_microphone_permission(RawMicrophonePermission(-1)),
            MicrophonePermission::Unknown(-1)
        );
    }
}
