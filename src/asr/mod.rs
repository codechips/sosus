//! Backend-neutral transcription types and the single backend construction seam.

#![allow(dead_code)]

use std::{fmt, path::PathBuf, sync::Arc};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod decode;
mod parakeet;
mod vocab;
mod whisper;

pub(crate) use decode::SUPPORTED_EXTENSIONS;
pub(crate) use vocab::Vocabulary;

/// Samples shared by ASR and diarization after one decode and resample pass.
#[derive(Clone, Debug)]
pub struct Audio16kMono {
    samples: Arc<[f32]>,
}

impl Audio16kMono {
    pub const SAMPLE_RATE: u32 = 16_000;

    pub fn new(samples: Vec<f32>) -> Self {
        Self {
            samples: samples.into(),
        }
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn duration_seconds(&self) -> f64 {
        self.samples.len() as f64 / f64::from(Self::SAMPLE_RATE)
    }

    pub fn shares_samples_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.samples, &other.samples)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum TranscriptionBackend {
    #[default]
    Parakeet = 0,
    Whisper = 1,
}

impl TranscriptionBackend {
    pub const ALL: [Self; 2] = [Self::Parakeet, Self::Whisper];

    pub fn capabilities(self) -> &'static BackendCapabilities {
        BACKEND_CAPABILITIES[self as usize]
    }

    fn alternative(self) -> Self {
        match self {
            Self::Parakeet => Self::Whisper,
            Self::Whisper => Self::Parakeet,
        }
    }
}

impl fmt::Display for TranscriptionBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.capabilities().id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageSupport {
    Universal,
    Enumerated(&'static [&'static str]),
}

impl LanguageSupport {
    pub fn supports(self, language: &str) -> bool {
        match self {
            Self::Universal => true,
            Self::Enumerated(languages) => languages.contains(&language),
        }
    }

    pub fn codes(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Universal => None,
            Self::Enumerated(languages) => Some(languages),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordTimestamps {
    Native,
    OptIn { experimental: bool },
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VocabularyBiasing {
    ContextGraph { max_terms: Option<usize> },
    PromptPriming { max_prompt_tokens: usize },
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub id: &'static str,
    pub display_name: &'static str,
    pub languages: LanguageSupport,
    pub word_timestamps: WordTimestamps,
    pub vocabulary: VocabularyBiasing,
    pub emits_punctuation: bool,
}

pub const PARAKEET_LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
];

pub const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su",
];

pub const PARAKEET_CAPABILITIES: BackendCapabilities = BackendCapabilities {
    id: "parakeet",
    display_name: "Parakeet TDT 0.6B v3",
    languages: LanguageSupport::Enumerated(PARAKEET_LANGUAGES),
    word_timestamps: WordTimestamps::Native,
    vocabulary: VocabularyBiasing::ContextGraph { max_terms: None },
    emits_punctuation: true,
};

pub const WHISPER_CAPABILITIES: BackendCapabilities = BackendCapabilities {
    id: "whisper",
    display_name: "Whisper",
    // Enumerating the 99 model languages lets startup reject invalid ISO codes.
    languages: LanguageSupport::Enumerated(WHISPER_LANGUAGES),
    word_timestamps: WordTimestamps::OptIn { experimental: true },
    vocabulary: VocabularyBiasing::PromptPriming {
        max_prompt_tokens: 224,
    },
    emits_punctuation: true,
};

const BACKEND_CAPABILITIES: [&BackendCapabilities; 2] =
    [&PARAKEET_CAPABILITIES, &WHISPER_CAPABILITIES];

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LanguageValidationError {
    #[error(
        "expected an empty string for auto-detection or a supported lowercase ISO language code"
    )]
    InvalidCode,
    #[error("{message}")]
    Unsupported { message: String },
}

pub fn validate_language(
    backend: TranscriptionBackend,
    language: &str,
) -> Result<(), LanguageValidationError> {
    if language.is_empty() {
        return Ok(());
    }
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|byte| byte.is_ascii_lowercase())
    {
        return Err(LanguageValidationError::InvalidCode);
    }

    let capabilities = backend.capabilities();
    if capabilities.languages.supports(language) {
        return Ok(());
    }

    let alternative = backend.alternative();
    let suggestion = if alternative.capabilities().languages.supports(language) {
        format!(" Select the `{alternative}` backend for `{language}`.")
    } else {
        String::new()
    };
    let coverage = capabilities
        .languages
        .codes()
        .map_or_else(|| "all languages".to_owned(), |codes| codes.join(", "));
    Err(LanguageValidationError::Unsupported {
        message: format!(
            "`{language}` is not supported by {backend}; supported codes: {coverage}.{suggestion}"
        ),
    })
}

