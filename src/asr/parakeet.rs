//! NVIDIA Parakeet TDT transcription backend.

use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineRecognizerResult,
    OfflineTransducerModelConfig,
};

use super::{
    AsrError, Audio16kMono, BackendCapabilities, PARAKEET_CAPABILITIES, PrepareOptions,
    ProgressSink, Segment, TranscribeOptions, Transcriber, TranscriptResult, Word,
};

const ENCODER: &str = "encoder.int8.onnx";
const DECODER: &str = "decoder.int8.onnx";
const JOINER: &str = "joiner.int8.onnx";
const TOKENS: &str = "tokens.txt";

pub struct ParakeetTranscriber {
    recognizer: Option<OfflineRecognizer>,
}

impl ParakeetTranscriber {
    pub fn new() -> Self {
        Self { recognizer: None }
    }
}

impl Transcriber for ParakeetTranscriber {
    fn capabilities(&self) -> &BackendCapabilities {
        &PARAKEET_CAPABILITIES
    }

    fn prepare(&mut self, options: &PrepareOptions) -> Result<(), AsrError> {
        let threads =
            i32::try_from(options.threads).map_err(|_| AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: format!(
                    "thread count {} exceeds the native runtime limit",
                    options.threads
                ),
            })?;
        if threads < 1 {
            return Err(AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: "thread count must be at least 1".to_owned(),
            });
        }

        for filename in [ENCODER, DECODER, JOINER, TOKENS] {
            require_model_file(&options.model_dir, filename, self.capabilities().id)?;
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(path_text(&options.model_dir.join(ENCODER))),
            decoder: Some(path_text(&options.model_dir.join(DECODER))),
            joiner: Some(path_text(&options.model_dir.join(JOINER))),
        };
        config.model_config.tokens = Some(path_text(&options.model_dir.join(TOKENS)));
        config.model_config.provider = Some("cpu".to_owned());
        config.model_config.num_threads = threads;
        config.model_config.model_type = Some("nemo_transducer".to_owned());
        config.decoding_method = Some("greedy_search".to_owned());
        config.max_active_paths = 1;

        self.recognizer = OfflineRecognizer::create(&config);
        if self.recognizer.is_none() {
            return Err(AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: "sherpa-onnx rejected the pinned Parakeet model files".to_owned(),
            });
        }
        Ok(())
    }

    fn transcribe(
        &mut self,
        audio: &Audio16kMono,
        options: &TranscribeOptions,
        progress: &dyn ProgressSink,
    ) -> Result<TranscriptResult, AsrError> {
        if progress.is_cancelled() {
            return Err(AsrError::Cancelled);
        }
        if !options.vocabulary.is_empty() {
            return Err(AsrError::VocabularyUnavailable {
                backend: self.capabilities().id,
                reason: "the pinned upstream export does not publish bpe.vocab".to_owned(),
            });
        }
        let recognizer =
            self.recognizer
                .as_ref()
                .ok_or_else(|| AsrError::BackendInitialization {
                    backend: self.capabilities().id,
                    reason: "prepare() must complete before transcription".to_owned(),
                })?;
        let stream = recognizer.create_stream();
        stream.accept_waveform(Audio16kMono::SAMPLE_RATE as i32, audio.samples());

        decode_with_progress_ticks(recognizer, &stream, progress);
        if progress.is_cancelled() {
            return Err(AsrError::Cancelled);
        }
        let result = stream.get_result().ok_or(AsrError::MissingResult {
            backend: self.capabilities().id,
        })?;
        let words = native_words(&result, audio.duration_seconds(), self.capabilities().id)?;
        let segments = if result.text.trim().is_empty() {
            Vec::new()
        } else {
            let start_seconds = words.first().map_or(0.0, |word| word.start_seconds);
            let end_seconds = words
                .last()
                .map_or_else(|| audio.duration_seconds(), |word| word.end_seconds);
            vec![Segment {
                start_seconds,
                end_seconds,
                text: result.text,
                words,
            }]
        };
        progress.report(1.0);

        Ok(TranscriptResult {
            language: options.language.clone().unwrap_or_default(),
            duration_seconds: audio.duration_seconds(),
            segments,
        })
    }
}

