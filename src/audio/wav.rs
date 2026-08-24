//! Durable mono PCM output for owned recordings.

use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
};

use hound::{SampleFormat, WavSpec, WavWriter};
use thiserror::Error;

use crate::paths;

pub const RECORDING_SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const CHECKPOINT_INTERVAL_SAMPLES: u64 = RECORDING_SAMPLE_RATE as u64;

type RecordingWriter = WavWriter<BufWriter<File>>;

/// Incremental writer for the canonical owned `recording.wav` artifact.
pub struct RecordingWavSink {
    path: PathBuf,
    writer: RecordingWriter,
    samples_written: u64,
    samples_since_checkpoint: u64,
}

impl RecordingWavSink {
    /// Create a new private mono 48 kHz signed 16-bit PCM WAV.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, RecordingWavError> {
        let path = path.as_ref().to_path_buf();
        let file =
            paths::create_private_file(&path).map_err(|source| RecordingWavError::Create {
                path: path.clone(),
                source,
            })?;
        let writer = WavWriter::new(BufWriter::new(file), recording_spec()).map_err(|source| {
            RecordingWavError::Write {
                path: path.clone(),
                source,
            }
        })?;

        Ok(Self {
            path,
            writer,
            samples_written: 0,
            samples_since_checkpoint: 0,
        })
    }

    /// Reopen a finalized Sosus WAV and append to its existing audio.
    pub fn append(path: impl AsRef<Path>) -> Result<Self, RecordingWavError> {
        let path = path.as_ref().to_path_buf();
        let writer = WavWriter::append(&path).map_err(|source| RecordingWavError::Append {
            path: path.clone(),
            source,
        })?;
        if writer.spec() != recording_spec() {
            return Err(RecordingWavError::UnexpectedFormat { path });
        }
        Ok(Self {
            samples_written: u64::from(writer.duration()),
            path,
            writer,
            samples_since_checkpoint: 0,
        })
    }

    /// Append normalized mono samples and checkpoint the RIFF header every second.
    pub fn write_samples(&mut self, samples: &[f32]) -> Result<(), RecordingWavError> {
        for &sample in samples {
            self.writer
                .write_sample(float_to_pcm(sample))
                .map_err(|source| self.write_error(source))?;
            self.samples_written += 1;
            self.samples_since_checkpoint += 1;

            if self.samples_since_checkpoint >= CHECKPOINT_INTERVAL_SAMPLES {
                self.checkpoint()?;
            }
        }
        Ok(())
    }

    /// Flush buffered audio and update the WAV lengths without closing the recording.
    pub fn checkpoint(&mut self) -> Result<(), RecordingWavError> {
        self.writer
            .flush()
            .map_err(|source| self.write_error(source))?;
        self.samples_since_checkpoint = 0;
        Ok(())
    }

    /// Finalize the header and close the WAV cleanly.
    pub fn finish(self) -> Result<(), RecordingWavError> {
        let Self {
            path,
            writer,
            samples_written: _,
            samples_since_checkpoint: _,
        } = self;
        writer
            .finalize()
            .map_err(|source| RecordingWavError::Write { path, source })
    }

    /// Append silence without allocating in proportion to the interruption length.
    pub fn write_silence(&mut self, samples: u64) -> Result<(), RecordingWavError> {
        const SILENCE_CHUNK_SAMPLES: usize = 8_192;
        let silence = [0.0; SILENCE_CHUNK_SAMPLES];
        let mut remaining = samples;
        while remaining > 0 {
            let count = remaining.min(SILENCE_CHUNK_SAMPLES as u64) as usize;
            self.write_samples(&silence[..count])?;
            remaining -= count as u64;
        }
        Ok(())
    }

    pub fn samples_written(&self) -> u64 {
        self.samples_written
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_error(&self, source: hound::Error) -> RecordingWavError {
        RecordingWavError::Write {
            path: self.path.clone(),
            source,
        }
    }
}

fn recording_spec() -> WavSpec {
    WavSpec {
        channels: CHANNELS,
        sample_rate: RECORDING_SAMPLE_RATE,
        bits_per_sample: BITS_PER_SAMPLE,
        sample_format: SampleFormat::Int,
    }
}

fn float_to_pcm(sample: f32) -> i16 {
    let sample = sample.clamp(-1.0, 1.0);
    let scale = if sample < 0.0 { 32_768.0 } else { 32_767.0 };
    (sample * scale).round() as i16
}

#[derive(Debug, Error)]
pub enum RecordingWavError {
    #[error("could not create private recording WAV at {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: paths::PathError,
    },
    #[error("could not write recording WAV at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
    #[error("could not append to recording WAV at {path}")]
    Append {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
    #[error("cannot continue recording because {path} is not a Sosus mono 48 kHz PCM WAV")]
    UnexpectedFormat { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                env::temp_dir().join(format!("sosus-wav-test-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn writes_exact_recording_format_and_clamps_samples() {
        let temp = TempDir::new();
        let path = temp.0.join("meeting/recording.wav");
        let mut sink = RecordingWavSink::create(&path).unwrap();
        sink.write_samples(&[-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0])
            .unwrap();
        assert_eq!(sink.samples_written(), 7);
        assert_eq!(sink.path(), path);
        sink.finish().unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, CHANNELS);
        assert_eq!(spec.sample_rate, RECORDING_SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, BITS_PER_SAMPLE);
        assert_eq!(spec.sample_format, SampleFormat::Int);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [i16::MIN, i16::MIN, -16_384, 0, 16_384, i16::MAX, i16::MAX]
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn checkpoint_makes_a_full_second_readable_before_finalization() {
        let temp = TempDir::new();
        let path = temp.0.join("recording.wav");
        let mut sink = RecordingWavSink::create(&path).unwrap();
        sink.write_samples(&vec![0.25; RECORDING_SAMPLE_RATE as usize])
            .unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), RECORDING_SAMPLE_RATE);
        drop(reader);

        sink.write_samples(&[0.5]).unwrap();
        sink.finish().unwrap();
        assert_eq!(
            hound::WavReader::open(&path).unwrap().duration(),
            RECORDING_SAMPLE_RATE + 1
        );
    }

    #[test]
    fn refuses_to_replace_an_existing_recording() {
        let temp = TempDir::new();
        let path = temp.0.join("recording.wav");
        fs::write(&path, b"existing").unwrap();

        assert!(matches!(
            RecordingWavSink::create(&path),
            Err(RecordingWavError::Create { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"existing");
    }

    #[test]
    fn appends_silence_to_a_finalized_sosus_wav() {
        let temp = TempDir::new();
        let path = temp.0.join("recording.wav");
        let mut sink = RecordingWavSink::create(&path).unwrap();
        sink.write_samples(&[0.5; 4]).unwrap();
        sink.finish().unwrap();

        let mut sink = RecordingWavSink::append(&path).unwrap();
        assert_eq!(sink.samples_written(), 4);
        sink.write_silence(3).unwrap();
        sink.finish().unwrap();

        let samples = hound::WavReader::open(path)
            .unwrap()
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples.len(), 7);
        assert_eq!(&samples[4..], &[0, 0, 0]);
    }
}
