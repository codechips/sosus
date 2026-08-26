//! Terminal UI state and event loop.

mod modals;
mod panes;
mod theme;
mod widgets;

use std::{
    collections::VecDeque,
    env,
    io::{self, Stdout, Write as _},
    panic,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Context;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

use crate::{
    archive::{self, Meeting, Segment},
    audio,
    config::{self, Config, ConfigFingerprint},
    paths::AppPaths,
    pipeline::{self, AppEvent as PipelineEvent},
};

const MINIMUM_WIDTH: u16 = 80;
const MINIMUM_HEIGHT: u16 = 24;
const DEFAULT_SIDEBAR_WIDTH: u16 = 28;
const MINIMUM_SIDEBAR_WIDTH: u16 = 18;
const MINIMUM_TRANSCRIPT_WIDTH: u16 = 40;
const PROCESSING_DOTS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(30);
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Meetings,
    Transcript,
    Recording,
}

impl Focus {
    const ALL: [Self; 3] = [Self::Meetings, Self::Transcript, Self::Recording];

    fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|focus| *focus == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|focus| *focus == self)
            .unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug)]
enum UiEvent {
    Terminal(Event),
    InputError(String),
}

struct App {
    archive_dir: String,
    error: Option<String>,
    focus: Focus,
    show_help: bool,
    settings: Option<modals::settings::SettingsModal>,
    picker: Option<PickerKind>,
    settings_context: Option<SettingsContext>,
    confirm_quit_processing: bool,
    delete_confirmation: Option<Meeting>,
    retranscribe_confirmation: Option<Retranscription>,
    retranscribe_speakers: Option<RetranscribeSpeakerPicker>,
    should_quit: bool,
    message: Option<String>,
    warnings: VecDeque<String>,
    recording_context: Option<RecordingStartup>,
    recording: Option<ActiveRecording>,
    interrupted_recording: Option<InterruptedRecording>,
    reconnecting: Option<ReconnectingRecording>,
    last_recording: Option<String>,
    pipeline_status: Option<String>,
    pipeline_active: bool,
    processing_spinner_frame: usize,
    meetings: Vec<Meeting>,
    selected_meeting: usize,
    transcript: Vec<Segment>,
    transcript_scroll: u16,
    selected_transcript_segment: usize,
    preview: Option<AudioPreview>,
    input_levels: Option<(f32, f32)>,
    sidebar_width: u16,
    resizing_sidebar: bool,
}

impl App {
    fn new(startup: Startup) -> Self {
        Self {
            archive_dir: startup.archive_dir,
            error: None,
            focus: Focus::Meetings,
            show_help: false,
            settings: None,
            picker: None,
            settings_context: startup.settings.map(|settings| SettingsContext {
                config: settings.config,
                config_path: settings.config_path,
                fingerprint: settings.fingerprint,
                model_dir: settings.model_dir,
            }),
            confirm_quit_processing: false,
            delete_confirmation: None,
            retranscribe_confirmation: None,
            retranscribe_speakers: None,
            should_quit: false,
            message: None,
            warnings: startup.warnings.into(),
            recording_context: startup.recording,
            recording: None,
            interrupted_recording: None,
            reconnecting: None,
            last_recording: None,
            pipeline_status: None,
            pipeline_active: false,
            processing_spinner_frame: 0,
            meetings: Vec::new(),
            selected_meeting: 0,
            transcript: Vec::new(),
            transcript_scroll: 0,
            selected_transcript_segment: 0,
            preview: None,
            input_levels: None,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            resizing_sidebar: false,
        }
    }

    fn refresh_archive(&mut self) {
        if let Ok(meetings) = archive::discover(std::path::Path::new(&self.archive_dir)) {
            self.meetings = meetings;
            self.selected_meeting = self
                .selected_meeting
                .min(self.meetings.len().saturating_sub(1));
            self.load_selected_transcript();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.meetings.is_empty() {
            return;
        }
        let last = self.meetings.len() - 1;
        let selected = if delta.is_negative() {
            self.selected_meeting.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected_meeting + delta as usize).min(last)
        };
        self.select_meeting(selected);
    }

    fn select_meeting(&mut self, selected: usize) {
        let Some(meeting) = self.meetings.get(selected) else {
            return;
        };
        self.selected_meeting = selected;
        let _ = meeting;
        self.load_selected_transcript();
    }

    fn load_selected_transcript(&mut self) {
        self.transcript = self
            .meetings
            .get(self.selected_meeting)
            .map(|meeting| {
                if meeting.transcript.is_empty() {
                    archive::load_transcript(meeting).unwrap_or_default()
                } else {
                    meeting.transcript.clone()
                }
            })
            .unwrap_or_default();
        self.transcript_scroll = 0;
        self.selected_transcript_segment = 0;
    }

    fn toggle_preview(&mut self) {
        let Some(meeting) = self.meetings.get(self.selected_meeting) else {
            self.message = Some("Choose a recording to preview".to_owned());
            return;
        };
        let Some(path) = archive::recording_path(&meeting.path) else {
            self.error = Some("Recording file was not found".to_owned());
            return;
        };
        if let Some(preview) = &mut self.preview {
            if preview.meeting_path == meeting.path {
                match preview.toggle() {
                    Ok(()) => return,
                    Err(error) => {
                        self.error = Some(error.to_string());
                        self.preview = None;
                        return;
                    }
                }
            }
            preview.stop();
        }
        match AudioPreview::start(path, meeting.path.clone(), meeting.name.clone()) {
            Ok(preview) => self.preview = Some(preview),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn stop_preview(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.stop();
        }
    }

    fn skip_preview(&mut self, seconds: f64) {
        if let Some(preview) = &mut self.preview {
            preview.skip(seconds);
            self.follow_preview();
        }
    }

    fn play_selected_segment(&mut self) {
        let Some(segment) = self.transcript.get(self.selected_transcript_segment) else {
            return;
        };
        let position = segment.start_s;
        let selected_path = self
            .meetings
            .get(self.selected_meeting)
            .map(|meeting| meeting.path.clone());
        if self
            .preview
            .as_ref()
            .is_none_or(|preview| Some(&preview.meeting_path) != selected_path.as_ref())
        {
            self.toggle_preview();
        }
        if let Some(preview) = &mut self.preview {
            preview.seek(position);
            if let Err(error) = preview.play() {
                self.error = Some(error.to_string());
                self.preview = None;
            }
        }
    }

    fn move_transcript_selection(&mut self, delta: isize) {
        if self.transcript.is_empty() {
            return;
        }
        let last = self.transcript.len() - 1;
        self.selected_transcript_segment = if delta.is_negative() {
            self.selected_transcript_segment
                .saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected_transcript_segment + delta as usize).min(last)
        };
        self.transcript_scroll = (self.selected_transcript_segment.saturating_mul(3))
            .try_into()
            .unwrap_or(u16::MAX);
    }

    fn follow_preview(&mut self) {
        let Some(preview) = &self.preview else {
            return;
        };
        let position = preview.position_seconds();
        if let Some(index) = self
            .transcript
            .iter()
            .position(|segment| segment.start_s <= position && position < segment.end_s)
        {
            self.selected_transcript_segment = index;
            self.transcript_scroll = (index.saturating_mul(3)).try_into().unwrap_or(u16::MAX);
        }
    }

    fn update_preview(&mut self) {
        let Some(preview) = &self.preview else {
            return;
        };
        if preview.is_playing() {
            self.follow_preview();
        } else if preview.finished() {
            self.preview = None;
        }
    }

    fn meeting_row_at(&self, row: u16, terminal_height: u16) -> Option<usize> {
        let recording_height = u16::from(self.recording.is_some()) * 4;
        let pane_height = terminal_height.saturating_sub(2 + recording_height);
        let index = row.checked_sub(2)? as usize;
        (index < pane_height.saturating_sub(2) as usize && index < self.meetings.len())
            .then_some(index)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<AppAction> {
        if let Some(picker) = &mut self.retranscribe_speakers {
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    let picker = self
                        .retranscribe_speakers
                        .take()
                        .expect("speaker picker should be present");
                    return Some(AppAction::TranscribeMeeting {
                        path: picker.path,
                        force: true,
                        language: Some(picker.language),
                        diarization: Some(picker.diarization),
                    });
                }
                (KeyCode::Esc, _) => self.retranscribe_speakers = None,
                (KeyCode::Up | KeyCode::Char('k'), _) => {
                    picker.diarization.previous_expected_speakers()
                }
                (KeyCode::Down | KeyCode::Char('j'), _) => {
                    picker.diarization.cycle_expected_speakers()
                }
                (KeyCode::Char('0'), _) => picker.diarization.expected_speakers = None,
                (KeyCode::Char(value), _) if ('1'..='6').contains(&value) => {
                    picker.diarization.expected_speakers = Some((value as u8 - b'0') as usize);
                }
                _ => {}
            }
            return None;
        }

