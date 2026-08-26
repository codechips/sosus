//! Recording, mixing, and WAV writing.

mod compact;
mod echo;
mod health;
mod level;
mod mic;
mod permission;
mod preview;
mod recording;
mod tap;
mod wav;

pub(crate) use compact::compact_wav_to_m4a;
pub(crate) use permission::ensure_capture_permissions;
pub(crate) use preview::{PreviewError, PreviewPlayer};
pub(crate) use recording::{MixSettings, RecordingSession};
