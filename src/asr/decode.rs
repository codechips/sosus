//! Media decoding and one-pass 16 kHz mono resampling.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use rubato::{FftFixedIn, Resampler};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, DecoderOptions},
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};
use thiserror::Error;

use super::Audio16kMono;

pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["wav", "mp3", "m4a", "flac", "ogg", "mp4", "m4v", "mov"];
const RESAMPLE_CHUNK_FRAMES: usize = 1_024;
const RESAMPLE_SUB_CHUNKS: usize = 2;
const MAX_NORMALIZED_SECONDS: usize = 4 * 60 * 60;
const MAX_NORMALIZED_SAMPLES: usize = Audio16kMono::SAMPLE_RATE as usize * MAX_NORMALIZED_SECONDS;

pub fn decode_audio_file(path: &Path) -> Result<Audio16kMono, AudioDecodeError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| SUPPORTED_EXTENSIONS.contains(&extension.as_str()))
        .ok_or_else(|| AudioDecodeError::UnsupportedExtension {
            path: path.to_path_buf(),
            supported: SUPPORTED_EXTENSIONS.join(", "),
        })?;

    let file = File::open(path).map_err(|source| AudioDecodeError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(&extension);
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|source| AudioDecodeError::Probe {
            path: path.to_path_buf(),
            source,
        })?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioDecodeError::NoAudioTrack {
            path: path.to_path_buf(),
        })?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|source| AudioDecodeError::Decoder {
            path: path.to_path_buf(),
            source,
        })?;

    let mut source_rate = None;
    let mut output = Vec::new();
    let mut resampler = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(source) => {
                return Err(AudioDecodeError::Container {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .map_err(|source| AudioDecodeError::Decode {
                path: path.to_path_buf(),
                source,
            })?;
        let spec = *decoded.spec();
        if let Some(previous_rate) = source_rate {
            if previous_rate != spec.rate {
                return Err(AudioDecodeError::ChangingSampleRate {
                    path: path.to_path_buf(),
                    previous: previous_rate,
                    current: spec.rate,
                });
            }
        } else {
            source_rate = Some(spec.rate);
            if spec.rate != Audio16kMono::SAMPLE_RATE {
                resampler = Some(StreamingResampler::new(spec.rate)?);
            }
        }

        let channels = spec.channels.count();
        if channels == 0 {
            return Err(AudioDecodeError::NoChannels {
                path: path.to_path_buf(),
            });
        }
        let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        samples.copy_interleaved_ref(decoded);
        let mut mono = Vec::with_capacity(samples.samples().len() / channels);
        for frame in samples.samples().chunks_exact(channels) {
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
        if let Some(resampler) = &mut resampler {
            resampler.push(&mono, &mut output)?;
        } else {
            append_bounded(&mut output, &mono)?;
        }
    }

    let source_rate = source_rate.ok_or_else(|| AudioDecodeError::EmptyAudio {
        path: path.to_path_buf(),
    })?;
    if output.is_empty() && resampler.is_none() {
        return Err(AudioDecodeError::EmptyAudio {
            path: path.to_path_buf(),
        });
    }
    if let Some(resampler) = &mut resampler {
        resampler.finish(&mut output)?;
    }
    if output.is_empty() {
        return Err(AudioDecodeError::EmptyAudio {
            path: path.to_path_buf(),
        });
    }

    let _ = source_rate;
    Ok(Audio16kMono::new(output))
}

struct StreamingResampler {
    inner: FftFixedIn<f32>,
    pending: Vec<f32>,
    buffer: Vec<Vec<f32>>,
    delay: usize,
    source_rate: usize,
    input_frames: usize,
}

impl StreamingResampler {
    fn new(source_rate: u32) -> Result<Self, AudioDecodeError> {
        let inner = FftFixedIn::<f32>::new(
            source_rate as usize,
            Audio16kMono::SAMPLE_RATE as usize,
            RESAMPLE_CHUNK_FRAMES,
            RESAMPLE_SUB_CHUNKS,
            1,
        )
        .map_err(AudioDecodeError::ResamplerConstruction)?;
        let buffer = vec![vec![0.0; inner.output_frames_max()]];
        Ok(Self {
            delay: inner.output_delay(),
            inner,
            pending: Vec::new(),
            buffer,
            source_rate: source_rate as usize,
            input_frames: 0,
        })
    }

    fn push(&mut self, samples: &[f32], output: &mut Vec<f32>) -> Result<(), AudioDecodeError> {
        self.input_frames = self.input_frames.saturating_add(samples.len());
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= self.inner.input_frames_next() {
            let input = [self.pending.as_slice()];
            let (consumed, produced) = self
                .inner
                .process_into_buffer(&input, &mut self.buffer, None)
                .map_err(AudioDecodeError::Resample)?;
            append_bounded(output, &self.buffer[0][..produced])?;
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    fn finish(&mut self, output: &mut Vec<f32>) -> Result<(), AudioDecodeError> {
        let expected = self
            .input_frames
            .saturating_mul(Audio16kMono::SAMPLE_RATE as usize)
            .saturating_add(self.source_rate / 2)
            / self.source_rate;
        let end = self.delay.saturating_add(expected);
        while output.len() < end {
            let input = (!self.pending.is_empty()).then_some([self.pending.as_slice()]);
            let produced = self
                .inner
                .process_partial_into_buffer(
                    input.as_ref().map(|input| input.as_slice()),
                    &mut self.buffer,
                    None,
                )
                .map_err(AudioDecodeError::Resample)?
                .1;
            append_bounded(output, &self.buffer[0][..produced])?;
            self.pending.clear();
            if produced == 0 && output.len() < end {
                return Err(AudioDecodeError::IncompleteResample {
                    expected,
                    actual: output.len().saturating_sub(self.delay),
                });
            }
        }
        let discard = self.delay.min(output.len());
        output.drain(..discard);
        output.truncate(expected);
        Ok(())
    }
}

fn append_bounded(output: &mut Vec<f32>, samples: &[f32]) -> Result<(), AudioDecodeError> {
    let attempted = output.len().saturating_add(samples.len());
    if attempted > MAX_NORMALIZED_SAMPLES {
        return Err(AudioDecodeError::TooLong {
            maximum_seconds: MAX_NORMALIZED_SECONDS,
        });
    }
    output.extend_from_slice(samples);
    Ok(())
}

#[derive(Debug, Error)]
pub enum AudioDecodeError {
    #[error(
        "unsupported audio file {path}; supported extensions are: {supported}",
        path = .path.display()
    )]
    UnsupportedExtension { path: PathBuf, supported: String },
    #[error("could not open audio file {path}", path = .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not recognize the media container in {path}", path = .path.display())]
    Probe {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("media file {path} contains no supported audio track", path = .path.display())]
    NoAudioTrack { path: PathBuf },
    #[error("could not create an audio decoder for {path}", path = .path.display())]
    Decoder {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("could not read the media container in {path}", path = .path.display())]
    Container {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("corrupt or unsupported audio data in {path}", path = .path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("audio sample rate changed in {path} from {previous} Hz to {current} Hz", path = .path.display())]
    ChangingSampleRate {
        path: PathBuf,
        previous: u32,
        current: u32,
    },
    #[error("audio track in {path} has no channels", path = .path.display())]
    NoChannels { path: PathBuf },
    #[error("audio file {path} contains no decodable samples", path = .path.display())]
    EmptyAudio { path: PathBuf },
    #[error("could not construct the audio resampler")]
    ResamplerConstruction(#[source] rubato::ResamplerConstructionError),
    #[error("audio resampling failed")]
    Resample(#[source] rubato::ResampleError),
    #[error("audio exceeds the {maximum_seconds}-second decoding limit")]
    TooLong { maximum_seconds: usize },
    #[error("audio resampling produced {actual} frames; expected {expected}")]
    IncompleteResample { expected: usize, actual: usize },
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    struct TempAudio(PathBuf);

    impl TempAudio {
        fn with_extension(extension: &str) -> Self {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            Self(env::temp_dir().join(format!(
                "sosus-decode-test-{}-{sequence}.{extension}",
                std::process::id()
            )))
        }
    }

    impl Drop for TempAudio {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn write_wav(path: &Path, sample_rate: u32, frames: &[[i16; 2]]) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 2,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in frames {
            writer.write_sample(frame[0]).unwrap();
            writer.write_sample(frame[1]).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn decodes_downmixes_and_resamples_wav_once() {
        let file = TempAudio::with_extension("wav");
        let frames = vec![[16_384, 16_384]; 48_000];
        write_wav(&file.0, 48_000, &frames);

        let audio = decode_audio_file(&file.0).unwrap();

        assert_eq!(audio.samples().len(), 16_000);
        assert_eq!(audio.duration_seconds(), 1.0);
        assert!(audio.samples().iter().all(|sample| sample.is_finite()));
        assert!(
            audio.samples()[1_000..15_000]
                .iter()
                .all(|sample| (*sample - 0.5).abs() < 0.01)
        );
    }

    #[test]
    fn downmixes_stereo_before_resampling() {
        let file = TempAudio::with_extension("wav");
        let frames = vec![[i16::MAX, -i16::MAX]; 1_600];
        write_wav(&file.0, 16_000, &frames);

        let audio = decode_audio_file(&file.0).unwrap();

        assert_eq!(audio.samples().len(), 1_600);
        assert!(audio.samples().iter().all(|sample| sample.abs() < 0.000_1));
    }

    #[test]
    fn rejects_unsupported_extensions_before_opening() {
        let file = TempAudio::with_extension("aiff");
        assert!(matches!(
            decode_audio_file(&file.0),
            Err(AudioDecodeError::UnsupportedExtension { .. })
        ));
    }

    #[test]
    fn corrupt_supported_file_returns_typed_error() {
        let file = TempAudio::with_extension("wav");
        fs::write(&file.0, b"not a wav").unwrap();

        assert!(matches!(
            decode_audio_file(&file.0),
            Err(AudioDecodeError::Probe { .. })
                | Err(AudioDecodeError::Container { .. })
                | Err(AudioDecodeError::Decode { .. })
        ));
    }
}
