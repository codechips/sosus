//! Typed configuration loading, validation, and invocation-scoped overrides.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table, value};

pub use crate::asr::TranscriptionBackend;
#[cfg(test)]
use crate::asr::{PARAKEET_LANGUAGES, WHISPER_LANGUAGES};

const BUILT_IN_TEMPLATES: &[&str] = &["meeting", "lecture", "brief"];
static CONFIG_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub audio: AudioConfig,
    pub transcription: TranscriptionConfig,
    pub vocabulary: VocabularyConfig,
    pub diarization: DiarizationConfig,
    pub summarization: SummarizationConfig,
    pub search: SearchConfig,
    pub output: OutputConfig,
    pub templates: BTreeMap<String, TemplateConfig>,
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_f64_range(
            "audio.system_gain_db",
            self.audio.system_gain_db,
            -24.0,
            12.0,
        )?;
        validate_f64_range("audio.mic_gain_db", self.audio.mic_gain_db, -24.0, 12.0)?;
        validate_f64_range(
            "audio.silence_threshold_dbfs",
            self.audio.silence_threshold_dbfs,
            -90.0,
            -20.0,
        )?;

        if self.audio.capture_mode == CaptureMode::Processes && self.audio.processes.is_empty() {
            return Err(invalid(
                "audio.processes",
                "must contain at least one PID when audio.capture_mode is `processes`",
            ));
        }
        if self.audio.processes.contains(&0) {
            return Err(invalid("audio.processes", "PIDs must be greater than zero"));
        }

        if self.transcription.backend == TranscriptionBackend::Parakeet
            && !self.transcription.model.is_empty()
        {
            return Err(invalid(
                "transcription.model",
                "must be empty for the `parakeet` backend; model selection is Whisper-only",
            ));
        }
        validate_language(
            self.transcription.backend,
            self.transcription.language.as_str(),
        )?;

        if !self.vocabulary.hotword_score.is_finite() || self.vocabulary.hotword_score <= 0.0 {
            return Err(invalid(
                "vocabulary.hotword_score",
                "must be a finite number greater than zero",
            ));
        }
        for (index, entry) in self.vocabulary.terms.iter().enumerate() {
            let key = format!("vocabulary.terms[{index}]");
            let term = entry.term();
            if term.trim().is_empty() {
                return Err(invalid(key, "term must not be empty"));
            }
            if term.contains(['\n', '\r']) {
                return Err(invalid(key, "term must be a single line"));
            }
            if let Some(weight) = entry.weight()
                && (!weight.is_finite() || weight <= 0.0)
            {
                return Err(invalid(
                    format!("vocabulary.terms[{index}].weight"),
                    "must be a finite number greater than zero",
                ));
            }
        }

        let min = self.diarization.min_speakers;
        let max = self.diarization.max_speakers;
        if min != 0 && max != 0 && min > max {
            return Err(invalid(
                "diarization.min_speakers",
                format!("must not exceed diarization.max_speakers ({max})"),
            ));
        }

        if self.search.top_k == 0 {
            return Err(invalid("search.top_k", "must be at least 1"));
        }
        if self.search.rrf_k == 0 {
            return Err(invalid("search.rrf_k", "must be at least 1"));
        }
        if self.output.dir.as_os_str().is_empty() {
            return Err(invalid("output.dir", "must not be empty"));
        }

        for (name, template) in &self.templates {
            if name.trim().is_empty() {
                return Err(invalid("templates", "template names must not be empty"));
            }
            template.validate(name)?;
        }
        if !BUILT_IN_TEMPLATES.contains(&self.summarization.template.as_str())
            && !self.templates.contains_key(&self.summarization.template)
        {
            return Err(invalid(
                "summarization.template",
                format!(
                    "unknown template `{}`; expected meeting, lecture, brief, or a key under [templates]",
                    self.summarization.template
                ),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub capture_mode: CaptureMode,
    pub processes: Vec<u32>,
    pub mic: bool,
    pub mic_device: String,
    pub system_gain_db: f64,
    pub mic_gain_db: f64,
    pub silence_timeout: u64,
    pub silence_threshold_dbfs: f64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            capture_mode: CaptureMode::All,
            processes: Vec::new(),
            mic: true,
            mic_device: String::new(),
            system_gain_db: -3.0,
            mic_gain_db: -3.0,
            silence_timeout: 300,
            silence_threshold_dbfs: -50.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    #[default]
    All,
    Processes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscriptionConfig {
    pub backend: TranscriptionBackend,
    pub model: String,
    pub language: String,
    pub threads: usize,
    pub initial_prompt: String,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            backend: TranscriptionBackend::Parakeet,
            model: String::new(),
            language: String::new(),
            threads: 0,
            initial_prompt: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VocabularyConfig {
    pub enabled: bool,
    pub file: PathBuf,
    pub hotword_score: f64,
    pub terms: Vec<VocabularyTerm>,
}

impl Default for VocabularyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file: PathBuf::new(),
            hotword_score: 1.5,
            terms: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VocabularyTerm {
    Simple(String),
    Detailed(WeightedVocabularyTerm),
}

impl VocabularyTerm {
    #[must_use]
    pub fn term(&self) -> &str {
        match self {
            Self::Simple(term) => term,
            Self::Detailed(term) => &term.term,
        }
    }

    #[must_use]
    pub fn weight(&self) -> Option<f64> {
        match self {
            Self::Simple(_) => None,
            Self::Detailed(term) => term.weight,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeightedVocabularyTerm {
    pub term: String,
    pub weight: Option<f64>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiarizationConfig {
    pub enabled: bool,
    pub min_speakers: usize,
    pub max_speakers: usize,
}

impl Default for DiarizationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_speakers: 2,
            max_speakers: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SummarizationConfig {
    pub enabled: bool,
    pub model: String,
    pub template: String,
    pub context_size: usize,
}

impl Default for SummarizationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "phi-4-mini".to_owned(),
            template: "meeting".to_owned(),
            context_size: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub top_k: usize,
    pub rrf_k: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            top_k: 12,
            rrf_k: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub dir: PathBuf,
    pub json: bool,
    pub keep_recording: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("~/sosus/recordings"),
            json: false,
            keep_recording: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateConfig {
    pub system_prompt: String,
    pub prompt: String,
}

impl TemplateConfig {
    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        validate_template_text(
            &format!("templates.{name}.system_prompt"),
            &self.system_prompt,
            false,
        )?;
        let transcript_count =
            validate_template_text(&format!("templates.{name}.prompt"), &self.prompt, true)?;
        if transcript_count == 0 {
            return Err(invalid(
                format!("templates.{name}.prompt"),
                "must contain the `{transcript}` placeholder",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    UnknownKey { key: String },
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey { key } => write!(formatter, "unknown configuration key `{key}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<ConfigWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocations {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
}

impl ConfigLocations {
    #[must_use]
    pub fn resolve(
        default_config_path: impl Into<PathBuf>,
        default_data_dir: impl Into<PathBuf>,
        environment: &EnvironmentOverrides,
        invocation: &ConfigOverrides,
    ) -> Self {
        Self {
            config_path: invocation
                .config_path
                .clone()
                .or_else(|| environment.config_path.clone())
                .unwrap_or_else(|| default_config_path.into()),
            data_dir: invocation
                .data_dir
                .clone()
                .or_else(|| environment.data_dir.clone())
                .unwrap_or_else(|| default_data_dir.into()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvironmentOverrides {
    pub config_path: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

impl EnvironmentOverrides {
    #[must_use]
    pub fn from_process() -> Self {
        Self::from_lookup(|key| std::env::var_os(key))
    }

    #[must_use]
    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<OsString>,
    {
        Self {
            config_path: lookup("SOSUS_CONFIG").map(PathBuf::from),
            data_dir: lookup("SOSUS_DATA_DIR").map(PathBuf::from),
        }
    }
}

/// Invocation-only settings. Applying these never writes the config file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigOverrides {
    pub config_path: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub capture_mode: Option<CaptureMode>,
    pub processes: Option<Vec<u32>>,
    pub mic: Option<bool>,
    pub mic_device: Option<String>,
    pub system_gain_db: Option<f64>,
    pub mic_gain_db: Option<f64>,
    pub silence_timeout: Option<u64>,
    pub silence_threshold_dbfs: Option<f64>,
    pub backend: Option<TranscriptionBackend>,
    pub asr_model: Option<String>,
    pub language: Option<String>,
    pub threads: Option<usize>,
    pub vocabulary_enabled: Option<bool>,
    pub vocabulary_file: Option<PathBuf>,
    pub vocabulary_terms: Vec<VocabularyTerm>,
    pub diarization_enabled: Option<bool>,
    pub min_speakers: Option<usize>,
    pub max_speakers: Option<usize>,
    pub summarization_enabled: Option<bool>,
    pub summary_template: Option<String>,
    pub llm_model: Option<String>,
    pub output_json: Option<bool>,
    pub keep_recording: Option<bool>,
}

impl ConfigOverrides {
    pub fn apply_to(&self, config: &mut Config) -> Result<(), ConfigError> {
        if self.threads == Some(0) {
            return Err(invalid("--threads", "must be at least 1"));
        }
        let mut candidate = config.clone();
        if let Some(value) = &self.output_dir {
            candidate.output.dir.clone_from(value);
        }
        if let Some(value) = self.capture_mode {
            candidate.audio.capture_mode = value;
        }
        if let Some(value) = &self.processes {
            candidate.audio.processes.clone_from(value);
        }
        if let Some(value) = self.mic {
            candidate.audio.mic = value;
        }
        if let Some(value) = &self.mic_device {
            candidate.audio.mic_device.clone_from(value);
        }
        if let Some(value) = self.system_gain_db {
            candidate.audio.system_gain_db = value;
        }
        if let Some(value) = self.mic_gain_db {
            candidate.audio.mic_gain_db = value;
        }
        if let Some(value) = self.silence_timeout {
            candidate.audio.silence_timeout = value;
        }
        if let Some(value) = self.silence_threshold_dbfs {
            candidate.audio.silence_threshold_dbfs = value;
        }
        if let Some(value) = self.backend {
            candidate.transcription.backend = value;
        }
        if let Some(value) = &self.asr_model {
            candidate.transcription.model.clone_from(value);
        }
        if let Some(value) = &self.language {
            candidate.transcription.language.clone_from(value);
        }
        if let Some(value) = self.threads {
            candidate.transcription.threads = value;
        }
        if let Some(value) = self.vocabulary_enabled {
            candidate.vocabulary.enabled = value;
        }
        if let Some(value) = &self.vocabulary_file {
            candidate.vocabulary.file.clone_from(value);
        }
        candidate
            .vocabulary
            .terms
            .extend(self.vocabulary_terms.iter().cloned());
        if let Some(value) = self.diarization_enabled {
            candidate.diarization.enabled = value;
        }
        if let Some(value) = self.min_speakers {
            candidate.diarization.min_speakers = value;
        }
        if let Some(value) = self.max_speakers {
            candidate.diarization.max_speakers = value;
        }
        if let Some(value) = self.summarization_enabled {
            candidate.summarization.enabled = value;
        }
        if let Some(value) = &self.summary_template {
            candidate.summarization.template.clone_from(value);
        }
        if let Some(value) = &self.llm_model {
            candidate.summarization.model.clone_from(value);
        }
        if let Some(value) = self.output_json {
            candidate.output.json = value;
        }
        if let Some(value) = self.keep_recording {
            candidate.output.keep_recording = value;
        }
        candidate.validate()?;
        *config = candidate;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub saved: Config,
    pub effective: Config,
    pub warnings: Vec<ConfigWarning>,
    pub locations: ConfigLocations,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid configuration key `{key}`: {message}")]
    Invalid { key: String, message: String },
    #[error("settings changed on disk; close and reopen Settings before saving")]
    Changed,
    #[error("could not write config `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A fingerprint of the config file when a settings session was opened.
///
/// Keeping this in the UI lets us refuse to overwrite a file changed by another
/// process while the dialog was open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFingerprint(Option<[u8; 32]>);

pub fn fingerprint(path: &Path) -> Result<ConfigFingerprint, ConfigError> {
    match fs::read(path) {
        Ok(contents) => Ok(ConfigFingerprint(Some(Sha256::digest(contents).into()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ConfigFingerprint(None)),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Persist the small set of settings exposed in the TUI without rewriting
/// comments, whitespace, unknown keys, or the rest of the configuration.
pub fn save_tui_settings(
    path: &Path,
    expected: &ConfigFingerprint,
    config: &Config,
) -> Result<ConfigFingerprint, ConfigError> {
    config.validate()?;
    let input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let current = ConfigFingerprint(Some(Sha256::digest(input.as_bytes()).into()));
    let current = if input.is_empty() && !path.exists() {
        ConfigFingerprint(None)
    } else {
        current
    };
    if &current != expected {
        return Err(ConfigError::Changed);
    }

    let mut document = input
        .parse::<DocumentMut>()
        .map_err(|error| invalid("<document>", error.to_string()))?;
    let audio = table_mut(&mut document, "audio");
    audio["mic"] = value(config.audio.mic);
    audio["system_gain_db"] = value(config.audio.system_gain_db);
    audio["mic_gain_db"] = value(config.audio.mic_gain_db);

    let transcription = table_mut(&mut document, "transcription");
    transcription["backend"] = value(config.transcription.backend.to_string());
    transcription["language"] = value(config.transcription.language.clone());
    if config.transcription.backend == TranscriptionBackend::Parakeet
        || config.transcription.model.is_empty()
    {
        transcription.remove("model");
    } else {
        transcription["model"] = value(config.transcription.model.clone());
    }

    let diarization = table_mut(&mut document, "diarization");
    diarization["enabled"] = value(config.diarization.enabled);
    diarization["min_speakers"] = value(config.diarization.min_speakers as i64);
    diarization["max_speakers"] = value(config.diarization.max_speakers as i64);
    let output = table_mut(&mut document, "output");
    output["json"] = value(config.output.json);

    write_private_atomic(path, document.to_string().as_bytes())?;
    fingerprint(path)
}

fn table_mut<'a>(document: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    if !document.as_table().contains_key(name) {
        document[name] = Item::Table(Table::new());
    }
    document[name]
        .as_table_mut()
        .expect("configuration section should be a table")
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("config", "has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;
    ensure_safe_config_parent(parent).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;
    let temporary = temporary_config_path(parent, path).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        use std::io::Write;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Write {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn ensure_safe_config_parent(parent: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::other("configuration parent is not a directory"));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configuration parent must not be writable by group or other users",
        ));
    }
    Ok(())
}

fn temporary_config_path(parent: &Path, path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    for _ in 0..128 {
        let sequence = CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private configuration temporary file",
    ))
}

pub fn parse(input: &str) -> Result<LoadedConfig, ConfigError> {
    let mut unknown = Vec::new();
    let deserializer =
        toml::Deserializer::parse(input).map_err(|error| deserialize_error(input, &error))?;
    let config: Config = serde_ignored::deserialize(deserializer, |path| {
        unknown.push(path.to_string());
    })
    .map_err(|error| deserialize_error(input, &error))?;

    config.validate()?;
    unknown.sort();
    unknown.dedup();
    Ok(LoadedConfig {
        config,
        warnings: unknown
            .into_iter()
            .map(|key| ConfigWarning::UnknownKey { key })
            .collect(),
    })
}

pub fn load(path: &Path) -> Result<LoadedConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => parse(""),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

pub fn load_effective(
    default_config_path: impl Into<PathBuf>,
    default_data_dir: impl Into<PathBuf>,
    environment: &EnvironmentOverrides,
    invocation: &ConfigOverrides,
) -> Result<EffectiveConfig, ConfigError> {
    let locations = ConfigLocations::resolve(
        default_config_path,
        default_data_dir,
        environment,
        invocation,
    );
    let loaded = load(&locations.config_path)?;
    let saved = loaded.config;
    let mut effective = saved.clone();
    invocation.apply_to(&mut effective)?;
    Ok(EffectiveConfig {
        saved,
        effective,
        warnings: loaded.warnings,
        locations,
    })
}

fn validate_language(backend: TranscriptionBackend, language: &str) -> Result<(), ConfigError> {
    crate::asr::validate_language(backend, language)
        .map_err(|error| invalid("transcription.language", error.to_string()))
}

fn validate_f64_range(key: &str, value: f64, min: f64, max: f64) -> Result<(), ConfigError> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(invalid(
            key,
            format!("expected a finite number from {min:.1} through {max:.1}, got {value}"),
        ));
    }
    Ok(())
}

fn validate_template_text(
    key: &str,
    value: &str,
    allow_transcript: bool,
) -> Result<usize, ConfigError> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut transcripts = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
            b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
            b'{' => {
                const PLACEHOLDER: &[u8] = b"{transcript}";
                if bytes[index..].starts_with(PLACEHOLDER) {
                    if !allow_transcript {
                        return Err(invalid(
                            key,
                            "the `{transcript}` placeholder belongs in prompt, not system_prompt",
                        ));
                    }
                    transcripts += 1;
                    index += PLACEHOLDER.len();
                } else {
                    return Err(invalid(
                        key,
                        "contains an unescaped `{`; only `{transcript}` is a placeholder, and literal braces must be `{{` or `}}`",
                    ));
                }
            }
            b'}' => {
                return Err(invalid(
                    key,
                    "contains an unescaped `}`; literal braces must be `{{` or `}}`",
                ));
            }
            _ => index += 1,
        }
    }
    Ok(transcripts)
}

fn deserialize_error(input: &str, error: &toml::de::Error) -> ConfigError {
    let key = error
        .span()
        .and_then(|span| key_at_offset(input, span.start))
        .unwrap_or_else(|| "<document>".to_owned());
    invalid(key, error.message())
}

fn key_at_offset(input: &str, offset: usize) -> Option<String> {
    let before = input.get(..offset.min(input.len()))?;
    let mut section = String::new();
    for line in before.lines() {
        let line = line.trim();
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = header.trim().to_owned();
        }
    }

    let line_start = input[..offset.min(input.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = input[line_start..]
        .find('\n')
        .map_or(input.len(), |index| line_start + index);
    let line = input[line_start..line_end].trim();
    let raw_key = line.split_once('=')?.0.trim().trim_matches(['\'', '"']);
    if raw_key.is_empty() {
        None
    } else if section.is_empty() {
        Some(raw_key.to_owned())
    } else {
        Some(format!("{section}.{raw_key}"))
    }
}

fn invalid(key: impl Into<String>, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        key: key.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invalid(input: &str, key: &str, expected_message: &str) {
        let error = parse(input).expect_err("configuration should be invalid");
        let rendered = error.to_string();
        assert!(
            rendered.contains(key),
            "{rendered:?} did not contain {key:?}"
        );
        assert!(
            rendered.contains(expected_message),
            "{rendered:?} did not contain {expected_message:?}"
        );
    }

    #[test]
    fn empty_document_uses_all_documented_defaults() {
        let loaded = parse("").unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.config.audio.silence_timeout, 300);
        assert_eq!(
            loaded.config.transcription.backend,
            TranscriptionBackend::Parakeet
        );
        assert_eq!(loaded.config.summarization.model, "phi-4-mini");
        assert_eq!(
            loaded.config.output.dir,
            PathBuf::from("~/sosus/recordings")
        );
    }

    #[test]
    fn tui_settings_save_preserves_unrelated_configuration_and_rejects_conflicts() {
        let directory =
            std::env::temp_dir().join(format!("sosus-config-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        fs::write(
            &path,
            "# keep this comment\n[custom]\nanswer = 42\n\n[audio]\nmic = true\n",
        )
        .unwrap();
        let expected = fingerprint(&path).unwrap();
        let mut config = load(&path).unwrap().config;
        config.audio.mic = false;
        config.output.json = true;

        let saved = save_tui_settings(&path, &expected, &config).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("# keep this comment"));
        assert!(contents.contains("[custom]"));
        assert!(contents.contains("answer = 42"));
        assert!(contents.contains("mic = false"));
        assert!(contents.contains("json = true"));
        assert!(contents.contains("min_speakers = 2"));
        assert!(contents.contains("max_speakers = 2"));

        fs::write(&path, "[audio]\nmic = true\n").unwrap();
        assert!(matches!(
            save_tui_settings(&path, &saved, &config),
            Err(ConfigError::Changed)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tui_settings_save_refuses_a_group_writable_parent() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "sosus-config-shared-test-{}-{}",
            std::process::id(),
            CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let path = directory.join("config.toml");
        let config = Config::default();

        let error = save_tui_settings(&path, &ConfigFingerprint(None), &config).unwrap_err();

        assert!(matches!(error, ConfigError::Write { .. }));
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn partial_tables_get_field_defaults() {
        let loaded = parse("[audio]\nmic = false\n[search]\ntop_k = 5\n").unwrap();
        assert!(!loaded.config.audio.mic);
        assert_eq!(loaded.config.audio.system_gain_db, -3.0);
        assert_eq!(loaded.config.search.top_k, 5);
        assert_eq!(loaded.config.search.rrf_k, 60);
    }

    #[test]
    fn malformed_type_names_the_offending_key_and_expected_type() {
        assert_invalid(
            "[audio]\nsilence_timeout = \"five minutes\"\n",
            "audio.silence_timeout",
            "expected u64",
        );
    }

    #[test]
    fn closed_enums_list_valid_variants() {
        assert_invalid(
            "[transcription]\nbackend = \"cloud\"\n",
            "transcription.backend",
            "parakeet",
        );
        assert_invalid(
            "[audio]\ncapture_mode = \"selected\"\n",
            "audio.capture_mode",
            "processes",
        );
        assert_invalid(
            "[output]\njson = \"yes\"\n",
            "output.json",
            "expected a boolean",
        );
    }

    #[test]
    fn unknown_keys_are_sorted_warnings_not_errors() {
        let loaded = parse(
            "mystery = true\n[audio]\nfuture_gain = 2\n[templates.notes]\nsystem_prompt = \"Be concise.\"\nprompt = \"Summarize {transcript}\"\ntemperature = 0.2\n",
        )
        .unwrap();
        assert_eq!(
            loaded.warnings,
            vec![
                ConfigWarning::UnknownKey {
                    key: "audio.future_gain".to_owned()
                },
                ConfigWarning::UnknownKey {
                    key: "mystery".to_owned()
                },
                ConfigWarning::UnknownKey {
                    key: "templates.notes.temperature".to_owned()
                },
            ]
        );
    }

    #[test]
    fn documented_audio_ranges_are_inclusive() {
        parse(
            "[audio]\nsystem_gain_db = -24.0\nmic_gain_db = 12.0\nsilence_threshold_dbfs = -90.0\n",
        )
        .unwrap();
        assert_invalid(
            "[audio]\nsystem_gain_db = -24.1\n",
            "audio.system_gain_db",
            "-24.0 through 12.0",
        );
        assert_invalid(
            "[audio]\nsilence_threshold_dbfs = -19.9\n",
            "audio.silence_threshold_dbfs",
            "-90.0 through -20.0",
        );
    }

    #[test]
    fn process_capture_requires_valid_pids() {
        assert_invalid(
            "[audio]\ncapture_mode = \"processes\"\n",
            "audio.processes",
            "at least one PID",
        );
        assert_invalid(
            "[audio]\nprocesses = [0]\n",
            "audio.processes",
            "greater than zero",
        );
    }

    #[test]
    fn validates_backend_model_and_language_compatibility() {
        assert_eq!(PARAKEET_LANGUAGES.len(), 25);
        assert_eq!(WHISPER_LANGUAGES.len(), 99);
        assert_invalid(
            "[transcription]\nmodel = \"base\"\n",
            "transcription.model",
            "Whisper-only",
        );
        assert_invalid(
            "[transcription]\nlanguage = \"no\"\n",
            "transcription.language",
            "Select the `whisper` backend",
        );
        parse("[transcription]\nlanguage = \"sv\"\n").unwrap();
        parse("[transcription]\nbackend = \"whisper\"\nmodel = \"base\"\nlanguage = \"no\"\n")
            .unwrap();
        parse("[transcription]\nbackend = \"whisper\"\nlanguage = \"haw\"\n").unwrap();
        assert_invalid(
            "[transcription]\nbackend = \"whisper\"\nlanguage = \"zz\"\n",
            "transcription.language",
            "not supported by whisper",
        );
    }

    #[test]
    fn validates_search_speaker_and_vocabulary_limits() {
        assert_invalid("[search]\ntop_k = 0\n", "search.top_k", "at least 1");
        assert_invalid(
            "[diarization]\nmin_speakers = 4\nmax_speakers = 2\n",
            "diarization.min_speakers",
            "must not exceed",
        );
        assert_invalid(
            "[vocabulary]\nhotword_score = 0.0\n",
            "vocabulary.hotword_score",
            "greater than zero",
        );
        assert_invalid(
            "[vocabulary]\nterms = [{ term = \"Asteron\", weight = -1.0 }]\n",
            "vocabulary.terms[0].weight",
            "greater than zero",
        );
    }

    #[test]
    fn custom_templates_require_transcript_and_escaped_other_braces() {
        parse("[summarization]\ntemplate = \"notes\"\n[templates.notes]\nsystem_prompt = 'Return JSON like {{\"summary\": \"...\"}}.'\nprompt = \"Summarize {transcript}\"\n").unwrap();
        assert_invalid(
            "[templates.notes]\nprompt = \"Summarize this\"\n",
            "templates.notes.prompt",
            "must contain",
        );
        assert_invalid(
            "[templates.notes]\nprompt = \"Summarize {transcript} into {format}\"\n",
            "templates.notes.prompt",
            "unescaped `{`",
        );
        assert_invalid(
            "[templates.notes]\nsystem_prompt = \"Use {transcript}\"\nprompt = \"Summarize {transcript}\"\n",
            "templates.notes.system_prompt",
            "belongs in prompt",
        );
    }

    #[test]
    fn environment_lookup_reads_only_two_supported_keys() {
        let mut requested = Vec::new();
        let overrides = EnvironmentOverrides::from_lookup(|key| {
            requested.push(key.to_owned());
            match key {
                "SOSUS_CONFIG" => Some(OsString::from("custom.toml")),
                "SOSUS_DATA_DIR" => Some(OsString::from("custom-data")),
                "SOSUS_TRANSCRIPTION_BACKEND" => Some(OsString::from("whisper")),
                _ => None,
            }
        });
        assert_eq!(requested, ["SOSUS_CONFIG", "SOSUS_DATA_DIR"]);
        assert_eq!(overrides.config_path, Some(PathBuf::from("custom.toml")));
        assert_eq!(overrides.data_dir, Some(PathBuf::from("custom-data")));
    }

    #[test]
    fn location_precedence_is_cli_then_environment_then_default() {
        let environment = EnvironmentOverrides {
            config_path: Some("env.toml".into()),
            data_dir: Some("env-data".into()),
        };
        let invocation = ConfigOverrides {
            config_path: Some("cli.toml".into()),
            ..ConfigOverrides::default()
        };
        let locations =
            ConfigLocations::resolve("default.toml", "default-data", &environment, &invocation);
        assert_eq!(locations.config_path, PathBuf::from("cli.toml"));
        assert_eq!(locations.data_dir, PathBuf::from("env-data"));
    }

    #[test]
    fn invocation_overrides_create_a_separate_validated_effective_layer() {
        let saved = Config::default();
        let mut effective = saved.clone();
        ConfigOverrides {
            backend: Some(TranscriptionBackend::Whisper),
            asr_model: Some("base".to_owned()),
            language: Some("no".to_owned()),
            threads: Some(4),
            output_dir: Some("/tmp/meetings".into()),
            vocabulary_terms: vec![VocabularyTerm::Simple("Asteron".to_owned())],
            ..ConfigOverrides::default()
        }
        .apply_to(&mut effective)
        .unwrap();
        assert_eq!(saved, Config::default());
        assert_eq!(
            effective.transcription.backend,
            TranscriptionBackend::Whisper
        );
        assert_eq!(effective.transcription.threads, 4);
        assert_eq!(effective.output.dir, PathBuf::from("/tmp/meetings"));
        assert_eq!(effective.vocabulary.terms.len(), 1);
    }

    #[test]
    fn cli_thread_override_must_be_at_least_one() {
        let mut config = Config::default();
        let original = config.clone();
        let error = ConfigOverrides {
            threads: Some(0),
            ..ConfigOverrides::default()
        }
        .apply_to(&mut config)
        .unwrap_err();
        assert!(error.to_string().contains("--threads"));
        assert_eq!(config, original);
    }

    #[test]
    fn invalid_invocation_override_does_not_partially_mutate_config() {
        let mut config = Config::default();
        let original = config.clone();
        let error = ConfigOverrides {
            output_dir: Some("/tmp/changed".into()),
            system_gain_db: Some(99.0),
            ..ConfigOverrides::default()
        }
        .apply_to(&mut config)
        .unwrap_err();
        assert!(error.to_string().contains("audio.system_gain_db"));
        assert_eq!(config, original);
    }
}