        if self.retranscribe_confirmation.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    let retranscription = self
                        .retranscribe_confirmation
                        .take()
                        .expect("retranscription confirmation should contain a meeting");
                    let diarization = RecordingDiarization::from_config(
                        self.settings_context
                            .as_ref()
                            .map(|context| &context.config),
                    );
                    if diarization.enabled {
                        self.retranscribe_speakers = Some(RetranscribeSpeakerPicker {
                            path: retranscription.meeting.path,
                            language: retranscription.language,
                            diarization,
                        });
                    } else {
                        return Some(AppAction::TranscribeMeeting {
                            path: retranscription.meeting.path,
                            force: true,
                            language: Some(retranscription.language),
                            diarization: Some(diarization),
                        });
                    }
                }
                (KeyCode::Esc, _) | (KeyCode::Char('t'), _) => {
                    self.retranscribe_confirmation = None;
                }
                _ => {}
            }
            return None;
        }

        if self.delete_confirmation.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    let path = self
                        .delete_confirmation
                        .take()
                        .expect("delete confirmation should contain a meeting")
                        .path;
                    return Some(AppAction::TrashMeetingFolder(path));
                }
                (KeyCode::Esc, _) | (KeyCode::Char('d'), _) => {
                    self.delete_confirmation = None;
                }
                _ => {}
            }
            return None;
        }

        if self.confirm_quit_processing {
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => self.should_quit = true,
                (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
                    self.confirm_quit_processing = false;
                }
                _ => {}
            }
            return None;
        }

        if self.error.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::NONE)
                    if self.interrupted_recording.is_some() =>
                {
                    return Some(AppAction::ContinueInterruptedRecording);
                }
                (KeyCode::Esc | KeyCode::Enter, _) => {
                    self.error = None;
                    self.interrupted_recording = None;
                    return None;
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {}
                _ => return None,
            }
        } else if !self.warnings.is_empty() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Enter, _) => {
                    self.warnings.pop_front();
                    return None;
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {}
                _ => return None,
            }
        }

        if let Some(picker) = &mut self.picker {
            match picker.modal.handle_key(key) {
                modals::picker::PickerAction::Cancel => self.picker = None,
                modals::picker::PickerAction::Choose(value) => {
                    match picker.kind.clone() {
                        PickerType::SettingsLanguage => {
                            if let Some(settings) = &mut self.settings {
                                settings.set_language(value);
                            }
                        }
                        PickerType::RecordingLanguage => {
                            if let Some(recording) = &mut self.recording {
                                recording.language = value.clone();
                                self.message = Some(format!(
                                    "Transcription language: {}",
                                    language_label(&value)
                                ));
                            }
                        }
                        PickerType::RetranscribeLanguage(meeting) => {
                            self.retranscribe_confirmation = Some(Retranscription {
                                meeting,
                                language: value,
                            });
                        }
                        PickerType::Model if value == "__custom__" => {
                            if let Some(settings) = &mut self.settings {
                                match choose_custom_model() {
                                    Ok(path) => settings.set_model(path.display().to_string()),
                                    Err(error) => {
                                        self.error =
                                            Some(format!("Could not import custom model: {error}"))
                                    }
                                }
                            }
                        }
                        PickerType::Model => {
                            if let Some(settings) = &mut self.settings {
                                settings.set_model(value);
                            }
                        }
                    }
                    self.picker = None;
                }
                modals::picker::PickerAction::None => {}
            }
            return None;
        }
        if let Some(settings) = &mut self.settings {
            match settings.handle_key(key) {
                modals::settings::SettingsAction::Cancel => self.settings = None,
                modals::settings::SettingsAction::Save => return Some(AppAction::SaveSettings),
                modals::settings::SettingsAction::PickLanguage => self.open_language_picker(),
                modals::settings::SettingsAction::PickModel => self.open_model_picker(),
                modals::settings::SettingsAction::None => {}
            }
            return None;
        }

        if key.code == KeyCode::Esc && self.show_help {
            self.show_help = false;
            return None;
        }

        if self.preview.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('p'), _) => {
                    self.toggle_preview();
                    return None;
                }
                (KeyCode::Char('x') | KeyCode::Esc, _) => {
                    self.stop_preview();
                    return None;
                }
                (KeyCode::Left, KeyModifiers::SHIFT) => {
                    self.skip_preview(-30.0);
                    return None;
                }
                (KeyCode::Right, KeyModifiers::SHIFT) => {
                    self.skip_preview(30.0);
                    return None;
                }
                (KeyCode::Left, _) => {
                    self.skip_preview(-5.0);
                    return None;
                }
                (KeyCode::Right, _) => {
                    self.skip_preview(5.0);
                    return None;
                }
                _ => {}
            }
        }

        match (key.code, key.modifiers) {
            (KeyCode::Up | KeyCode::Char('k'), _) if self.focus == Focus::Meetings => {
                self.move_selection(-1);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) if self.focus == Focus::Meetings => {
                self.move_selection(1);
            }
            (KeyCode::Up | KeyCode::Char('k'), _) if self.focus == Focus::Transcript => {
                self.move_transcript_selection(-1);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) if self.focus == Focus::Transcript => {
                self.move_transcript_selection(1);
            }
            (KeyCode::Enter, _) if self.focus == Focus::Transcript => {
                self.play_selected_segment();
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) if self.recording.is_some() => {
                return Some(AppAction::StopRecording);
            }
            (KeyCode::Char('q'), _) if self.recording.is_some() => {
                return Some(AppAction::StopRecordingAndQuit);
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _)
                if self.pipeline_active =>
            {
                self.confirm_quit_processing = true;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('r'), _) if self.reconnecting.is_some() => {}
            (KeyCode::Char('r'), _) => return Some(AppAction::ToggleRecording),
            (KeyCode::Char('m'), _) => return microphone_mute_action(self.recording.is_some()),
            (KeyCode::Char('p'), _) if self.recording.is_none() && !self.pipeline_active => {
                self.toggle_preview();
            }
            (KeyCode::Char('s'), _) if self.recording.is_some() => {
                if let Some(active) = &mut self.recording {
                    active.diarization.cycle_expected_speakers();
                }
            }
            (KeyCode::Char('l'), _) if self.recording.is_some() => {
                self.open_recording_language_picker();
            }
            (KeyCode::Char('l'), _) if !self.pipeline_active => {
                if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                    self.open_retranscription_language_picker(meeting.clone());
                }
            }
            (KeyCode::Char('t'), _) if self.recording.is_none() && !self.pipeline_active => {
                if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                    if meeting.path.join("transcript.md").is_file() {
                        self.retranscribe_confirmation = Some(Retranscription {
                            meeting: meeting.clone(),
                            language: self
                                .settings_context
                                .as_ref()
                                .map(|context| context.config.transcription.language.clone())
                                .unwrap_or_default(),
                        });
                    } else {
                        return Some(AppAction::TranscribeMeeting {
                            path: meeting.path.clone(),
                            force: false,
                            diarization: None,
                            language: None,
                        });
                    }
                }
            }
            (KeyCode::Char('o'), _) => {
                if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                    return Some(AppAction::OpenMeetingFolder(meeting.path.clone()));
                }
            }
            (KeyCode::Char('d'), _) => {
                self.delete_confirmation = self.meetings.get(self.selected_meeting).cloned();
            }
            (KeyCode::Char('D'), _) => {
                if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                    return Some(AppAction::TrashMeetingFolder(meeting.path.clone()));
                }
            }
            (KeyCode::Char('?'), _) => self.show_help = !self.show_help,
            (KeyCode::F(2), _) => self.open_settings(),
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => {
                self.focus = self.focus.previous();
            }
            (KeyCode::Tab, _) => self.focus = self.focus.next(),
            _ => {}
        }
        None
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, terminal_width: u16, terminal_height: u16) {
        if self.error.is_some()
            || !self.warnings.is_empty()
            || self.show_help
            || self.settings.is_some()
            || self.picker.is_some()
            || self.confirm_quit_processing
            || self.delete_confirmation.is_some()
            || self.retranscribe_confirmation.is_some()
            || self.retranscribe_speakers.is_some()
        {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if mouse.row > 0
                    && mouse
                        .column
                        .abs_diff(self.clamped_sidebar_width(terminal_width))
                        <= 1 =>
            {
                self.resizing_sidebar = true;
            }
            MouseEventKind::Down(MouseButton::Left)
                if mouse.column < self.clamped_sidebar_width(terminal_width) =>
            {
                if let Some(selected) = self.meeting_row_at(mouse.row, terminal_height) {
                    self.select_meeting(selected);
                    self.focus = Focus::Meetings;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing_sidebar => {
                self.sidebar_width = mouse.column.clamp(
                    MINIMUM_SIDEBAR_WIDTH,
                    self.maximum_sidebar_width(terminal_width),
                );
            }
            MouseEventKind::Up(MouseButton::Left) => self.resizing_sidebar = false,
            MouseEventKind::ScrollUp
                if mouse.column >= self.clamped_sidebar_width(terminal_width) =>
            {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(3);
                self.focus = Focus::Transcript;
            }
            MouseEventKind::ScrollDown
                if mouse.column >= self.clamped_sidebar_width(terminal_width) =>
            {
                self.transcript_scroll = self.transcript_scroll.saturating_add(3);
                self.focus = Focus::Transcript;
            }
            _ => {}
        }
    }

    fn clamped_sidebar_width(&self, terminal_width: u16) -> u16 {
        self.sidebar_width.clamp(
            MINIMUM_SIDEBAR_WIDTH,
            self.maximum_sidebar_width(terminal_width),
        )
    }

    fn maximum_sidebar_width(&self, terminal_width: u16) -> u16 {
        terminal_width
            .saturating_sub(MINIMUM_TRANSCRIPT_WIDTH)
            .max(MINIMUM_SIDEBAR_WIDTH)
    }

    fn open_settings(&mut self) {
        let Some(context) = &self.settings_context else {
            self.error = Some("Settings are not configured for this session".to_owned());
            return;
        };
        self.settings = Some(modals::settings::SettingsModal::new(context.config.clone()));
    }
    fn open_language_picker(&mut self) {
        let Some(settings) = &self.settings else {
            return;
        };
        self.picker = Some(PickerKind::new(
            PickerType::SettingsLanguage,
            "Language",
            settings
                .language_options()
                .into_iter()
                .map(|(value, label)| (value, label, String::new()))
                .collect(),
            &settings.config().transcription.language,
        ));
    }
    fn open_recording_language_picker(&mut self) {
        let Some(recording) = &self.recording else {
            return;
        };
        let backend = self
            .settings_context
            .as_ref()
            .map(|context| context.config.transcription.backend)
            .unwrap_or_default();
        self.picker = Some(PickerKind::new(
            PickerType::RecordingLanguage,
            "Transcription language",
            modals::settings::SettingsModal::language_options_for_backend(backend)
                .into_iter()
                .map(|(value, label)| (value, label, String::new()))
                .collect(),
            &recording.language,
        ));
    }
    fn open_retranscription_language_picker(&mut self, meeting: Meeting) {
        let Some(context) = &self.settings_context else {
            self.error = Some("Settings are not configured for this session".to_owned());
            return;
        };
        self.picker = Some(PickerKind::new(
            PickerType::RetranscribeLanguage(meeting),
            "Transcription language",
            modals::settings::SettingsModal::language_options_for_backend(
                context.config.transcription.backend,
            )
            .into_iter()
            .map(|(value, label)| (value, label, String::new()))
            .collect(),
            &context.config.transcription.language,
        ));
    }
    fn open_model_picker(&mut self) {
        let Some(context) = &self.settings_context else {
            return;
        };
        let Some(settings) = &self.settings else {
            return;
        };
        self.picker = Some(PickerKind::new(
            PickerType::Model,
            "Whisper model",
            modals::settings::SettingsModal::model_options(&context.model_dir),
            &settings.config().transcription.model,
        ));
    }

    fn save_settings(&mut self) {
        let Some(settings) = self.settings.take() else {
            return;
        };
        let Some(context) = &mut self.settings_context else {
            self.error = Some("Settings are not configured for this session".to_owned());
            return;
        };
        match config::save_tui_settings(
            &context.config_path,
            &context.fingerprint,
            settings.config(),
        ) {
            Ok(fingerprint) => {
                context.config = settings.config().clone();
                context.fingerprint = fingerprint;
                if let Some(recording) = &mut self.recording_context {
                    recording.mix_settings = audio::MixSettings::from_db(
                        context.config.audio.system_gain_db,
                        context.config.audio.mic_gain_db,
                    );
                }
                self.message = Some("Settings saved · applies to the next operation".to_owned());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    async fn start_recording(&mut self) -> anyhow::Result<()> {
        self.stop_preview();
        let context = self
            .recording_context
            .as_ref()
            .context("recording is not configured")?;
        audio::ensure_capture_permissions().await?;
        let started_at = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let (meeting_dir, session) = audio::RecordingSession::start_new_meeting_with_mix_settings(
            &context.app_paths,
            started_at,
            context.mix_settings,
        )?;
        let formats = session.source_formats();
        tracing::info!(
            event = "recording_started",
            status = "tui",
            system_sample_rate = formats.system_sample_rate,
            system_channels = formats.system_channels,
            microphone_sample_rate = formats.microphone_sample_rate,
            microphone_channels = formats.microphone_channels,
        );
        self.recording = Some(ActiveRecording {
            session,
            meeting_dir,
            language: self
                .settings_context
                .as_ref()
                .map(|context| context.config.transcription.language.clone())
                .unwrap_or_default(),
            diarization: RecordingDiarization::from_config(
                self.settings_context
                    .as_ref()
                    .map(|context| &context.config),
            ),
        });
        self.input_levels = Some((0.0, 0.0));
        self.interrupted_recording = None;
        self.reconnecting = None;
        self.message = None;
        Ok(())
    }

    async fn continue_recording(&mut self) -> anyhow::Result<()> {
        let interrupted = self
            .interrupted_recording
            .as_ref()
            .context("there is no interrupted recording to continue")?;
        let context = self
            .recording_context
            .as_ref()
            .context("recording is not configured")?;
        let interruption_seconds = interrupted.stopped_at.elapsed().as_secs_f64();
        let session = audio::RecordingSession::continue_with_mix_settings(
            &interrupted.path,
            context.mix_settings,
            interruption_seconds,
        )?;
        tracing::info!(
            event = "recording_continued",
            elapsed_ms = (interruption_seconds * 1_000.0) as u64,
            status = "tui"
        );
        self.recording = Some(ActiveRecording {
            session,
            meeting_dir: interrupted.meeting_dir.clone(),
            language: interrupted.language.clone(),
            diarization: interrupted.diarization,
        });
        self.input_levels = Some((0.0, 0.0));
        self.error = None;
        self.message = Some("Recording continued; interruption is preserved as silence".to_owned());
        self.interrupted_recording = None;
        Ok(())
    }

    fn begin_reconnect(&mut self, failure: String, completed: CompletedRecording) {
        self.reconnecting = Some(ReconnectingRecording {
            interrupted: InterruptedRecording {
                path: completed.path,
                meeting_dir: completed.meeting_dir,
                language: completed.language,
                diarization: completed.diarization,
                stopped_at: Instant::now(),
            },
            failure,
            started_at: Instant::now(),
            next_attempt: Instant::now(),
            attempts: 0,
        });
        self.message = None;
    }

    fn retry_reconnect(&mut self) {
        let Some(reconnecting) = &mut self.reconnecting else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(reconnecting.started_at) >= RECONNECT_TIMEOUT {
            let reconnecting = self.reconnecting.take().expect("reconnect state exists");
            let path = reconnecting.interrupted.path.display().to_string();
            self.interrupted_recording = Some(reconnecting.interrupted);
            self.error = Some(format!(
                "{}\n\nSaved partial recording to {path}. Automatic reconnection did not succeed within 30 seconds.\n\n[c] Continue after fixing the audio issue — the interruption will be preserved as silence.\n[Enter/Esc] Dismiss",
                reconnecting.failure
            ));
            return;
        }
        if now < reconnecting.next_attempt {
            return;
        }
        reconnecting.attempts += 1;
        reconnecting.next_attempt = now + RECONNECT_INTERVAL;
        let Some(context) = &self.recording_context else {
            return;
        };
        let interruption_seconds = reconnecting.interrupted.stopped_at.elapsed().as_secs_f64();
        match audio::RecordingSession::continue_with_mix_settings(
            &reconnecting.interrupted.path,
            context.mix_settings,
            interruption_seconds,
        ) {
            Ok(session) => {
                tracing::info!(
                    event = "recording_auto_reconnected",
                    attempt = reconnecting.attempts,
                    elapsed_ms = (interruption_seconds * 1_000.0) as u64,
                    status = "tui"
                );
                self.recording = Some(ActiveRecording {
                    session,
                    meeting_dir: reconnecting.interrupted.meeting_dir.clone(),
                    language: reconnecting.interrupted.language.clone(),
                    diarization: reconnecting.interrupted.diarization,
                });
                self.input_levels = Some((0.0, 0.0));
                self.message =
                    Some("Audio reconnected; interruption preserved as silence".to_owned());
                self.reconnecting = None;
            }
            Err(error) => {
                tracing::warn!(
                    event = "recording_reconnect_attempt",
                    attempt = reconnecting.attempts,
                    error_category = "capture_start_failed",
                    status = "retrying"
                );
                let _ = error;
            }
        }
    }

    fn stop_recording(&mut self) -> anyhow::Result<Option<CompletedRecording>> {
        let Some(active) = self.recording.take() else {
            return Ok(None);
        };
        self.input_levels = None;
        self.recording_context
            .as_ref()
            .context("recording is not configured")?;
        let outcome = active.session.finish()?;
        self.last_recording = Some(active.meeting_dir.display().to_string());
        self.message = Some(format!(
            "Saved recording ({:.1}s)",
            outcome.duration_seconds
        ));
        if outcome.system_dropouts > 0 || outcome.microphone_dropouts > 0 {
            self.message = Some(format!(
                "Saved recording; dropouts system={}, mic={}",
                outcome.system_dropouts, outcome.microphone_dropouts
            ));
        }
        if outcome.microphone_failed {
            self.message = Some("Saved recording; microphone stream was lost".to_owned());
        }
        Ok(Some(CompletedRecording {
            path: outcome.path,
            meeting_dir: active.meeting_dir,
            language: active.language,
            diarization: active.diarization,
        }))
    }

    fn pump_recording(&mut self) -> anyhow::Result<()> {
        if let Some(active) = &mut self.recording {
            active.session.pump()?;
            self.input_levels = Some(active.session.input_levels());
        }
        Ok(())
    }

    fn handle_pipeline_event(&mut self, event: PipelineEvent) {
        let completed = matches!(event, PipelineEvent::WorkCompleted);
        match event {
            PipelineEvent::WorkStarted => {
                self.pipeline_active = true;
                self.processing_spinner_frame = 0;
                self.pipeline_status = Some("Starting".to_owned());
            }
            PipelineEvent::Stage(stage) => self.pipeline_status = Some(stage),
            PipelineEvent::WorkProgress { .. } => {}
            PipelineEvent::WorkCompleted => {
                self.pipeline_active = false;
                self.pipeline_status = None;
            }
            PipelineEvent::WorkCancelled => {
                self.pipeline_active = false;
                self.pipeline_status = Some("Processing cancelled".to_owned());
            }
            PipelineEvent::WorkFailed(error) => {
                self.pipeline_active = false;
                self.pipeline_status = Some(format!("Processing failed: {error}"));
            }
            PipelineEvent::WorkerStopped => {}
        }
        if completed {
            self.refresh_archive();
        }
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < MINIMUM_WIDTH || area.height < MINIMUM_HEIGHT {
            render_too_small(frame, area);
            return;
        }
        let content_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        );
        render_header_bar(frame, area);
        let sidebar_width = self.clamped_sidebar_width(area.width);

        let (columns, recording_area, preview_area) = if self.recording.is_some() {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                // Keep the recording controls in a compact, predictable lower pane. A
                // fixed height also prevents the visualizer from swallowing the explorer
                // and transcript when the terminal is tall.
                .constraints([Constraint::Min(8), Constraint::Length(4)])
                .split(content_area);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(sidebar_width),
                    Constraint::Min(MINIMUM_TRANSCRIPT_WIDTH),
                ])
                .split(rows[0]);
            (columns, Some(rows[1]), None)
        } else if self.preview.is_some() {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(8), Constraint::Length(1)])
                .split(content_area);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(sidebar_width),
                    Constraint::Min(MINIMUM_TRANSCRIPT_WIDTH),
                ])
                .split(rows[0]);
            (columns, None, Some(rows[1]))
        } else {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(sidebar_width),
                    Constraint::Min(MINIMUM_TRANSCRIPT_WIDTH),
                ])
                .split(content_area);
            (columns, None, None)
        };

        panes::meetings::render(
            frame,
            columns[0],
            self.focus == Focus::Meetings,
            &self.meetings,
            self.selected_meeting,
        );
        panes::transcript::render(
            frame,
            columns[1],
            self.focus == Focus::Transcript,
            &self.transcript,
            self.transcript_scroll,
            self.preview.as_ref().and_then(|preview| {
                self.transcript.iter().position(|segment| {
                    segment.start_s <= preview.position_seconds()
                        && preview.position_seconds() < segment.end_s
                })
            }),
            (!self.transcript.is_empty()).then_some(self.selected_transcript_segment),
        );
        if let Some(recording_area) = recording_area {
            panes::recording::render(
                frame,
                recording_area,
                self.focus == Focus::Recording,
                self.recording
                    .as_ref()
                    .map(|active| active.session.elapsed_seconds()),
                self.last_recording.as_deref(),
                self.input_levels,
                self.recording
                    .as_ref()
                    .is_some_and(|active| active.session.microphone_muted()),
            );
        }
        if let Some(preview_area) = preview_area {
            if let Some(preview) = &self.preview {
                render_preview_bar(frame, preview_area, preview);
            }
        }

        if let Some(error) = &self.error {
            render_notice(frame, "Error", error, centered_rect(70, 42, area));
        } else if let Some(warning) = self.warnings.front() {
            render_notice(
                frame,
                "Config warning",
                warning,
                centered_rect(70, 42, area),
            );
        } else if self.show_help {
            render_help(frame, centered_rect(64, 68, area));
        } else if let Some(picker) = &self.picker {
            let height = if matches!(&picker.kind, PickerType::Model) {
                54
            } else {
                72
            };
            render_picker(frame, &picker.modal, centered_rect(64, height, area));
        } else if let Some(settings) = &self.settings {
            render_settings(frame, settings, centered_rect(58, 78, area));
        } else if self.confirm_quit_processing {
            render_quit_processing_confirmation(frame, centered_rect(54, 28, area));
        } else if let Some(retranscription) = &self.retranscribe_confirmation {
            let model = retranscription_model(
                self.settings_context
                    .as_ref()
                    .map(|context| &context.config),
            );
            render_retranscribe_confirmation(
                frame,
                &retranscription.meeting.name,
                &model,
                &language_label(&retranscription.language),
                centered_rect(58, 50, area),
            );
        } else if let Some(picker) = &self.retranscribe_speakers {
            render_retranscribe_speaker_picker(frame, picker, centered_rect(70, 50, area));
        } else if let Some(meeting) = &self.delete_confirmation {
            render_delete_confirmation(frame, &meeting.name, centered_rect(54, 28, area));
        }
        render_status_bar(frame, area, self);
    }

    fn terminal_activity(&self) -> Option<&'static str> {
        if self.recording.is_some() {
            Some("Recording")
        } else if self.reconnecting.is_some() {
            Some("Reconnecting")
        } else if self.pipeline_active {
            match self.pipeline_status.as_deref() {
                Some("Transcribing") => Some("Transcribing"),
                Some("Diarizing") => Some("Diarizing"),
                _ => Some("Processing"),
            }
        } else {
            None
        }
    }
}