fn require_model_file(
    model_dir: &Path,
    filename: &str,
    backend: &'static str,
) -> Result<(), AsrError> {
    let path = model_dir.join(filename);
    if path.is_file() {
        Ok(())
    } else {
        Err(AsrError::BackendInitialization {
            backend,
            reason: format!("required model file is missing: {}", path.display()),
        })
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn decode_with_progress_ticks(
    recognizer: &OfflineRecognizer,
    stream: &sherpa_onnx::OfflineStream,
    progress: &dyn ProgressSink,
) {
    let finished = AtomicBool::new(false);
    thread::scope(|scope| {
        let ticker = scope.spawn(|| {
            progress.report(0.0);
            while !finished.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_secs(1));
                if !finished.load(Ordering::Acquire) {
                    progress.report(0.0);
                }
            }
        });
        recognizer.decode(stream);
        finished.store(true, Ordering::Release);
        ticker.thread().unpark();
    });
}

fn native_words(
    result: &OfflineRecognizerResult,
    audio_duration: f64,
    backend: &'static str,
) -> Result<Vec<Word>, AsrError> {
    if result.tokens.is_empty() {
        return if result.text.trim().is_empty() {
            Ok(Vec::new())
        } else {
            Err(invalid_timings(
                backend,
                "non-empty text had no native tokens",
            ))
        };
    }
    let timestamps = result
        .timestamps
        .as_deref()
        .ok_or_else(|| invalid_timings(backend, "timestamps were absent"))?;
    if timestamps.len() != result.tokens.len() {
        return Err(invalid_timings(
            backend,
            format!(
                "{} tokens had {} timestamps",
                result.tokens.len(),
                timestamps.len()
            ),
        ));
    }
    if let Some(durations) = &result.durations
        && durations.len() != result.tokens.len()
    {
        return Err(invalid_timings(
            backend,
            format!(
                "{} tokens had {} durations",
                result.tokens.len(),
                durations.len()
            ),
        ));
    }

    let sentencepiece = result.tokens.iter().any(|token| token.contains('▁'));
    let mut words = Vec::new();
    let mut current: Option<Word> = None;
    for (index, token) in result.tokens.iter().enumerate() {
        let starts_word = !sentencepiece || token.starts_with('▁');
        let text = token.trim_start_matches('▁').replace('▁', " ");
        if text.is_empty() {
            continue;
        }
        let start = f64::from(timestamps[index]);
        let end = result
            .durations
            .as_ref()
            .map(|durations| start + f64::from(durations[index]))
            .or_else(|| timestamps.get(index + 1).map(|next| f64::from(*next)))
            .unwrap_or(audio_duration)
            .clamp(start, audio_duration);

        if starts_word {
            if let Some(word) = current.take() {
                words.push(word);
            }
            current = Some(Word {
                start_seconds: start,
                end_seconds: end,
                text,
                score: 0.0,
            });
        } else if let Some(word) = &mut current {
            word.text.push_str(&text);
            word.end_seconds = end;
        } else {
            current = Some(Word {
                start_seconds: start,
                end_seconds: end,
                text,
                score: 0.0,
            });
        }
    }
    if let Some(word) = current {
        words.push(word);
    }
    Ok(words)
}

fn invalid_timings(backend: &'static str, reason: impl Into<String>) -> AsrError {
    AsrError::InvalidWordTimings {
        backend,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_sentencepiece_tokens_into_native_words() {
        let result = OfflineRecognizerResult {
            text: "Hello, world.".to_owned(),
            tokens: vec![
                "▁Hello".to_owned(),
                ",".to_owned(),
                "▁world".to_owned(),
                ".".to_owned(),
            ],
            timestamps: Some(vec![0.1, 0.4, 0.6, 0.9]),
            durations: Some(vec![0.3, 0.1, 0.3, 0.1]),
        };

        let words = native_words(&result, 1.0, "parakeet").unwrap();

        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello,");
        assert_eq!(words[1].text, "world.");
        assert!((words[0].start_seconds - 0.1).abs() < 1e-5);
        assert!((words[1].end_seconds - 1.0).abs() < 1e-5);
    }

    #[test]
    fn missing_native_timestamps_is_an_error_not_silent_degradation() {
        let result = OfflineRecognizerResult {
            text: "Hello".to_owned(),
            tokens: vec!["Hello".to_owned()],
            timestamps: None,
            durations: None,
        };

        assert!(matches!(
            native_words(&result, 1.0, "parakeet"),
            Err(AsrError::InvalidWordTimings { .. })
        ));
    }
}