pub fn requests_backend_word_timestamps(
    capabilities: &BackendCapabilities,
    words_required: bool,
) -> Result<bool, AsrError> {
    match capabilities.word_timestamps {
        WordTimestamps::Native => Ok(true),
        WordTimestamps::OptIn { .. } => Ok(words_required),
        WordTimestamps::Unsupported if words_required => Err(AsrError::WordTimestampsUnsupported {
            backend: capabilities.id,
        }),
        WordTimestamps::Unsupported => Ok(false),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub score: f32,
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub words: Vec<Word>,
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptResult {
    pub language: String,
    pub duration_seconds: f64,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug)]
pub struct PrepareOptions {
    pub model_dir: PathBuf,
    pub threads: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VocabularyHint {
    pub text: String,
    pub weight: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TranscribeOptions {
    pub language: Option<String>,
    pub vocabulary: Vec<VocabularyHint>,
    pub words_required: bool,
}

pub trait ProgressSink: Sync {
    fn report(&self, fraction: f32);

    fn is_cancelled(&self) -> bool {
        false
    }
}

pub trait Transcriber: Send {
    fn capabilities(&self) -> &BackendCapabilities;
    fn prepare(&mut self, options: &PrepareOptions) -> Result<(), AsrError>;
    fn transcribe(
        &mut self,
        audio: &Audio16kMono,
        options: &TranscribeOptions,
        progress: &dyn ProgressSink,
    ) -> Result<TranscriptResult, AsrError>;
}

pub fn create_transcriber(backend: TranscriptionBackend) -> Box<dyn Transcriber> {
    match backend {
        TranscriptionBackend::Parakeet => Box::new(parakeet::ParakeetTranscriber::new()),
        TranscriptionBackend::Whisper => Box::new(whisper::WhisperTranscriber::new()),
    }
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("the {backend} backend is not implemented yet")]
    BackendNotImplemented { backend: &'static str },
    #[error("could not initialize the {backend} backend: {reason}")]
    BackendInitialization {
        backend: &'static str,
        reason: String,
    },
    #[error("the {backend} backend returned no recognition result")]
    MissingResult { backend: &'static str },
    #[error("the {backend} backend returned invalid native word timings: {reason}")]
    InvalidWordTimings {
        backend: &'static str,
        reason: String,
    },
    #[error("vocabulary biasing is unavailable for {backend}: {reason}")]
    VocabularyUnavailable {
        backend: &'static str,
        reason: String,
    },
    #[error("the {backend} backend cannot provide required word timestamps")]
    WordTimestampsUnsupported { backend: &'static str },
    #[error("transcription was cancelled")]
    Cancelled,
}

pub use decode::decode_audio_file;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_validation_is_capability_driven() {
        assert_eq!(PARAKEET_LANGUAGES.len(), 25);
        assert_eq!(WHISPER_LANGUAGES.len(), 99);
        validate_language(TranscriptionBackend::Parakeet, "sv").unwrap();
        validate_language(TranscriptionBackend::Whisper, "no").unwrap();

        let error = validate_language(TranscriptionBackend::Parakeet, "no").unwrap_err();
        assert!(error.to_string().contains("Select the `whisper` backend"));
        assert!(matches!(
            validate_language(TranscriptionBackend::Whisper, "zz"),
            Err(LanguageValidationError::Unsupported { .. })
        ));
    }

    #[test]
    fn timestamp_policy_uses_declared_capabilities() {
        assert!(requests_backend_word_timestamps(&PARAKEET_CAPABILITIES, false).unwrap());
        assert!(!requests_backend_word_timestamps(&WHISPER_CAPABILITIES, false).unwrap());
        assert!(requests_backend_word_timestamps(&WHISPER_CAPABILITIES, true).unwrap());
    }

    #[test]
    fn vocabulary_disclosure_is_capability_driven() {
        assert_eq!(
            PARAKEET_CAPABILITIES.vocabulary,
            VocabularyBiasing::ContextGraph { max_terms: None }
        );
        assert_eq!(
            WHISPER_CAPABILITIES.vocabulary,
            VocabularyBiasing::PromptPriming {
                max_prompt_tokens: 224
            }
        );
    }

    #[test]
    fn construction_is_closed_and_capabilities_survive_dynamic_dispatch() {
        for backend in TranscriptionBackend::ALL {
            let transcriber = create_transcriber(backend);
            assert_eq!(transcriber.capabilities(), backend.capabilities());
        }
    }

    #[test]
    fn decoded_audio_clones_share_one_buffer() {
        let audio = Audio16kMono::new(vec![0.0; Audio16kMono::SAMPLE_RATE as usize]);
        let diarization_input = audio.clone();

        assert!(audio.shares_samples_with(&diarization_input));
        assert_eq!(audio.duration_seconds(), 1.0);
    }
}