fn choose_custom_model() -> anyhow::Result<PathBuf> {
    let output = Command::new("osascript")
        .args([
            "-e",
            "POSIX path of (choose file with prompt \"Choose a Whisper GGML/GGUF model\")",
        ])
        .output()
        .context("open model file chooser")?;
    anyhow::ensure!(output.status.success(), "selection cancelled");
    let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let extension = path.extension().and_then(|value| value.to_str());
    anyhow::ensure!(
        matches!(extension, Some("bin" | "ggml" | "gguf")),
        "choose a .bin, .ggml, or .gguf Whisper model"
    );
    anyhow::ensure!(path.is_file(), "selected path is not a file");
    Ok(path)
}

pub struct Startup {
    pub archive_dir: String,
    pub warnings: Vec<String>,
    pub recording: Option<RecordingStartup>,
    pub settings: Option<SettingsStartup>,
}

pub struct RecordingStartup {
    pub app_paths: AppPaths,
    pub mix_settings: audio::MixSettings,
    pub config_path: PathBuf,
}

pub struct SettingsStartup {
    pub config: Config,
    pub config_path: PathBuf,
    pub fingerprint: ConfigFingerprint,
    pub model_dir: PathBuf,
}

struct SettingsContext {
    config: Config,
    config_path: PathBuf,
    fingerprint: ConfigFingerprint,
    model_dir: PathBuf,
}

