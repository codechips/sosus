//! whisper.cpp transcription backend.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{
    AsrError, Audio16kMono, BackendCapabilities, PrepareOptions, ProgressSink, Segment,
    TranscribeOptions, Transcriber, TranscriptResult, WHISPER_CAPABILITIES,
};

static INSTALL_LOGGING_HOOKS: Once = Once::new();
// whisper.cpp is designed to receive successive context-sized windows. Passing a
// whole meeting can return a successful call with no segments at all.
const MAX_CHUNK_SECONDS: usize = 30;
const MAX_CHUNK_SAMPLES: usize = MAX_CHUNK_SECONDS * Audio16kMono::SAMPLE_RATE as usize;

pub struct WhisperTranscriber {
    context: Option<WhisperContext>,
    threads: i32,
    model_id: String,
}

impl WhisperTranscriber {
    pub fn new() -> Self {
        Self {
            context: None,
            threads: 1,
            model_id: String::new(),
        }
    }
}

impl Transcriber for WhisperTranscriber {
    fn capabilities(&self) -> &BackendCapabilities {
        &WHISPER_CAPABILITIES
    }

    fn prepare(&mut self, options: &PrepareOptions) -> Result<(), AsrError> {
        self.threads =
            i32::try_from(options.threads).map_err(|_| AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: format!(
                    "thread count {} exceeds the native runtime limit",
                    options.threads
                ),
            })?;
        if self.threads < 1 {
            return Err(AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: "thread count must be at least 1".to_owned(),
            });
        }

        let path = model_file(&options.model_dir)?;
        self.model_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_owned();
        INSTALL_LOGGING_HOOKS.call_once(whisper_rs::install_logging_hooks);
        self.context = Some(create_context(&path, self.capabilities().id)?);
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
        let segments = {
            let context = self
                .context
                .as_ref()
                .ok_or_else(|| AsrError::BackendInitialization {
                    backend: self.capabilities().id,
                    reason: "prepare() must complete before transcription".to_owned(),
                })?;
            self.transcribe_chunks(context, audio, options, progress)?
        };

        if segments.is_empty() && has_audible_signal(audio) {
            tracing::warn!(
                event = "asr_empty_result",
                backend = self.capabilities().id,
                model_id = self.model_id,
                duration_ms = (audio.duration_seconds() * 1_000.0) as u64,
                status = "failed"
            );
            return Err(AsrError::MissingResult {
                backend: self.capabilities().id,
            });
        }
        tracing::info!(
            event = "asr_completed",
            backend = self.capabilities().id,
            model_id = self.model_id,
            duration_ms = (audio.duration_seconds() * 1_000.0) as u64,
            count = segments.len(),
            status = "completed"
        );
        progress.report(1.0);
        Ok(TranscriptResult {
            language: options.language.clone().unwrap_or_default(),
            duration_seconds: audio.duration_seconds(),
            provenance: Default::default(),
            segments,
        })
    }
}

