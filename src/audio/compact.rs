//! Safe post-processing compaction of completed WAV recordings to AAC/M4A.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use thiserror::Error;

const AFCONVERT: &str = "/usr/bin/afconvert";
const AFINFO: &str = "/usr/bin/afinfo";
const AAC_BIT_RATE: &str = "64000";

/// Convert a completed owned WAV recording to M4A atomically, then remove the WAV.
///
/// The original is retained unless both encoding and container verification succeed.
pub fn compact_wav_to_m4a(wav_path: &Path) -> Result<PathBuf, CompactRecordingError> {
    let meeting_dir = wav_path
        .parent()
        .ok_or_else(|| CompactRecordingError::InvalidInput {
            path: wav_path.to_owned(),
        })?;
    let destination = meeting_dir.join("recording.m4a");
    let temporary = meeting_dir.join(format!(".recording.{}.m4a.partial", std::process::id()));
    let _ = fs::remove_file(&temporary);

    let output = Command::new(AFCONVERT)
        .args(["-f", "m4af", "-d", "aac", "-b", AAC_BIT_RATE])
        .arg(wav_path)
        .arg(&temporary)
        .output()
        .map_err(|source| CompactRecordingError::Encode {
            path: wav_path.to_owned(),
            source,
        })?;
    if !output.status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(CompactRecordingError::EncoderFailed {
            path: wav_path.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let verification = Command::new(AFINFO)
        .arg(&temporary)
        .output()
        .map_err(|source| CompactRecordingError::Verify {
            path: temporary.clone(),
            source,
        })?;
    if !verification.status.success()
        || fs::metadata(&temporary).map_or(true, |metadata| metadata.len() == 0)
    {
        let _ = fs::remove_file(&temporary);
        return Err(CompactRecordingError::VerificationFailed { path: temporary });
    }

    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|source| {
        CompactRecordingError::Finalize {
            path: temporary.clone(),
            source,
        }
    })?;
    fs::rename(&temporary, &destination).map_err(|source| CompactRecordingError::Finalize {
        path: destination.clone(),
        source,
    })?;
    fs::remove_file(wav_path).map_err(|source| CompactRecordingError::RemoveSource {
        path: wav_path.to_owned(),
        source,
    })?;
    Ok(destination)
}

#[derive(Debug, Error)]
pub enum CompactRecordingError {
    #[error("recording path has no parent directory: {path}")]
    InvalidInput { path: PathBuf },
    #[error("could not start the macOS AAC encoder for {path}")]
    Encode {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("macOS AAC encoder failed for {path}: {detail}")]
    EncoderFailed { path: PathBuf, detail: String },
    #[error("could not verify the M4A recording at {path}")]
    Verify {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("M4A verification failed for {path}")]
    VerificationFailed { path: PathBuf },
    #[error("could not finalize the M4A recording at {path}")]
    Finalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove the original WAV recording at {path}")]
    RemoveSource {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    #[test]
    fn compaction_verifies_m4a_before_removing_the_wav() {
        let directory = env::temp_dir().join(format!(
            "sosus-m4a-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let wav = directory.join("recording.wav");
        let mut writer = hound::WavWriter::create(
            &wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..48_000 {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();

        let m4a = compact_wav_to_m4a(&wav).unwrap();

        assert!(!wav.exists());
        assert!(m4a.is_file());
        assert!(fs::metadata(&m4a).unwrap().len() > 0);
        assert!(
            Command::new(AFINFO)
                .arg(&m4a)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(crate::asr::decode_audio_file(&m4a).is_ok());
        fs::remove_dir_all(directory).unwrap();
    }
}