#[derive(Clone)]
enum PickerType {
    SettingsLanguage,
    Model,
    RecordingLanguage,
    RetranscribeLanguage(Meeting),
}
struct PickerKind {
    kind: PickerType,
    modal: modals::picker::PickerModal,
}
impl PickerKind {
    fn new(
        kind: PickerType,
        title: &'static str,
        items: Vec<(String, String, String)>,
        selected: &str,
    ) -> Self {
        let items = items
            .into_iter()
            .map(|(value, label, detail)| modals::picker::PickerItem {
                value,
                label,
                detail,
            })
            .collect();
        Self {
            kind,
            modal: modals::picker::PickerModal::new(title, items, selected),
        }
    }
}

struct ActiveRecording {
    session: audio::RecordingSession,
    meeting_dir: PathBuf,
    language: String,
    diarization: RecordingDiarization,
}

struct AudioPreview {
    player: audio::PreviewPlayer,
    meeting_path: PathBuf,
    meeting_name: String,
    paused: bool,
}

impl AudioPreview {
    fn start(
        recording_path: PathBuf,
        meeting_path: PathBuf,
        meeting_name: String,
    ) -> Result<Self, audio::PreviewError> {
        let player = audio::PreviewPlayer::open(&recording_path)?;
        player.play()?;
        Ok(Self {
            player,
            meeting_path,
            meeting_name,
            paused: false,
        })
    }

    fn toggle(&mut self) -> Result<(), audio::PreviewError> {
        if self.paused {
            self.play()
        } else {
            self.player.pause();
            self.paused = true;
            Ok(())
        }
    }

    fn play(&mut self) -> Result<(), audio::PreviewError> {
        self.player.play()?;
        self.paused = false;
        Ok(())
    }

    fn stop(&self) {
        self.player.stop();
    }

    fn seek(&mut self, seconds: f64) {
        self.player.seek(seconds);
    }

    fn skip(&mut self, seconds: f64) {
        self.seek(self.position_seconds() + seconds);
    }

    fn position_seconds(&self) -> f64 {
        self.player.position_seconds()
    }

    fn duration_seconds(&self) -> f64 {
        self.player.duration_seconds()
    }

    fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    fn finished(&self) -> bool {
        !self.paused && self.position_seconds() >= self.duration_seconds() - 0.1
    }
}

impl Drop for AudioPreview {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordingDiarization {
    enabled: bool,
    expected_speakers: Option<usize>,
}

impl RecordingDiarization {
    fn from_config(config: Option<&Config>) -> Self {
        let Some(config) = config else {
            return Self {
                enabled: false,
                expected_speakers: None,
            };
        };
        let diarization = &config.diarization;
        let expected_speakers = (diarization.min_speakers > 0
            && diarization.min_speakers == diarization.max_speakers)
            .then_some(diarization.min_speakers);
        Self {
            enabled: diarization.enabled,
            expected_speakers,
        }
    }

    fn cycle_expected_speakers(&mut self) {
        if !self.enabled {
            return;
        }
        self.expected_speakers = match self.expected_speakers {
            None => Some(1),
            Some(count) if count < 6 => Some(count + 1),
            Some(_) => None,
        };
    }

    fn previous_expected_speakers(&mut self) {
        if !self.enabled {
            return;
        }
        self.expected_speakers = match self.expected_speakers {
            None => Some(6),
            Some(1) => None,
            Some(count) => Some(count - 1),
        };
    }

    fn label(self) -> &'static str {
        match self.expected_speakers {
            None => "Auto",
            Some(1) => "1 speaker",
            Some(2) => "2 speakers",
            Some(3) => "3 speakers",
            Some(4) => "4 speakers",
            Some(5) => "5 speakers",
            Some(_) => "6 speakers",
        }
    }
}

struct CompletedRecording {
    path: PathBuf,
    meeting_dir: PathBuf,
    language: String,
    diarization: RecordingDiarization,
}

struct InterruptedRecording {
    path: PathBuf,
    meeting_dir: PathBuf,
    language: String,
    diarization: RecordingDiarization,
    stopped_at: Instant,
}

struct ReconnectingRecording {
    interrupted: InterruptedRecording,
    failure: String,
    started_at: Instant,
    next_attempt: Instant,
    attempts: u32,
}

struct RetranscribeSpeakerPicker {
    path: PathBuf,
    language: String,
    diarization: RecordingDiarization,
}

