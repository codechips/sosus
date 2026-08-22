//! Core system-plus-microphone recording coordinator.

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use thiserror::Error;
use time::OffsetDateTime;

use super::{
    mic::{MicrophoneCapture, MicrophoneCaptureError, MicrophoneReader},
    tap::{SystemAudioCapture, SystemAudioCaptureError, SystemAudioReader},
    wav::{RECORDING_SAMPLE_RATE, RecordingWavError, RecordingWavSink},
};
use crate::paths::AppPaths;

const SOURCE_BUFFER_FRAMES: usize = 8_192;
const DEFAULT_GAIN: f32 = 0.707_945_76; // -3 dB

/// A live core recording. Call [`pump`](Self::pump) regularly from a non-real-time task.
pub struct RecordingSession {
    system_capture: SystemAudioCapture,
    system_reader: SystemAudioReader,
    microphone_capture: MicrophoneCapture,
    microphone_reader: MicrophoneReader,
    sink: RecordingWavSink,
    system_converter: RateConverter,
    microphone_converter: RateConverter,
    system_ready: VecDeque<f32>,
    microphone_ready: VecDeque<f32>,
    system_input: Vec<f32>,
    microphone_input: Vec<f32>,
    system_output: Vec<f32>,
    microphone_output: Vec<f32>,
    mix: Vec<f32>,
    started: Instant,
    system_dropouts: u64,
    microphone_dropouts: u64,
    microphone_failed: bool,
    system_peak: f32,
    microphone_peak: f32,
}

impl RecordingSession {
    /// Reserve a meeting folder and start recording, removing the reservation if capture cannot
    /// be initialized. This prevents failed starts from appearing as empty recordings.
    pub fn start_new_meeting(
        app_paths: &AppPaths,
        started_at: OffsetDateTime,
    ) -> anyhow::Result<(PathBuf, Self)> {
        let meeting_dir = app_paths.create_meeting_dir(started_at)?;
        let recording_path = meeting_dir.join("recording.wav");
        match Self::start(&recording_path) {
            Ok(session) => Ok((meeting_dir, session)),
            Err(error) => {
                if let Err(cleanup_error) = remove_failed_meeting_dir(&meeting_dir) {
                    return Err(error).with_context(|| {
                        format!(
                            "could not initialize recording; failed to remove {}: {cleanup_error}",
                            meeting_dir.display()
                        )
                    });
                }
                Err(error)
                    .context("could not initialize recording; removed the empty meeting folder")
            }
        }
    }

    /// Start both required sources and create a new canonical WAV sink.
    pub fn start(path: impl AsRef<Path>) -> Result<Self, RecordingError> {
        let (system_capture, system_reader) = SystemAudioCapture::start_default()?;
        let (microphone_capture, microphone_reader) = MicrophoneCapture::start_default()?;
        let sink = RecordingWavSink::create(path)?;
        let system_converter = RateConverter::new(system_capture.sample_rate());
        let microphone_converter = RateConverter::new(microphone_capture.sample_rate());

        Ok(Self {
            system_capture,
            system_reader,
            microphone_capture,
            microphone_reader,
            sink,
            system_converter,
            microphone_converter,
            system_ready: VecDeque::with_capacity(RECORDING_SAMPLE_RATE as usize),
            microphone_ready: VecDeque::with_capacity(RECORDING_SAMPLE_RATE as usize),
            system_input: vec![0.0; SOURCE_BUFFER_FRAMES],
            microphone_input: vec![0.0; SOURCE_BUFFER_FRAMES],
            system_output: Vec::with_capacity(SOURCE_BUFFER_FRAMES * 2),
            microphone_output: Vec::with_capacity(SOURCE_BUFFER_FRAMES * 2),
            mix: Vec::with_capacity(SOURCE_BUFFER_FRAMES * 2),
            started: Instant::now(),
            system_dropouts: 0,
            microphone_dropouts: 0,
            microphone_failed: false,
            system_peak: 0.0,
            microphone_peak: 0.0,
        })
    }

