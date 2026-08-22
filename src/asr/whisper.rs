//! whisper.cpp transcription backend.

use std::{path::Path, sync::Once};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{
    AsrError, Audio16kMono, BackendCapabilities, PrepareOptions, ProgressSink, Segment,
    TranscribeOptions, Transcriber, TranscriptResult, WHISPER_CAPABILITIES,
};

const MODEL_FILENAME: &str = "ggml-base.bin";
static INSTALL_LOGGING_HOOKS: Once = Once::new();

pub struct WhisperTranscriber {
    context: Option<WhisperContext>,
    threads: i32,
}

impl WhisperTranscriber {
    pub fn new() -> Self {
        Self {
            context: None,
            threads: 1,
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

        let path = options.model_dir.join(MODEL_FILENAME);
        require_model_file(&path, self.capabilities().id)?;
        INSTALL_LOGGING_HOOKS.call_once(whisper_rs::install_logging_hooks);
        let mut parameters = WhisperContextParameters::new();
        parameters.use_gpu(true);
        self.context = Some(
            WhisperContext::new_with_params(path.to_string_lossy().as_ref(), parameters).map_err(
                |error| AsrError::BackendInitialization {
                    backend: self.capabilities().id,
                    reason: error.to_string(),
                },
            )?,
        );
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
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: "prepare() must complete before transcription".to_owned(),
            })?;
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

        progress.report(0.0);
        state
            .full(params, audio.samples())
            .map_err(|error| AsrError::BackendInitialization {
                backend: self.capabilities().id,
                reason: error.to_string(),
            })?;
        if progress.is_cancelled() {
            return Err(AsrError::Cancelled);
        }
        let mut segments = Vec::new();
        let count = state.full_n_segments();
        for index in 0..count {
            let segment =
                state
                    .get_segment(index)
                    .ok_or_else(|| AsrError::BackendInitialization {
                        backend: self.capabilities().id,
                        reason: format!("segment {index} disappeared from the native result"),
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
            let start_seconds = segment.start_timestamp() as f64 / 100.0;
            let end_seconds = segment.end_timestamp() as f64 / 100.0;
            segments.push(Segment {
                start_seconds,
                end_seconds: end_seconds.max(start_seconds),
                text,
                words: Vec::new(),
                speaker: None,
            });
        }
        progress.report(1.0);
        Ok(TranscriptResult {
            language: options.language.clone().unwrap_or_default(),
            duration_seconds: audio.duration_seconds(),
            segments,
        })
    }
}

fn require_model_file(path: &Path, backend: &'static str) -> Result<(), AsrError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(AsrError::BackendInitialization {
            backend,
            reason: format!("required model file is missing: {}", path.display()),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

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

        assert!(error.to_string().contains(MODEL_FILENAME));
        fs::remove_dir_all(directory).unwrap();
    }
}
