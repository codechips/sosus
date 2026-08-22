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
    Engine,
    Model,
    JsonExport,
}

impl Field {
    const ALL: [Self; 8] = [
        Self::Microphone,
        Self::SystemLevel,
        Self::MicrophoneLevel,
        Self::Language,
        Self::Diarization,
        Self::Engine,
        Self::Model,
        Self::JsonExport,
    ];

    fn next(self, reverse: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        let len = Self::ALL.len();
        Self::ALL[(index + if reverse { len - 1 } else { 1 }) % len]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Cancel,
    Save,
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
            KeyCode::Enter => SettingsAction::Save,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.next(true);
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = self.selected.next(false);
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
                if self.draft.transcription.backend == TranscriptionBackend::Whisper {
                    self.draft.transcription.model =
                        cycle_model(&self.draft.transcription.model, reverse);
                }
            }
            Field::JsonExport => self.draft.output.json = !self.draft.output.json,
        }
    }

    pub fn rows(&self) -> Vec<(&'static str, String, bool)> {
        Field::ALL
            .iter()
            .map(|field| {
                let value = match field {
                    Field::Microphone => on_off(self.draft.audio.mic).to_owned(),
                    Field::SystemLevel => gain(self.draft.audio.system_gain_db),
                    Field::MicrophoneLevel => gain(self.draft.audio.mic_gain_db),
                    Field::Language => language(&self.draft.transcription.language).to_owned(),
                    Field::Diarization => on_off(self.draft.diarization.enabled).to_owned(),
                    Field::Engine => match self.draft.transcription.backend {
                        TranscriptionBackend::Parakeet => "Parakeet".to_owned(),
                        TranscriptionBackend::Whisper => "Whisper".to_owned(),
                    },
                    Field::Model => model_label(&self.draft.transcription.model).to_owned(),
                    Field::JsonExport => on_off(self.draft.output.json).to_owned(),
                };
                let label = match field {
                    Field::Microphone => "Microphone",
                    Field::SystemLevel => "System level",
                    Field::MicrophoneLevel => "Mic level",
                    Field::Language => "Language",
                    Field::Diarization => "Diarization",
                    Field::Engine => "Engine",
                    Field::Model => "Model",
                    Field::JsonExport => "JSON export",
                };
                (label, value, *field == self.selected)
            })
            .collect()
    }
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

fn language(value: &str) -> String {
    match value {
        "" => "Auto".to_owned(),
        "sv" => "Swedish (sv)".to_owned(),
        "en" => "English (en)".to_owned(),
        "no" => "Norwegian (no)".to_owned(),
        code => code.to_owned(),
    }
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
}
