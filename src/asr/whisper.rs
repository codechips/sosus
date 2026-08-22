//! whisper.cpp transcription backend.

use super::{
    AsrError, Audio16kMono, BackendCapabilities, PrepareOptions, ProgressSink, TranscribeOptions,
    Transcriber, TranscriptResult, WHISPER_CAPABILITIES,
};

pub struct WhisperTranscriber;

impl WhisperTranscriber {
    pub fn new() -> Self {
        Self
    }
}

impl Transcriber for WhisperTranscriber {
    fn capabilities(&self) -> &BackendCapabilities {
        &WHISPER_CAPABILITIES
    }

    fn prepare(&mut self, _options: &PrepareOptions) -> Result<(), AsrError> {
        Err(AsrError::BackendNotImplemented {
            backend: self.capabilities().id,
        })
    }

    fn transcribe(
        &mut self,
        _audio: &Audio16kMono,
        _options: &TranscribeOptions,
        _progress: &dyn ProgressSink,
    ) -> Result<TranscriptResult, AsrError> {
        Err(AsrError::BackendNotImplemented {
            backend: self.capabilities().id,
        })
    }
}
