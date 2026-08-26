//! Native macOS audio preview playback.

use std::path::Path;

use objc2::AnyThread;
use objc2_avf_audio::AVAudioPlayer;
use objc2_foundation::{NSString, NSURL};
use thiserror::Error;

/// A small AVFoundation wrapper for archive playback in the TUI thread.
pub struct PreviewPlayer {
    player: objc2::rc::Retained<AVAudioPlayer>,
    duration_seconds: f64,
}

impl PreviewPlayer {
    pub fn open(path: &Path) -> Result<Self, PreviewError> {
        if !path.is_file() {
            return Err(PreviewError::MissingRecording(path.to_path_buf()));
        }
        let path_text = NSString::from_str(&path.to_string_lossy());
        let url = NSURL::fileURLWithPath(&path_text);
        // SAFETY: The NSURL references a local file and AVAudioPlayer retains it
        // while it is used exclusively on the TUI thread.
        let player =
            unsafe { AVAudioPlayer::initWithContentsOfURL_error(AVAudioPlayer::alloc(), &url) }
                .map_err(|error| PreviewError::Open {
                    path: path.to_path_buf(),
                    reason: error.localizedDescription().to_string(),
                })?;
        // SAFETY: AVAudioPlayer is a valid initialized player on this thread.
        unsafe {
            player.prepareToPlay();
        }
        Ok(Self {
            // SAFETY: duration is available after successful initialization.
            duration_seconds: unsafe { player.duration() }.max(0.0),
            player,
        })
    }

    pub fn play(&self) -> Result<(), PreviewError> {
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        if unsafe { self.player.play() } {
            Ok(())
        } else {
            Err(PreviewError::PlaybackRefused)
        }
    }

    pub fn pause(&self) {
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        unsafe { self.player.pause() };
    }

    pub fn stop(&self) {
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        unsafe {
            self.player.stop();
            self.player.setCurrentTime(0.0);
        }
    }

    pub fn seek(&self, seconds: f64) -> f64 {
        let position = seconds.clamp(0.0, self.duration_seconds);
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        unsafe { self.player.setCurrentTime(position) };
        position
    }

    pub fn position_seconds(&self) -> f64 {
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        unsafe { self.player.currentTime() }.clamp(0.0, self.duration_seconds)
    }

    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    pub fn is_playing(&self) -> bool {
        // SAFETY: AVAudioPlayer is retained by self and only used on the TUI thread.
        unsafe { self.player.isPlaying() }
    }
}

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("recording was not found: {0}")]
    MissingRecording(std::path::PathBuf),
    #[error("could not open {path} for playback: {reason}")]
    Open {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("macOS refused to start audio playback")]
    PlaybackRefused,
}
