//! NVIDIA Parakeet TDT transcription backend.

use super::{
    AsrError, Audio16kMono, BackendCapabilities, PARAKEET_CAPABILITIES, PrepareOptions,
    ProgressSink, TranscribeOptions, Transcriber, TranscriptResult,
};

pub struct ParakeetTranscriber;

impl ParakeetTranscriber {
    pub fn new() -> Self {
        Self
    }
}

impl Transcriber for ParakeetTranscriber {
    fn capabilities(&self) -> &BackendCapabilities {
        &PARAKEET_CAPABILITIES
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