struct Retranscription {
    meeting: Meeting,
    language: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AppAction {
    ToggleRecording,
    ContinueInterruptedRecording,
    ToggleMicrophoneMute,
    StopRecording,
    StopRecordingAndQuit,
    TranscribeMeeting {
        path: PathBuf,
        force: bool,
        language: Option<String>,
        diarization: Option<RecordingDiarization>,
    },
    OpenMeetingFolder(PathBuf),
    TrashMeetingFolder(PathBuf),
    SaveSettings,
}

fn microphone_mute_action(recording_active: bool) -> Option<AppAction> {
    recording_active.then_some(AppAction::ToggleMicrophoneMute)
}

pub async fn run(startup: Startup) -> anyhow::Result<()> {
    install_panic_hook();
    let (mut guard, mut terminal) = TerminalGuard::enter().context("enter terminal UI")?;
    let result = run_loop(&mut terminal, startup).await;
    let restore_result = guard.restore(&mut terminal).context("restore terminal UI");

    result.and(restore_result)
}

async fn run_loop(terminal: &mut AppTerminal, startup: Startup) -> anyhow::Result<()> {
    let mut app = App::new(startup);
    app.refresh_archive();
    let mut terminal_title = TerminalTitle::new();
    let mut input = InputThread::spawn().context("start terminal input reader")?;
    let mut pipeline = pipeline::Worker::spawn().context("start pipeline worker")?;
    let (pipeline_tx, mut pipeline_rx) = mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    while !app.should_quit {
        terminal_title.update(
            app.terminal_activity().is_some(),
            app.processing_spinner_frame,
        );
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            _ = tick.tick() => {
                if app.recording.is_some() || app.pipeline_active || app.reconnecting.is_some() {
                    app.processing_spinner_frame =
                        (app.processing_spinner_frame + 1) % PROCESSING_DOTS.len();
                }
                app.update_preview();
                if let Err(error) = app.pump_recording() {
                    let finalize_result = app.stop_recording();
                    let finalized = finalize_result
                        .as_ref()
                        .ok()
                        .and_then(|recording| recording.as_ref())
                        .map(|recording| InterruptedRecording {
                            path: recording.path.clone(),
                            meeting_dir: recording.meeting_dir.clone(),
                            language: recording.language.clone(),
                            diarization: recording.diarization,
                            stopped_at: Instant::now(),
                        });
                    app.error = Some(match finalize_result.err() {
                        Some(finalize) => format!("{error:#}; finalization also failed: {finalize:#}"),
                        None => match finalized {
                            Some(interrupted) => {
                                app.begin_reconnect(
                                    format!("{error:#}"),
                                    CompletedRecording {
                                        path: interrupted.path,
                                        meeting_dir: interrupted.meeting_dir,
                                        language: interrupted.language,
                                        diarization: interrupted.diarization,
                                    },
                                );
                                String::new()
                            }
                            None => format!("{error:#}"),
                        },
                    });
                    if app.reconnecting.is_some() {
                        app.error = None;
                    }
                }
                if app.reconnecting.is_some() {
                    app.retry_reconnect();
                }
            }
            maybe_event = input.recv() => {
                match maybe_event {
                    Some(UiEvent::Terminal(Event::Key(key))) if key.is_press() => {
                        match app.handle_key(key) {
                            Some(AppAction::ToggleRecording) if app.recording.is_some() => {
                                match app.stop_recording() {
                                    Ok(Some(recording)) => {
                                        launch_pipeline(
                                            &mut app,
                                            recording.path,
                                            false,
                                            Some(recording.language),
                                            Some(recording.diarization),
                                            &pipeline_tx,
                                        )
                                    }
                                    Ok(None) => {}
                                    Err(error) => app.error = Some(format!("{error:#}")),
                                }
                            }
                            Some(AppAction::ToggleRecording) => {
                                if let Err(error) = app.start_recording().await {
                                    app.error = Some(format!("{error:#}"));
                                }
                            }
                            Some(AppAction::ContinueInterruptedRecording) => {
                                if let Err(error) = app.continue_recording().await {
                                    app.error = Some(format!(
                                        "Could not continue the partial recording: {error:#}\n\n[c] Retry  [Enter/Esc] Dismiss"
                                    ));
                                }
                            }
                            Some(AppAction::ToggleMicrophoneMute) => {
                                if let Some(active) = &mut app.recording {
                                    active.session.toggle_microphone_muted();
                                }
                            }
                            Some(AppAction::StopRecording) => {
                                match app.stop_recording() {
                                    Ok(Some(recording)) => {
                                        launch_pipeline(
                                            &mut app,
                                            recording.path,
                                            false,
                                            Some(recording.language),
                                            Some(recording.diarization),
                                            &pipeline_tx,
                                        )
                                    }
                                    Ok(None) => {}
                                    Err(error) => app.error = Some(format!("{error:#}")),
                                }
                            }
                            Some(AppAction::StopRecordingAndQuit) => {
                                match app.stop_recording() {
                                    Ok(Some(_)) => app.should_quit = true,
                                    Ok(None) => app.should_quit = true,
                                    Err(error) => app.error = Some(format!("{error:#}")),
                                }
                            }
                            Some(AppAction::TranscribeMeeting {
                                path,
                                force,
                                language,
                                diarization,
                            }) => {
                                launch_pipeline(
                                    &mut app,
                                    path,
                                    force,
                                    language,
                                    diarization,
                                    &pipeline_tx,
                                );
                            }
                            Some(AppAction::OpenMeetingFolder(path)) => {
                                if let Err(error) = open_meeting_folder(&path) {
                                    app.error = Some(format!("{error:#}"));
                                }
                            }
                            Some(AppAction::TrashMeetingFolder(path)) => {
                                match trash_meeting_folder(&path) {
                                    Ok(()) => {
                                        app.refresh_archive();
                                        app.message = Some("Moved recording to Trash".to_owned());
                                    }
                                    Err(error) => app.error = Some(format!("{error:#}")),
                                }
                            }
                            Some(AppAction::SaveSettings) => app.save_settings(),
                            None => {}
                        }
                    }
                    Some(UiEvent::Terminal(Event::Mouse(mouse))) => {
                        let size = terminal.size()?;
                        app.handle_mouse(mouse, size.width, size.height);
                    }
                    Some(UiEvent::Terminal(_)) => {}
                    Some(UiEvent::InputError(error)) => app.error = Some(error),
                    None => {
                        input.restart().context("restart terminal input reader")?;
                        app.error = Some("Terminal input reader stopped and was restarted.".to_owned());
                    }
                }
            }
            maybe_event = pipeline.recv() => {
                match maybe_event {
                    Some(event) => app.handle_pipeline_event(event),
                    None => app.should_quit = true,
                }
            }
            maybe_event = pipeline_rx.recv() => {
                if let Some(event) = maybe_event {
                    app.handle_pipeline_event(event);
                }
            }
        }
    }

    if let Err(error) = app.stop_recording() {
        input.stop().context("stop terminal input reader")?;
        pipeline.shutdown().context("stop pipeline worker")?;
        return Err(error).context("finalize recording during terminal shutdown");
    }

    input.stop().context("stop terminal input reader")?;
    pipeline.shutdown().context("stop pipeline worker")?;
    Ok(())
}

fn launch_pipeline(
    app: &mut App,
    recording_path: PathBuf,
    force: bool,
    language: Option<String>,
    diarization: Option<RecordingDiarization>,
    events: &mpsc::UnboundedSender<PipelineEvent>,
) {
    let Some(context) = &app.recording_context else {
        app.error = Some("Recording is not configured".to_owned());
        return;
    };
    let output_dir = context.app_paths.output_dir().to_path_buf();
    let config_path = context.config_path.clone();
    app.handle_pipeline_event(PipelineEvent::WorkStarted);
    let events = events.clone();
    tokio::spawn(async move {
        let mut command = TokioCommand::new(std::env::current_exe().expect("current executable"));
        command
            .args(["resume"])
            .arg(recording_path)
            .args(["--config"])
            .arg(config_path)
            .args(["--output-dir"])
            .arg(output_dir);
        if force {
            command.arg("--force");
        }
        if let Some(language) = language {
            command.arg("--language").arg(language);
        }
        if let Some(diarization) = diarization {
            if diarization.enabled {
                command.arg("--speakers").arg(
                    diarization
                        .expected_speakers
                        .map_or("auto".to_owned(), |count| count.to_string()),
                );
            } else {
                command.arg("--no-diarize");
            }
        }
        let result = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match result {
            Ok(child) => child,
            Err(error) => {
                let _ = events.send(PipelineEvent::WorkFailed(error.to_string()));
                return;
            }
        };
        let mut last_stage = "Processing".to_owned();
        let mut failure_detail = None;
        if let Some(stderr) = child.stderr.take() {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(detail) = line.strip_prefix("Transcription failed: ") {
                    failure_detail = Some(detail.to_owned());
                }
                let stage = pipeline_stage_from_stderr(&line);
                if let Some(stage) = stage {
                    last_stage = stage.to_owned();
                    let _ = events.send(PipelineEvent::Stage(stage.to_owned()));
                }
            }
        }
        match child.wait().await {
            Ok(status) if status.success() => {
                let _ = events.send(PipelineEvent::WorkCompleted);
            }
            Ok(status) => {
                let failure =
                    failure_detail.unwrap_or_else(|| format!("pipeline exited with {status}"));
                let _ = events.send(PipelineEvent::WorkFailed(format!(
                    "{last_stage} failed: {failure}"
                )));
            }
            Err(error) => {
                let _ = events.send(PipelineEvent::WorkFailed(error.to_string()));
            }
        }
    });
}

fn pipeline_stage_from_stderr(line: &str) -> Option<&str> {
    if line.starts_with("Preparing transcription") {
        Some("Preparing transcription")
    } else if line.starts_with("Reading recording") {
        Some("Reading recording")
    } else if line.starts_with("Loading transcriber") {
        Some("Loading transcriber")
    } else if line.starts_with("Transcribing using ") {
        Some(line)
    } else if line.starts_with("Transcribing ") {
        Some("Transcribing")
    } else if line.starts_with("Preparing diarization") {
        Some("Preparing diarization")
    } else if line.starts_with("Diarizing ") {
        Some("Diarizing")
    } else if line.starts_with("Saving transcript") {
        Some("Saving transcript")
    } else {
        None
    }
}

fn open_meeting_folder(path: &Path) -> anyhow::Result<()> {
    Command::new("open")
        .arg(path)
        .spawn()
        .with_context(|| format!("could not open {} in Finder", path.display()))?;
    Ok(())
}

fn trash_meeting_folder(path: &Path) -> anyhow::Result<()> {
    let status = finder_trash_command(path)
        .status()
        .with_context(|| format!("could not ask Finder to trash {}", path.display()))?;
    if !status.success() {
        anyhow::bail!("Finder could not move {} to Trash", path.display());
    }
    Ok(())
}

fn finder_trash_command(path: &Path) -> Command {
    let script = "on run argv\n  set targetItem to POSIX file (item 1 of argv) as alias\n  tell application \"Finder\" to delete targetItem\nend run";
    let mut command = Command::new("osascript");
    command
        .args(["-e", script, "--"])
        .arg(path)
        .stdout(Stdio::null());
    command
}