impl WhisperTranscriber {
    fn transcribe_chunks(
        &self,
        context: &WhisperContext,
        audio: &Audio16kMono,
        options: &TranscribeOptions,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<Segment>, AsrError> {
        progress.report(0.0);
        let mut segments = Vec::new();
        let chunks = chunk_ranges(audio.samples().len());
        tracing::info!(
            event = "asr_started",
            backend = self.capabilities().id,
            model_id = self.model_id,
            duration_ms = (audio.duration_seconds() * 1_000.0) as u64,
            count = chunks.len(),
            status = "running"
        );
        for (chunk_index, range) in chunks.iter().enumerate() {
            if progress.is_cancelled() {
                return Err(AsrError::Cancelled);
            }
            let mut state =
                context
                    .create_state()
                    .map_err(|error| AsrError::BackendInitialization {
                        backend: self.capabilities().id,
                        reason: error.to_string(),
                    })?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(self.threads);
            params.set_language(options.language.as_deref());
            params.set_detect_language(options.language.is_none());
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            state
                .full(params, &audio.samples()[range.clone()])
                .map_err(|error| AsrError::BackendInitialization {
                    backend: self.capabilities().id,
                    reason: error.to_string(),
                })?;
            let offset_seconds = range.start as f64 / f64::from(Audio16kMono::SAMPLE_RATE);
            for index in 0..state.full_n_segments() {
                let segment =
                    state
                        .get_segment(index)
                        .ok_or_else(|| AsrError::BackendInitialization {
                            backend: self.capabilities().id,
                            reason: format!(
                                "segment {index} disappeared from Whisper chunk {}",
                                chunk_index + 1
                            ),
                        })?;
                let text = segment
                    .to_str()
                    .map_err(|error| AsrError::BackendInitialization {
                        backend: self.capabilities().id,
                        reason: error.to_string(),
                    })?
                    .trim()
                    .to_owned();
                if text.is_empty() {
                    continue;
                }
                let start_seconds = offset_seconds + segment.start_timestamp() as f64 / 100.0;
                let end_seconds = offset_seconds + segment.end_timestamp() as f64 / 100.0;
                segments.push(Segment {
                    start_seconds,
                    end_seconds: end_seconds.max(start_seconds),
                    text,
                    words: Vec::new(),
                    speaker: None,
                });
            }
            progress.report((chunk_index + 1) as f32 / chunks.len() as f32);
        }
        Ok(segments)
    }
}

fn create_context(path: &Path, backend: &'static str) -> Result<WhisperContext, AsrError> {
    let mut parameters = WhisperContextParameters::new();
    parameters.use_gpu(true);
    WhisperContext::new_with_params(path.to_string_lossy().as_ref(), parameters).map_err(|error| {
        AsrError::BackendInitialization {
            backend,
            reason: error.to_string(),
        }
    })
}

fn has_audible_signal(audio: &Audio16kMono) -> bool {
    audio.samples().iter().any(|sample| sample.abs() >= 0.01)
}

fn chunk_ranges(sample_count: usize) -> Vec<std::ops::Range<usize>> {
    (0..sample_count)
        .step_by(MAX_CHUNK_SAMPLES)
        .map(|start| start..(start + MAX_CHUNK_SAMPLES).min(sample_count))
        .collect()
}

fn model_file(model_dir: &Path) -> Result<PathBuf, AsrError> {
    let candidates = fs::read_dir(model_dir)
        .map_err(|error| AsrError::BackendInitialization {
            backend: WHISPER_CAPABILITIES.id,
            reason: format!(
                "could not read model directory {}: {error}",
                model_dir.display()
            ),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("bin" | "ggml" | "gguf")
            )
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        Ok(candidates.into_iter().next().expect("one candidate"))
    } else {
        Err(AsrError::BackendInitialization {
            backend: WHISPER_CAPABILITIES.id,
            reason: format!(
                "expected exactly one Whisper GGML/GGUF model file in {}; found {}",
                model_dir.display(),
                candidates.len()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    #[test]
    fn bounds_long_audio_to_context_sized_chunks() {
        let ranges = chunk_ranges(MAX_CHUNK_SAMPLES * 2 + 7);
        assert_eq!(
            ranges,
            vec![
                0..MAX_CHUNK_SAMPLES,
                MAX_CHUNK_SAMPLES..MAX_CHUNK_SAMPLES * 2,
                MAX_CHUNK_SAMPLES * 2..MAX_CHUNK_SAMPLES * 2 + 7
            ]
        );
    }

    #[test]
    fn distinguishes_audible_audio_from_silence() {
        assert!(!has_audible_signal(&Audio16kMono::new(vec![0.009; 16])));
        assert!(has_audible_signal(&Audio16kMono::new(vec![0.01; 16])));
    }

    #[test]
    fn prepare_requires_the_pinned_model_file() {
        let directory = env::temp_dir().join(format!(
            "sosus-whisper-missing-model-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();

        let error = WhisperTranscriber::new()
            .prepare(&PrepareOptions {
                model_dir: directory.clone(),
                threads: 1,
            })
            .unwrap_err();

        assert!(error.to_string().contains("exactly one Whisper"));
        fs::remove_dir_all(directory).unwrap();
    }
}
