//! Persistent settings editor state.

use crossterm::event::{KeyCode, KeyEvent};

use crate::{
    asr::{PARAKEET_LANGUAGES, TranscriptionBackend, WHISPER_LANGUAGES},
    config::Config,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Microphone,
    SystemLevel,
    MicrophoneLevel,
    Language,
    Diarization,
    ExpectedSpeakers,
    Engine,
    Model,
    JsonExport,
    CompactM4a,
}

impl Field {
    const WITH_DIARIZATION: [Self; 10] = [
        Self::Microphone,
        Self::SystemLevel,
        Self::MicrophoneLevel,
        Self::Language,
        Self::Diarization,
        Self::ExpectedSpeakers,
        Self::Engine,
        Self::Model,
        Self::JsonExport,
        Self::CompactM4a,
    ];

    const WITHOUT_DIARIZATION: [Self; 9] = [
        Self::Microphone,
        Self::SystemLevel,
        Self::MicrophoneLevel,
        Self::Language,
        Self::Diarization,
        Self::Engine,
        Self::Model,
        Self::JsonExport,
        Self::CompactM4a,
    ];

    fn available(diarization_enabled: bool) -> &'static [Self] {
        if diarization_enabled {
            &Self::WITH_DIARIZATION
        } else {
            &Self::WITHOUT_DIARIZATION
        }
    }

    fn next(self, reverse: bool, diarization_enabled: bool) -> Self {
        let fields = Self::available(diarization_enabled);
        let index = fields.iter().position(|field| *field == self).unwrap_or(0);
        let len = fields.len();
        fields[(index + if reverse { len - 1 } else { 1 }) % len]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Cancel,
    Save,
    PickLanguage,
    PickModel,
}

pub struct SettingsModal {
    draft: Config,
    selected: Field,
}