type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<(Self, AppTerminal)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_terminal();
                return Err(error);
            }
        };
        if let Err(error) = terminal.hide_cursor() {
            let _ = restore_terminal();
            return Err(error);
        }
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        Ok((Self { active: true }, terminal))
    }

    fn restore(&mut self, terminal: &mut AppTerminal) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        let cursor_result = terminal.show_cursor();
        let restore_result = restore_active_terminal();
        let result = cursor_result.and(restore_result);
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = restore_active_terminal();
        }
    }
}

/// Keeps terminal-window activity visible without adding noise to the TUI.
///
/// OSC title updates are harmlessly ignored by terminals that do not support
/// them. For unknown terminals we deliberately use a static active title
/// rather than sending rapid animation updates.
struct TerminalTitle {
    animated: bool,
    current: String,
}

impl TerminalTitle {
    fn new() -> Self {
        Self {
            animated: terminal_supports_title_animation(env::var("TERM_PROGRAM").ok().as_deref()),
            current: String::new(),
        }
    }

    fn update(&mut self, active: bool, frame: usize) {
        let title = terminal_title(active, frame, self.animated);
        if title != self.current {
            let _ = write_terminal_title(&title);
            self.current = title;
        }
    }
}

impl Drop for TerminalTitle {
    fn drop(&mut self) {
        let _ = write_terminal_title("SOSUS");
    }
}

fn terminal_supports_title_animation(term_program: Option<&str>) -> bool {
    matches!(
        term_program.map(str::to_ascii_lowercase).as_deref(),
        Some("iterm.app" | "ghostty" | "wezterm" | "apple_terminal")
    )
}

fn terminal_title(active: bool, frame: usize, animated: bool) -> String {
    match (active, animated) {
        (true, true) => format!("{} SOSUS", PROCESSING_DOTS[frame % PROCESSING_DOTS.len()]),
        (true, false) => "• SOSUS".to_owned(),
        (false, _) => "SOSUS".to_owned(),
    }
}

fn write_terminal_title(title: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "\x1b]2;{title}\x07")?;
    stdout.flush()
}

fn restore_active_terminal() -> io::Result<()> {
    if !TERMINAL_ACTIVE.swap(false, Ordering::AcqRel) {
        return Ok(());
    }

    let result = restore_terminal();
    if result.is_err() {
        TERMINAL_ACTIVE.store(true, Ordering::Release);
    }
    result
}

fn restore_terminal() -> io::Result<()> {
    let cursor_result = execute!(io::stdout(), Show);
    let screen_result = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let raw_mode_result = disable_raw_mode();
    cursor_result.and(screen_result).and(raw_mode_result)
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore_active_terminal();
        previous(info);
    }));
}

struct InputThread {
    stop: Arc<AtomicBool>,
    events: mpsc::Receiver<UiEvent>,
    join: Option<JoinHandle<()>>,
}

impl InputThread {
    fn spawn() -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (event_tx, events) = mpsc::channel(128);
        let join = thread::Builder::new()
            .name("sosus-terminal-input".to_owned())
            .spawn(move || input_loop(&thread_stop, &event_tx))?;