    /// Drain queued callback data, mix it, and append it to the WAV.
    pub fn pump(&mut self) -> Result<(), RecordingError> {
        self.system_peak *= 0.82;
        self.microphone_peak *= 0.82;
        if self.system_reader.stream_failed() {
            return Err(RecordingError::SystemStreamFailed);
        }
        self.microphone_failed |= self.microphone_reader.stream_failed();

        self.drain_microphone();
        self.drain_system();
        self.write_due_samples()
    }

    /// Stop the streams, drain their final queued buffers, and finalize the WAV.
    pub fn finish(mut self) -> Result<RecordingOutcome, RecordingError> {
        self.drain_microphone();
        self.drain_system();
        self.write_due_samples()?;

        let samples_written = self.sink.samples_written();
        let path = self.sink.path().to_path_buf();
        self.sink.finish()?;

        Ok(RecordingOutcome {
            path,
            duration_seconds: samples_written as f64 / f64::from(RECORDING_SAMPLE_RATE),
            elapsed_seconds: self.started.elapsed().as_secs_f64(),
            system_dropouts: self.system_dropouts,
            microphone_dropouts: self.microphone_dropouts,
            microphone_failed: self.microphone_failed,
        })
    }

    pub fn source_formats(&self) -> SourceFormats {
        SourceFormats {
            system_sample_rate: self.system_capture.sample_rate(),
            system_channels: self.system_capture.channels(),
            microphone_sample_rate: self.microphone_capture.sample_rate(),
            microphone_channels: self.microphone_capture.channels(),
        }
    }

    pub fn elapsed_seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn input_levels(&self) -> (f32, f32) {
        (self.system_peak, self.microphone_peak)
    }

    fn drain_microphone(&mut self) {
        let dropped = self.microphone_reader.take_dropped_frames();
        self.microphone_dropouts = self.microphone_dropouts.saturating_add(dropped);
        if dropped > 0 {
            self.microphone_converter
                .process_silence(dropped, &mut self.microphone_output);
            self.microphone_ready
                .extend(self.microphone_output.iter().copied());
        }

        loop {
            let count = self
                .microphone_reader
                .read_available(&mut self.microphone_input);
            if count == 0 {
                break;
            }
            self.microphone_converter
                .process(&self.microphone_input[..count], &mut self.microphone_output);
            self.microphone_peak = self
                .microphone_peak
                .max(peak(&self.microphone_input[..count]));
            self.microphone_ready
                .extend(self.microphone_output.iter().copied());
        }
    }

    fn drain_system(&mut self) {
        let dropped = self.system_reader.take_dropped_frames();
        self.system_dropouts = self.system_dropouts.saturating_add(dropped);
        if dropped > 0 {
            self.system_converter
                .process_silence(dropped, &mut self.system_output);
            self.system_ready.extend(self.system_output.iter().copied());
        }

        loop {
            let count = self.system_reader.read_available(&mut self.system_input);
            if count == 0 {
                break;
            }
            self.system_converter
                .process(&self.system_input[..count], &mut self.system_output);
            self.system_peak = self.system_peak.max(peak(&self.system_input[..count]));
            self.system_ready.extend(self.system_output.iter().copied());
        }
    }

    fn write_due_samples(&mut self) -> Result<(), RecordingError> {
        let due = samples_due(
            self.started.elapsed().as_secs_f64(),
            self.sink.samples_written(),
        );
        self.mix.clear();
        self.mix.reserve(due);
        for _ in 0..due {
            let system = self.system_ready.pop_front().unwrap_or(0.0);
            let microphone = self.microphone_ready.pop_front().unwrap_or(0.0);
            self.mix
                .push((system * DEFAULT_GAIN + microphone * DEFAULT_GAIN).clamp(-1.0, 1.0));
        }
        self.sink.write_samples(&self.mix)?;
        Ok(())
    }
}

fn remove_failed_meeting_dir(path: &Path) -> std::io::Result<()> {
    fs::remove_dir_all(path)
}

fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0, f32::max)
        .min(1.0)
}