impl SettingsModal {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            draft: config,
            selected: Field::Microphone,
        }
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.draft
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsAction {
        match key.code {
            KeyCode::Esc => SettingsAction::Cancel,
            KeyCode::Enter => match self.selected {
                Field::Language => SettingsAction::PickLanguage,
                Field::Model => SettingsAction::PickModel,
                _ => SettingsAction::Save,
            },
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.next(true, self.draft.diarization.enabled);
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = self.selected.next(false, self.draft.diarization.enabled);
                SettingsAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.adjust(true);
                SettingsAction::None
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.adjust(false);
                SettingsAction::None
            }
            _ => SettingsAction::None,
        }
    }

    pub fn set_language(&mut self, language: String) {
        self.draft.transcription.language = language;
    }
    pub fn set_model(&mut self, model: String) {
        self.draft.transcription.backend = TranscriptionBackend::Whisper;
        self.draft.transcription.model = model;
    }
    pub fn language_options(&self) -> Vec<(String, String)> {
        let languages = match self.draft.transcription.backend {
            TranscriptionBackend::Parakeet => PARAKEET_LANGUAGES,
            TranscriptionBackend::Whisper => WHISPER_LANGUAGES,
        };
        let mut options = vec![(String::new(), "Auto-detect".to_owned())];
        options.extend(
            languages
                .iter()
                .map(|code| ((*code).to_owned(), language(code))),
        );
        options
    }
    pub fn model_options(model_dir: &std::path::Path) -> Vec<(String, String, String)> {
        let mut options: Vec<(String, String, String)> = crate::models::manifest()
            .map(|manifest| {
                manifest
                    .asr_models("whisper", model_dir)
                    .into_iter()
                    .map(|model| {
                        let status = if model.installed {
                            "installed"
                        } else {
                            "downloads on use"
                        };
                        (
                            model.alias.clone(),
                            model_label(&model.alias).to_owned(),
                            format!(
                                "{} · {} · {}",
                                model.source,
                                human_bytes(model.bytes),
                                status
                            ),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        options.push((
            "__custom__".to_owned(),
            "Import custom model…".to_owned(),
            "Choose a local GGML/GGUF model file".to_owned(),
        ));
        options
    }

    fn adjust(&mut self, reverse: bool) {
        match self.selected {
            Field::Microphone => self.draft.audio.mic = !self.draft.audio.mic,
            Field::SystemLevel => adjust_gain(&mut self.draft.audio.system_gain_db, reverse),
            Field::MicrophoneLevel => adjust_gain(&mut self.draft.audio.mic_gain_db, reverse),
            Field::Language => {
                self.draft.transcription.language = cycle_language(
                    &self.draft.transcription.language,
                    self.draft.transcription.backend,
                    reverse,
                );
            }
            Field::Diarization => self.draft.diarization.enabled = !self.draft.diarization.enabled,
            Field::ExpectedSpeakers => {
                cycle_expected_speakers(&mut self.draft.diarization, reverse)
            }
            Field::Engine => {
                self.draft.transcription.backend = match self.draft.transcription.backend {
                    TranscriptionBackend::Parakeet => TranscriptionBackend::Whisper,
                    TranscriptionBackend::Whisper => TranscriptionBackend::Parakeet,
                };
                self.draft.transcription.model.clear();
                if !self
                    .draft
                    .transcription
                    .backend
                    .capabilities()
                    .languages
                    .supports(&self.draft.transcription.language)
                {
                    self.draft.transcription.language.clear();
                }
            }
            Field::Model => {
                if self.draft.transcription.backend == TranscriptionBackend::Parakeet {
                    self.draft.transcription.backend = TranscriptionBackend::Whisper;
                    self.draft.transcription.model = if reverse {
                        "nb-whisper-small".to_owned()
                    } else {
                        "whisper-tiny".to_owned()
                    };
                } else {
                    self.draft.transcription.model =
                        cycle_model(&self.draft.transcription.model, reverse);
                }
            }
            Field::JsonExport => self.draft.output.json = !self.draft.output.json,
            Field::CompactM4a => self.draft.output.compact_m4a = !self.draft.output.compact_m4a,
        }
    }

    pub fn rows(&self) -> Vec<(&'static str, String, bool)> {
        Field::available(self.draft.diarization.enabled)
            .iter()
            .map(|field| {
                let value = match field {
                    Field::Microphone => on_off(self.draft.audio.mic).to_owned(),
                    Field::SystemLevel => gain(self.draft.audio.system_gain_db),
                    Field::MicrophoneLevel => gain(self.draft.audio.mic_gain_db),
                    Field::Language => language(&self.draft.transcription.language).to_owned(),
                    Field::Diarization => on_off(self.draft.diarization.enabled).to_owned(),
                    Field::ExpectedSpeakers => expected_speakers(&self.draft.diarization),
                    Field::Engine => match self.draft.transcription.backend {
                        TranscriptionBackend::Parakeet => "Parakeet".to_owned(),
                        TranscriptionBackend::Whisper => "Whisper".to_owned(),
                    },
                    Field::Model
                        if self.draft.transcription.backend == TranscriptionBackend::Parakeet =>
                    {
                        "Choose a Whisper model".to_owned()
                    }
                    Field::Model => model_label(&self.draft.transcription.model).to_owned(),
                    Field::JsonExport => on_off(self.draft.output.json).to_owned(),
                    Field::CompactM4a => on_off(self.draft.output.compact_m4a).to_owned(),
                };
                let label = match field {
                    Field::Microphone => "Microphone",
                    Field::SystemLevel => "System level",
                    Field::MicrophoneLevel => "Mic level",
                    Field::Language => "Language",
                    Field::Diarization => "Diarization",
                    Field::ExpectedSpeakers => "Expected speakers",
                    Field::Engine => "Engine",
                    Field::Model => "Model",
                    Field::JsonExport => "JSON export",
                    Field::CompactM4a => "Compact audio to M4A",
                };
                (label, value, *field == self.selected)
            })
            .collect()
    }
}

fn human_bytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

fn adjust_gain(value: &mut f64, reverse: bool) {
    *value = (*value + if reverse { -1.0 } else { 1.0 }).clamp(-24.0, 12.0);
}

fn cycle_language(current: &str, backend: TranscriptionBackend, reverse: bool) -> String {
    let languages = match backend {
        TranscriptionBackend::Parakeet => PARAKEET_LANGUAGES,
        TranscriptionBackend::Whisper => WHISPER_LANGUAGES,
    };
    let mut choices = vec![""];
    for preferred in ["en", "sv", "no"] {
        if languages.contains(&preferred) {
            choices.push(preferred);
        }
    }
    for code in languages {
        if !choices.contains(code) {
            choices.push(code);
        }
    }
    let index = choices
        .iter()
        .position(|language| *language == current)
        .unwrap_or(0);
    let len = choices.len();
    choices[(index + if reverse { len - 1 } else { 1 }) % len].to_owned()
}

fn cycle_model(current: &str, reverse: bool) -> String {
    const MODELS: [&str; 8] = [
        "",
        "whisper-tiny",
        "whisper-base",
        "whisper-small",
        "whisper-medium",
        "whisper-large-v3-turbo",
        "kb-whisper-base",
        "nb-whisper-small",
    ];
    let index = MODELS
        .iter()
        .position(|model| *model == current)
        .unwrap_or(0);
    let len = MODELS.len();
    MODELS[(index + if reverse { len - 1 } else { 1 }) % len].to_owned()
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}

fn gain(value: f64) -> String {
    format!("{value:+.0} dB")
}

fn expected_speakers(config: &crate::config::DiarizationConfig) -> String {
    if config.min_speakers > 0 && config.min_speakers == config.max_speakers {
        config.min_speakers.to_string()
    } else {
        "Auto".to_owned()
    }
}

fn cycle_expected_speakers(config: &mut crate::config::DiarizationConfig, reverse: bool) {
    const OPTIONS: [usize; 7] = [0, 1, 2, 3, 4, 5, 6];
    let current = if config.min_speakers > 0 && config.min_speakers == config.max_speakers {
        config.min_speakers
    } else {
        0
    };
    let index = OPTIONS
        .iter()
        .position(|count| *count == current)
        .unwrap_or(0);
    let next = OPTIONS[(index + if reverse { OPTIONS.len() - 1 } else { 1 }) % OPTIONS.len()];
    config.min_speakers = next;
    config.max_speakers = next;
}

fn language(value: &str) -> String {
    let name = match value {
        "" => return "Auto".to_owned(),
        "en" => "English",
        "zh" => "Chinese",
        "de" => "German",
        "es" => "Spanish",
        "ru" => "Russian",
        "ko" => "Korean",
        "fr" => "French",
        "ja" => "Japanese",
        "pt" => "Portuguese",
        "tr" => "Turkish",
        "pl" => "Polish",
        "ca" => "Catalan",
        "nl" => "Dutch",
        "ar" => "Arabic",
        "sv" => "Swedish",
        "it" => "Italian",
        "id" => "Indonesian",
        "hi" => "Hindi",
        "fi" => "Finnish",
        "vi" => "Vietnamese",
        "he" => "Hebrew",
        "uk" => "Ukrainian",
        "el" => "Greek",
        "ms" => "Malay",
        "cs" => "Czech",
        "ro" => "Romanian",
        "da" => "Danish",
        "hu" => "Hungarian",
        "ta" => "Tamil",
        "no" => "Norwegian",
        "th" => "Thai",
        "ur" => "Urdu",
        "hr" => "Croatian",
        "bg" => "Bulgarian",
        "lt" => "Lithuanian",
        "la" => "Latin",
        "mi" => "Māori",
        "ml" => "Malayalam",
        "cy" => "Welsh",
        "sk" => "Slovak",
        "te" => "Telugu",
        "fa" => "Persian",
        "lv" => "Latvian",
        "bn" => "Bengali",
        "sr" => "Serbian",
        "az" => "Azerbaijani",
        "sl" => "Slovenian",
        "kn" => "Kannada",
        "et" => "Estonian",
        "mk" => "Macedonian",
        "br" => "Breton",
        "eu" => "Basque",
        "is" => "Icelandic",
        "hy" => "Armenian",
        "ne" => "Nepali",
        "mn" => "Mongolian",
        "bs" => "Bosnian",
        "kk" => "Kazakh",
        "sq" => "Albanian",
        "sw" => "Swahili",
        "gl" => "Galician",
        "mr" => "Marathi",
        "pa" => "Punjabi",
        "si" => "Sinhala",
        "km" => "Khmer",
        "sn" => "Shona",
        "yo" => "Yoruba",
        "so" => "Somali",
        "af" => "Afrikaans",
        "oc" => "Occitan",
        "ka" => "Georgian",
        "be" => "Belarusian",
        "tg" => "Tajik",
        "sd" => "Sindhi",
        "gu" => "Gujarati",
        "am" => "Amharic",
        "yi" => "Yiddish",
        "lo" => "Lao",
        "uz" => "Uzbek",
        "fo" => "Faroese",
        "ht" => "Haitian Creole",
        "ps" => "Pashto",
        "tk" => "Turkmen",
        "nn" => "Nynorsk",
        "mt" => "Maltese",
        "sa" => "Sanskrit",
        "lb" => "Luxembourgish",
        "my" => "Myanmar",
        "bo" => "Tibetan",
        "tl" => "Tagalog",
        "mg" => "Malagasy",
        "as" => "Assamese",
        "tt" => "Tatar",
        "haw" => "Hawaiian",
        "ln" => "Lingala",
        "ha" => "Hausa",
        "ba" => "Bashkir",
        "jw" => "Javanese",
        "su" => "Sundanese",
        _ => "Unknown language",
    };
    format!("{name} ({value})")
}

fn model_label(value: &str) -> &'static str {
    match value {
        "" | "whisper-base" => "Whisper Base",
        "whisper-tiny" => "Whisper Tiny",
        "whisper-small" => "Whisper Small",
        "whisper-medium" => "Whisper Medium",
        "whisper-large-v3-turbo" => "Whisper Large v3 Turbo",
        "kb-whisper-base" => "KB-Whisper Base (Swedish)",
        "nb-whisper-small" => "NB-Whisper Small (Norwegian)",
        "__custom__" => "Import custom model…",
        _ => "Custom Whisper model",
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn edits_are_staged_and_language_cycles_between_supported_choices() {
        let mut settings = SettingsModal::new(Config::default());
        assert_eq!(
            settings.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            SettingsAction::None
        );
        assert!(!settings.config().audio.mic);
        settings.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        settings.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        settings.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        settings.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(settings.config().transcription.language, "en");
        settings.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(settings.config().transcription.language, "sv");
    }

    #[test]
    fn selecting_a_model_switches_from_parakeet_to_whisper() {
        let mut settings = SettingsModal::new(Config::default());
        for _ in 0..7 {
            settings.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        settings.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            settings.config().transcription.backend,
            TranscriptionBackend::Whisper
        );
        assert_eq!(settings.config().transcription.model, "whisper-tiny");
    }

    #[test]
    fn expected_speakers_cycles_and_is_hidden_when_diarization_is_off() {
        let mut settings = SettingsModal::new(Config::default());
        assert!(
            settings
                .rows()
                .iter()
                .any(|(label, value, _)| *label == "Expected speakers" && value == "2")
        );

        for _ in 0..5 {
            settings.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        settings.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(settings.config().diarization.min_speakers, 3);
        assert_eq!(settings.config().diarization.max_speakers, 3);

        settings.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        settings.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!settings.config().diarization.enabled);
        assert!(
            !settings
                .rows()
                .iter()
                .any(|(label, _, _)| *label == "Expected speakers")
        );
    }

    #[test]
    fn compact_m4a_is_a_visible_opt_in_setting() {
        let mut settings = SettingsModal::new(Config::default());
        assert!(
            settings
                .rows()
                .iter()
                .any(|(label, value, _)| { *label == "Compact audio to M4A" && value == "Off" })
        );
        while settings.selected != Field::CompactM4a {
            settings.selected = settings.selected.next(false, true);
        }
        settings.adjust(false);
        assert!(settings.config().output.compact_m4a);
    }
}