        Ok(Self {
            stop,
            events,
            join: Some(join),
        })
    }

    async fn recv(&mut self) -> Option<UiEvent> {
        self.events.recv().await
    }

    fn stop(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| io::Error::other("terminal input reader panicked"))?;
        }
        Ok(())
    }

    fn restart(&mut self) -> io::Result<()> {
        self.stop()?;
        *self = Self::spawn()?;
        Ok(())
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn input_loop(stop: &AtomicBool, events: &mpsc::Sender<UiEvent>) {
    while !stop.load(Ordering::Acquire) {
        match event::poll(Duration::from_millis(25)) {
            Ok(true) => match event::read() {
                Ok(event) => {
                    if !send_input_event(events, UiEvent::Terminal(event)) {
                        break;
                    }
                }
                Err(error) => {
                    if !send_input_event(events, UiEvent::InputError(error.to_string())) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            },
            Ok(false) => {}
            Err(error) => {
                if !send_input_event(events, UiEvent::InputError(error.to_string())) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn send_input_event(events: &mpsc::Sender<UiEvent>, event: UiEvent) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
            if matches!(
                event,
                UiEvent::Terminal(
                    Event::Mouse(_) | Event::Resize(_, _) | Event::FocusGained | Event::FocusLost
                )
            ) {
                true
            } else {
                events.blocking_send(event).is_ok()
            }
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect) {
    let message = format!(
        "Terminal too small: {}x{}. sosus requires at least {MINIMUM_WIDTH}x{MINIMUM_HEIGHT}.",
        area.width, area.height
    );
    frame.render_widget(
        Paragraph::new(message)
            .style(theme::warning_text())
            .block(Block::default().borders(Borders::ALL).title("sosus")),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let content = Text::from(vec![
        Line::from("Tab / Shift+Tab  Move focus"),
        Line::from("F2               Settings"),
        Line::from("r                Start / stop recording"),
        Line::from("c                Continue an interrupted recording"),
        Line::from("m                Mute / unmute microphone"),
        Line::from("s                Change expected speakers while recording"),
        Line::from("l                Choose transcription language"),
        Line::from("t                Process / re-transcribe selected recording"),
        Line::from("p / x            Play or pause / stop selected recording"),
        Line::from("← / →            Skip 5s (Shift: 30s) while previewing"),
        Line::from("Enter             Play selected transcript segment"),
        Line::from("o                Open selected recording in Finder"),
        Line::from("d / D            Delete with confirmation / immediately"),
        Line::from("?                Toggle help"),
        Line::from("q                Stop recording and quit"),
        Line::from("Ctrl+C           Stop recording, otherwise quit"),
        Line::from("Esc              Close overlay"),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Help")
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_settings(frame: &mut Frame<'_>, settings: &modals::settings::SettingsModal, area: Rect) {
    let mut content = vec![
        Line::styled(
            "Changes apply to the next recording or transcription.",
            theme::secondary_text(),
        ),
        Line::from(""),
    ];
    for (label, value, selected) in settings.rows() {
        let line = Line::from(format!("{label:<18} {value}"));
        content.push(if selected {
            line.style(theme::selected_row())
        } else {
            line.style(theme::primary_text())
        });
    }
    content.extend([
        Line::from(""),
        Line::styled(
            "[s] Save changes · Enter choose language/model · Esc cancel",
            theme::secondary_text(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Settings")
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_picker(frame: &mut Frame<'_>, picker: &modals::picker::PickerModal, area: Rect) {
    let visible = picker.visible();
    let mut content = vec![
        Line::styled(
            format!("Filter: {}", picker.filter()),
            theme::secondary_text(),
        ),
        Line::from(""),
    ];
    for (index, item) in visible.into_iter().take(14).enumerate() {
        let line = Line::from(item.label.clone());
        content.push(if index == picker.selected_index() {
            line.style(theme::selected_row())
        } else {
            line.style(theme::primary_text())
        });
        if !item.detail.is_empty() {
            content.push(Line::styled(
                format!("  {}", item.detail),
                theme::secondary_text(),
            ));
        }
    }
    if content.len() == 2 {
        content.push(Line::styled("No matches", theme::secondary_text()));
    }
    content.extend([
        Line::from(""),
        Line::styled(
            "Type to filter · Enter choose · Esc back",
            theme::secondary_text(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(picker.title())
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_notice(frame: &mut Frame<'_>, title: &str, message: &str, area: Rect) {
    let content = Text::from(vec![
        Line::styled(message, theme::warning_text()),
        Line::from(""),
        Line::styled("Press Esc or Enter to dismiss.", theme::secondary_text()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_quit_processing_confirmation(frame: &mut Frame<'_>, area: Rect) {
    let content = Text::from(vec![
        Line::styled("Transcription is still processing.", theme::warning_text()),
        Line::from(""),
        Line::styled("Quit while it is still running?", theme::primary_text()),
        Line::from(""),
        Line::styled(
            "Enter to quit · Esc or q to continue",
            theme::secondary_text(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Processing")
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_delete_confirmation(frame: &mut Frame<'_>, meeting_name: &str, area: Rect) {
    let content = Text::from(vec![
        Line::styled("Move this recording to Trash?", theme::warning_text()),
        Line::from(""),
        Line::styled(meeting_name, theme::primary_text()),
        Line::from(""),
        Line::styled(
            "Enter to delete · Esc or d to cancel",
            theme::secondary_text(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Delete recording")
        .style(theme::overlay())
        .padding(Padding::uniform(1));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn retranscription_model(config: Option<&Config>) -> String {
    let Some(config) = config else {
        return "the configured transcription model".to_owned();
    };
    let backend = config.transcription.backend.to_string();
    let model = if config.transcription.model.is_empty() {
        match backend.as_str() {
            "parakeet" => "parakeet-tdt-0.6b-v3-int8",
            "whisper" => "whisper-base",
            _ => "default model",
        }
    } else {
        &config.transcription.model
    };
    format!("{backend} · {model}")
}

fn language_label(language: &str) -> String {
    if language.is_empty() {
        "Auto-detect".to_owned()
    } else {
        modals::settings::language_name(language)
    }
}

fn render_retranscribe_confirmation(
    frame: &mut Frame<'_>,
    meeting_name: &str,
    model: &str,
    language: &str,
    area: Rect,
) {
    let content = Text::from(vec![
        Line::styled(
            format!("Re-transcribe {meeting_name}?"),
            theme::warning_text(),
        ),
        Line::from(""),
        Line::styled(
            "The current transcript stays until the replacement succeeds.",
            theme::secondary_text(),
        ),
        Line::from(""),
        Line::from(vec![
            Span::styled("Will use: ", theme::secondary_text()),
            Span::styled(model, theme::primary_text()),
        ]),
        Line::from(vec![
            Span::styled("Language: ", theme::secondary_text()),
            Span::styled(language, theme::primary_text()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("[Enter]", theme::primary_text()),
            Span::raw(" Continue   "),
            Span::styled("[Esc]", theme::primary_text()),
            Span::raw(" Cancel"),
        ]),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Re-transcribe")
        .style(theme::overlay())
        .padding(Padding::uniform(2));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_retranscribe_speaker_picker(
    frame: &mut Frame<'_>,
    picker: &RetranscribeSpeakerPicker,
    area: Rect,
) {
    let options = [
        (None, "Auto"),
        (Some(1), "1"),
        (Some(2), "2"),
        (Some(3), "3"),
        (Some(4), "4"),
        (Some(5), "5"),
        (Some(6), "6"),
    ];
    let selected = picker.diarization.expected_speakers;
    let mut choices = Vec::new();
    for (value, option) in options {
        choices.push(Span::styled(
            format!("[{option}]"),
            if value == selected {
                theme::selected_row()
            } else {
                theme::secondary_text()
            },
        ));
        choices.push(Span::raw(" "));
    }
    let lines = vec![
        Line::styled("Expected speakers", theme::primary_text()),
        Line::from(""),
        Line::from(choices),
        Line::from(""),
        Line::styled(
            "[↑/↓/0–6] Select   [Enter] Start   [Esc] Cancel",
            theme::secondary_text(),
        ),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Re-transcribe")
        .style(theme::overlay())
        .padding(Padding::uniform(2));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = if let Some(active) = &app.recording {
        let elapsed = active.session.elapsed_seconds() as u64;
        let microphone_status = if active.session.microphone_muted() {
            "·  MIC MUTED  ·  m to unmute"
        } else {
            "·  m to mute"
        };
        let speaker_status = if active.diarization.enabled {
            format!(" · {} · s to change", active.diarization.label())
        } else {
            String::new()
        };
        Line::from(vec![
            Span::styled(" ●", theme::recording_indicator()),
            Span::raw(format!(
                " Recording  {:02}:{:02}  ·  r to stop {microphone_status}{speaker_status}",
                elapsed / 60,
                elapsed % 60
            )),
        ])
    } else if let Some(stage) = &app.pipeline_status {
        if app.pipeline_active {
            Line::from(vec![
                Span::styled(
                    format!(" {}", PROCESSING_DOTS[app.processing_spinner_frame]),
                    theme::meter_signal(),
                ),
                Span::raw(format!(" {stage}")),
            ])
        } else {
            Line::from(format!(" {stage}"))
        }
    } else if let Some(reconnecting) = &app.reconnecting {
        let elapsed = reconnecting.started_at.elapsed().as_secs();
        Line::from(vec![
            Span::styled(
                format!(" {}", PROCESSING_DOTS[app.processing_spinner_frame]),
                theme::warning_text(),
            ),
            Span::raw(format!(" Reconnecting audio… {elapsed}s")),
        ])
    } else if let Some(message) = &app.message {
        Line::from(format!(" {message}"))
    } else {
        Line::from(" r to record")
    };
    let status_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
    frame.render_widget(
        Paragraph::new(status).style(theme::status_bar()),
        status_area,
    );
}

fn render_preview_bar(frame: &mut Frame<'_>, area: Rect, preview: &AudioPreview) {
    let icon = if preview.is_playing() { "▶" } else { "Ⅱ" };
    let position = format_playback_time(preview.position_seconds());
    let duration = format_playback_time(preview.duration_seconds());
    let fixed_width = preview.meeting_name.chars().count() + position.len() + duration.len() + 14;
    let meter_width = usize::from(area.width)
        .saturating_sub(fixed_width)
        .clamp(8, 48);
    let filled = (preview.position_seconds() / preview.duration_seconds().max(0.1)
        * meter_width as f64)
        .round() as usize;
    let meter = format!(
        "{}{}",
        "▰".repeat(filled.min(meter_width)),
        "▱".repeat(meter_width - filled.min(meter_width))
    );
    let line = Line::from(vec![
        Span::styled(format!(" {icon} "), theme::meter_signal()),
        Span::styled(
            format!("{position} / {duration}  "),
            theme::secondary_text(),
        ),
        Span::styled(meter, theme::meter_signal()),
        Span::styled(
            format!("  {}", preview.meeting_name),
            theme::secondary_text(),
        ),
    ]);
    frame.render_widget(Paragraph::new(line).style(theme::status_bar()), area);
}

fn format_playback_time(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn render_header_bar(frame: &mut Frame<'_>, area: Rect) {
    let header_area = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(Paragraph::new("").style(theme::status_bar()), header_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" SOSUS", theme::primary_text().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" v{}", env!("CARGO_PKG_VERSION")),
                theme::secondary_text(),
            ),
        ]))
        .style(theme::status_bar()),
        header_area,
    );
    frame.render_widget(
        Paragraph::new(" ? Help ")
            .alignment(ratatui::layout::Alignment::Right)
            .style(theme::status_bar()),
        header_area,
    );
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - height_percent) / 2),
        Constraint::Percentage(height_percent),
        Constraint::Percentage((100 - height_percent) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - width_percent) / 2),
        Constraint::Percentage(width_percent),
        Constraint::Percentage((100 - width_percent) / 2),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn app() -> App {
        App::new(Startup {
            archive_dir: "/tmp/sosus-test-recordings".to_owned(),
            warnings: Vec::new(),
            recording: None,
            settings: None,
        })
    }

    #[test]
    fn focus_cycles_in_both_directions() {
        let mut app = app();
        let _ = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Transcript);
        let _ = app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Meetings);
        let _ = app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Recording);
    }

    #[test]
    fn transcript_navigation_selects_a_segment_and_keeps_it_visible() {
        let mut app = app();
        app.transcript = vec![
            Segment {
                start_s: 0.0,
                end_s: 1.0,
                speaker: None,
                text: "first".to_owned(),
            },
            Segment {
                start_s: 1.0,
                end_s: 2.0,
                speaker: None,
                text: "second".to_owned(),
            },
        ];

        app.move_transcript_selection(1);

        assert_eq!(app.selected_transcript_segment, 1);
        assert_eq!(app.transcript_scroll, 3);
    }

    #[test]
    fn playback_time_is_compact_and_human_readable() {
        assert_eq!(format_playback_time(4.0), "00:04");
        assert_eq!(format_playback_time(125.0), "02:05");
        assert_eq!(format_playback_time(3_726.0), "1:02:06");
    }

    #[test]
    fn dragging_the_divider_resizes_the_sidebar_with_safe_bounds() {
        let mut app = app();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: DEFAULT_SIDEBAR_WIDTH,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert!(app.resizing_sidebar);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 50,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.sidebar_width, 50);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.sidebar_width, MINIMUM_SIDEBAR_WIDTH);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert!(!app.resizing_sidebar);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 60,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.sidebar_width, MINIMUM_SIDEBAR_WIDTH);
    }

    #[test]
    fn mouse_click_selects_a_meeting_and_wheel_scrolls_its_transcript() {
        let mut app = app();
        app.meetings = vec![
            Meeting {
                path: PathBuf::from("/tmp/one"),
                name: "one".to_owned(),
                duration_seconds: None,
                transcript: vec![Segment {
                    start_s: 0.0,
                    end_s: 1.0,
                    speaker: None,
                    text: "one".to_owned(),
                }],
            },
            Meeting {
                path: PathBuf::from("/tmp/two"),
                name: "two".to_owned(),
                duration_seconds: None,
                transcript: vec![Segment {
                    start_s: 0.0,
                    end_s: 1.0,
                    speaker: None,
                    text: "two".to_owned(),
                }],
            },
        ];
        app.transcript_scroll = 9;
        app.focus = Focus::Transcript;

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.selected_meeting, 1);
        assert_eq!(app.transcript[0].text, "two");
        assert_eq!(app.transcript_scroll, 0);
        assert_eq!(app.focus, Focus::Meetings);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 40,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.transcript_scroll, 3);
        assert_eq!(app.focus, Focus::Transcript);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 40,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.transcript_scroll, 0);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 22,
                modifiers: KeyModifiers::NONE,
            },
            100,
            24,
        );
        assert_eq!(app.selected_meeting, 1);
    }

    #[test]
    fn global_quit_keys_request_clean_shutdown() {
        for key in [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut app = app();
            let _ = app.handle_key(key);
            assert!(app.should_quit);
        }
    }

    #[test]
    fn quitting_during_processing_requires_confirmation() {
        let mut app = app();
        app.pipeline_active = true;

        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.confirm_quit_processing);
        assert!(!app.should_quit);

        let _ = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.confirm_quit_processing);

        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn recording_key_requests_a_toggle() {
        let mut app = app();
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(AppAction::ToggleRecording)
        );
    }

    #[test]
    fn continue_key_is_available_only_for_an_interrupted_recording_dialog() {
        let mut app = app();
        app.error = Some("system-audio capture failed".to_owned());
        app.interrupted_recording = Some(InterruptedRecording {
            path: PathBuf::from("/tmp/meeting/recording.wav"),
            meeting_dir: PathBuf::from("/tmp/meeting"),
            language: String::new(),
            diarization: RecordingDiarization {
                enabled: false,
                expected_speakers: None,
            },
            stopped_at: Instant::now(),
        });

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)),
            Some(AppAction::ContinueInterruptedRecording)
        );
    }

    #[test]
    fn microphone_mute_shortcut_only_dispatches_while_recording() {
        assert_eq!(microphone_mute_action(false), None);
        assert_eq!(
            microphone_mute_action(true),
            Some(AppAction::ToggleMicrophoneMute)
        );
    }

    #[test]
    fn recording_speaker_count_cycles_from_auto_through_six() {
        let mut diarization = RecordingDiarization {
            enabled: true,
            expected_speakers: None,
        };
        for expected in 1..=6 {
            diarization.cycle_expected_speakers();
            assert_eq!(diarization.expected_speakers, Some(expected));
        }
        diarization.cycle_expected_speakers();
        assert_eq!(diarization.expected_speakers, None);
    }

    #[test]
    fn recording_speaker_count_is_inert_when_diarization_is_disabled() {
        let mut diarization = RecordingDiarization {
            enabled: false,
            expected_speakers: Some(2),
        };
        diarization.cycle_expected_speakers();
        assert_eq!(diarization.expected_speakers, Some(2));
    }

    #[test]
    fn open_key_requests_the_selected_meeting_folder() {
        let mut app = app();
        app.meetings = vec![Meeting {
            path: PathBuf::from("/tmp/meeting"),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        }];

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE)),
            Some(AppAction::OpenMeetingFolder(PathBuf::from("/tmp/meeting")))
        );
    }

    #[test]
    fn finder_trash_command_passes_the_path_as_an_apple_script_argument() {
        let command = finder_trash_command(Path::new("/tmp/meeting"));
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args[0], "-e");
        assert!(args[1].contains("as alias"));
        assert_eq!(args[2], "--");
        assert_eq!(args[3], "/tmp/meeting");
    }

    #[test]
    fn transcribe_key_requests_the_selected_meeting_when_idle() {
        let mut app = app();
        app.meetings = vec![Meeting {
            path: PathBuf::from("/tmp/meeting"),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        }];

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            Some(AppAction::TranscribeMeeting {
                path: PathBuf::from("/tmp/meeting"),
                force: false,
                language: None,
                diarization: None,
            })
        );

        app.pipeline_active = true;
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn language_key_for_a_meeting_opens_a_transient_picker_then_confirms() {
        let mut app = app();
        let config_path = PathBuf::from("/tmp/sosus-language-picker-config.toml");
        app.settings_context = Some(SettingsContext {
            config: Config::default(),
            fingerprint: config::fingerprint(&config_path).unwrap(),
            config_path,
            model_dir: PathBuf::from("/tmp/sosus-language-picker-models"),
        });
        app.meetings = vec![Meeting {
            path: PathBuf::from("/tmp/meeting"),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        }];

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            None
        );
        assert!(matches!(
            app.picker.as_ref().map(|picker| &picker.kind),
            Some(PickerType::RetranscribeLanguage(_))
        ));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        let confirmation = app
            .retranscribe_confirmation
            .as_ref()
            .expect("language choice should lead to confirmation");
        assert_eq!(confirmation.meeting.name, "meeting");
        assert!(confirmation.language.is_empty());
    }

    #[test]
    fn retranscribing_confirms_then_selects_the_expected_speaker_count() {
        let root = std::env::temp_dir().join(format!(
            "sosus-retranscribe-test-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("transcript.md"), "# Transcript\n").unwrap();
        let mut app = app();
        let config_path = root.join("config.toml");
        app.settings_context = Some(SettingsContext {
            config: Config::default(),
            fingerprint: config::fingerprint(&config_path).unwrap(),
            config_path,
            model_dir: root.join("models"),
        });
        app.meetings = vec![Meeting {
            path: root.clone(),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        }];

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            None
        );
        assert!(app.retranscribe_confirmation.is_some());
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            app.retranscribe_speakers
                .as_ref()
                .and_then(|picker| picker.diarization.expected_speakers),
            Some(2)
        );
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(AppAction::TranscribeMeeting {
                path: root.clone(),
                force: true,
                language: Some(String::new()),
                diarization: Some(RecordingDiarization {
                    enabled: true,
                    expected_speakers: Some(3),
                }),
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retranscription_model_names_the_selected_whisper_model() {
        let mut config = Config::default();
        config.transcription.backend = crate::asr::TranscriptionBackend::Whisper;
        config.transcription.model = "whisper-small".to_owned();
        assert_eq!(
            retranscription_model(Some(&config)),
            "whisper · whisper-small"
        );
    }

    #[test]
    fn pipeline_status_preserves_the_transcription_model() {
        assert_eq!(
            pipeline_stage_from_stderr(
                "Transcribing using whisper · whisper-large-v3-turbo (58m 00s of audio)..."
            ),
            Some("Transcribing using whisper · whisper-large-v3-turbo (58m 00s of audio)...")
        );
    }

    #[test]
    fn retranscribing_skips_speaker_selection_when_diarization_is_disabled() {
        let root = std::env::temp_dir().join(format!(
            "sosus-retranscribe-no-diarization-test-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("transcript.md"), "# Transcript\n").unwrap();
        let mut app = app();
        let mut config = Config::default();
        config.diarization.enabled = false;
        let config_path = root.join("config.toml");
        app.settings_context = Some(SettingsContext {
            config,
            fingerprint: config::fingerprint(&config_path).unwrap(),
            config_path,
            model_dir: root.join("models"),
        });
        app.meetings = vec![Meeting {
            path: root.clone(),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        }];

        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(AppAction::TranscribeMeeting {
                path: root.clone(),
                force: true,
                language: Some(String::new()),
                diarization: Some(RecordingDiarization {
                    enabled: false,
                    expected_speakers: Some(2),
                }),
            })
        );
        assert!(app.retranscribe_speakers.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_key_confirms_but_uppercase_deletes_immediately() {
        let meeting = Meeting {
            path: PathBuf::from("/tmp/meeting"),
            name: "meeting".to_owned(),
            duration_seconds: None,
            transcript: Vec::new(),
        };
        let mut confirmed = app();
        confirmed.meetings = vec![meeting.clone()];

        assert_eq!(
            confirmed.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            confirmed
                .delete_confirmation
                .as_ref()
                .map(|item| &item.path),
            Some(&meeting.path)
        );
        assert_eq!(
            confirmed.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(AppAction::TrashMeetingFolder(meeting.path.clone()))
        );

        let mut immediate = app();
        immediate.meetings = vec![meeting.clone()];
        assert_eq!(
            immediate.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)),
            Some(AppAction::TrashMeetingFolder(meeting.path))
        );
    }

    #[test]
    fn pipeline_status_keeps_the_current_user_facing_stage() {
        let mut app = app();
        app.handle_pipeline_event(PipelineEvent::WorkStarted);
        assert!(app.pipeline_active);
        assert_eq!(app.pipeline_status.as_deref(), Some("Starting"));

        app.handle_pipeline_event(PipelineEvent::Stage("Transcribing".to_owned()));
        app.handle_pipeline_event(PipelineEvent::WorkProgress {
            completed: 1,
            total: 3,
        });
        assert_eq!(app.pipeline_status.as_deref(), Some("Transcribing"));

        app.handle_pipeline_event(PipelineEvent::WorkCompleted);
        assert!(!app.pipeline_active);
        assert!(app.pipeline_status.is_none());
    }

    #[test]
    fn terminal_title_uses_braille_only_for_known_animated_terminals() {
        assert!(terminal_supports_title_animation(Some("ghostty")));
        assert!(!terminal_supports_title_animation(Some("xterm")));
        assert_eq!(terminal_title(true, 0, true), "⠋ SOSUS");
        assert_eq!(terminal_title(true, 0, false), "• SOSUS");
        assert_eq!(terminal_title(false, 0, true), "SOSUS");
    }

    #[test]
    fn meetings_navigation_is_bounded() {
        let mut app = app();
        app.meetings = vec![
            Meeting {
                path: PathBuf::from("/tmp/one"),
                name: "one".to_owned(),
                duration_seconds: None,
                transcript: Vec::new(),
            },
            Meeting {
                path: PathBuf::from("/tmp/two"),
                name: "two".to_owned(),
                duration_seconds: None,
                transcript: Vec::new(),
            },
        ];
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.selected_meeting, 1);
        let _ = app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_meeting, 1);
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.selected_meeting, 0);
    }

    #[test]
    fn config_warnings_are_preserved_and_dismissed_in_order() {
        let mut app = App::new(Startup {
            archive_dir: "/tmp/sosus-test-recordings".to_owned(),
            warnings: vec!["first".to_owned(), "second".to_owned()],
            recording: None,
            settings: None,
        });
        assert_eq!(app.warnings.front().map(String::as_str), Some("first"));

        let _ = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.warnings.front().map(String::as_str), Some("second"));

        let _ = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.warnings.is_empty());
    }

    #[test]
    fn minimum_size_renders_clear_message() {
        let backend = TestBackend::new(79, 23);
        let mut terminal = Terminal::new(backend).expect("test terminal should construct");
        terminal
            .draw(|frame| app().render(frame))
            .expect("render should succeed");
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Terminal too small"));
    }

    #[test]
    fn retranscribe_speaker_picker_fits_all_choices_and_controls_in_a_small_modal() {
        let backend = TestBackend::new(56, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal should construct");
        let picker = RetranscribeSpeakerPicker {
            path: PathBuf::from("/tmp/meeting"),
            language: String::new(),
            diarization: RecordingDiarization {
                enabled: true,
                expected_speakers: Some(2),
            },
        };
        terminal
            .draw(|frame| {
                render_retranscribe_speaker_picker(frame, &picker, Rect::new(0, 0, 56, 12));
            })
            .expect("render should succeed");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for choice in ["[Auto]", "[1]", "[2]", "[3]", "[4]", "[5]", "[6]"] {
            assert!(rendered.contains(choice), "missing {choice}");
        }
        assert!(rendered.contains("[Enter] Start"));
        assert!(rendered.contains("[Esc] Cancel"));
    }

    #[test]
    fn normal_size_renders_core_panes() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal should construct");
        terminal
            .draw(|frame| app().render(frame))
            .expect("render should succeed");
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for content in ["SOSUS", "? Help", "No recordings", "Choose a recording"] {
            assert!(rendered.contains(content), "missing content: {content}");
        }
    }

    #[test]
    fn resize_recovers_from_too_small_layout() {
        let backend = TestBackend::new(79, 23);
        let mut terminal = Terminal::new(backend).expect("test terminal should construct");
        terminal
            .draw(|frame| app().render(frame))
            .expect("small render should succeed");

        terminal.backend_mut().resize(80, 24);
        terminal
            .resize(Rect::new(0, 0, 80, 24))
            .expect("test terminal should resize");
        terminal
            .draw(|frame| app().render(frame))
            .expect("normal render should succeed");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("Terminal too small"));
    }
}