fn samples_due(elapsed_seconds: f64, samples_written: u64) -> usize {
    let target_samples = (elapsed_seconds * f64::from(RECORDING_SAMPLE_RATE)) as u64;
    target_samples.saturating_sub(samples_written) as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFormats {
    pub system_sample_rate: u32,
    pub system_channels: u16,
    pub microphone_sample_rate: u32,
    pub microphone_channels: u16,
}

#[derive(Debug)]
pub struct RecordingOutcome {
    pub path: std::path::PathBuf,
    pub duration_seconds: f64,
    pub elapsed_seconds: f64,
    pub system_dropouts: u64,
    pub microphone_dropouts: u64,
    pub microphone_failed: bool,
}

/// A streaming zero-order-hold converter with exact long-term frame counts.
///
/// The higher-quality adaptive conversion required for long recordings belongs
/// to recording hardening; this keeps arbitrary device rates usable in the core slice.
struct RateConverter {
    input_rate: u32,
    input_frames: u64,
    output_frames: u64,
    previous: f32,
}

impl RateConverter {
    fn new(input_rate: u32) -> Self {
        Self {
            input_rate,
            input_frames: 0,
            output_frames: 0,
            previous: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        output.clear();
        if input.is_empty() {
            return;
        }

        let chunk_start = self.input_frames;
        self.input_frames = self.input_frames.saturating_add(input.len() as u64);
        let target_outputs = self
            .input_frames
            .saturating_mul(u64::from(RECORDING_SAMPLE_RATE))
            / u64::from(self.input_rate);

        while self.output_frames < target_outputs {
            let source_frame = self
                .output_frames
                .saturating_mul(u64::from(self.input_rate))
                / u64::from(RECORDING_SAMPLE_RATE);
            let sample = if source_frame < chunk_start {
                self.previous
            } else {
                let index = (source_frame - chunk_start) as usize;
                input[index.min(input.len() - 1)]
            };
            output.push(sample);
            self.output_frames += 1;
        }
        self.previous = *input.last().unwrap_or(&self.previous);
    }

    fn process_silence(&mut self, frames: u64, output: &mut Vec<f32>) {
        output.clear();
        self.input_frames = self.input_frames.saturating_add(frames);
        let target_outputs = self
            .input_frames
            .saturating_mul(u64::from(RECORDING_SAMPLE_RATE))
            / u64::from(self.input_rate);
        output.resize((target_outputs - self.output_frames) as usize, 0.0);
        self.output_frames = target_outputs;
        self.previous = 0.0;
    }
}

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error(transparent)]
    SystemCapture(#[from] SystemAudioCaptureError),
    #[error(transparent)]
    MicrophoneCapture(#[from] MicrophoneCaptureError),
    #[error(transparent)]
    Wav(#[from] RecordingWavError),
    #[error("system-audio capture failed while recording")]
    SystemStreamFailed,
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    #[test]
    fn failed_start_cleanup_removes_the_entire_meeting_reservation() {
        let path = env::temp_dir().join(format!(
            "sosus-failed-recording-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("recording.wav"), b"partial recording").unwrap();

        remove_failed_meeting_dir(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn rate_conversion_has_exact_long_term_length() {
        let mut converter = RateConverter::new(44_100);
        let mut output = Vec::new();
        let input = vec![0.25; 441];
        let mut total = 0;

        for _ in 0..100 {
            converter.process(&input, &mut output);
            total += output.len();
            assert!(output.iter().all(|sample| *sample == 0.25));
        }

        assert_eq!(total, 48_000);
    }

    #[test]
    fn dropout_silence_preserves_converted_timeline() {
        let mut converter = RateConverter::new(96_000);
        let mut output = Vec::new();
        converter.process_silence(960, &mut output);

        assert_eq!(output, vec![0.0; 480]);
    }

    #[test]
    fn wall_clock_drives_the_recording_timeline() {
        assert_eq!(samples_due(0.01, 0), 480);
        assert_eq!(samples_due(1.0, 47_520), 480);
        assert_eq!(samples_due(1.0, 48_000), 0);
    }

    #[test]
    fn peak_meter_uses_absolute_clamped_signal() {
        assert_eq!(peak(&[-0.25, 0.6, -1.2]), 1.0);
        assert_eq!(peak(&[]), 0.0);
    }
}
