//! Terminal UI state and event loop.

mod modals;
mod panes;
mod theme;
mod widgets;

use std::{
    collections::VecDeque,
    io::{self, Stdout},
    panic,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
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
    should_quit: bool,
    message: Option<String>,
    warnings: VecDeque<String>,
    recording_context: Option<RecordingStartup>,
    recording: Option<ActiveRecording>,
    last_recording: Option<String>,
    pipeline_status: Option<String>,
    pipeline_active: bool,
    processing_spinner_frame: usize,
    meetings: Vec<Meeting>,
    selected_meeting: usize,
    transcript: Vec<Segment>,
    transcript_scroll: u16,
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
            should_quit: false,
            message: None,
            warnings: startup.warnings.into(),
            recording_context: startup.recording,
            recording: None,
            last_recording: None,
            pipeline_status: None,
            pipeline_active: false,
            processing_spinner_frame: 0,
            meetings: Vec::new(),
            selected_meeting: 0,
            transcript: Vec::new(),
            transcript_scroll: 0,
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
            if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                self.transcript = meeting.transcript.clone();
                self.transcript_scroll = 0;
            } else {
                self.transcript.clear();
                self.transcript_scroll = 0;
            }
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
        self.transcript = meeting.transcript.clone();
        self.transcript_scroll = 0;
    }

    fn meeting_row_at(&self, row: u16, terminal_height: u16) -> Option<usize> {
        let recording_height = u16::from(self.recording.is_some()) * 4;
        let pane_height = terminal_height.saturating_sub(2 + recording_height);
        let index = row.checked_sub(2)? as usize;
        (index < pane_height.saturating_sub(2) as usize && index < self.meetings.len())
            .then_some(index)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<AppAction> {
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
                (KeyCode::Esc | KeyCode::Enter, _) => {
                    self.error = None;
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
                    if let Some(settings) = &mut self.settings {
                        match picker.kind {
                            PickerType::Language => settings.set_language(value),
                            PickerType::Model if value == "__custom__" => {
                                match choose_custom_model() {
                                    Ok(path) => settings.set_model(path.display().to_string()),
                                    Err(error) => {
                                        self.error =
                                            Some(format!("Could not import custom model: {error}"))
                                    }
                                }
                            }
                            PickerType::Model => settings.set_model(value),
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

        match (key.code, key.modifiers) {
            (KeyCode::Up | KeyCode::Char('k'), _) if self.focus == Focus::Meetings => {
                self.move_selection(-1);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) if self.focus == Focus::Meetings => {
                self.move_selection(1);
            }
            (KeyCode::Up | KeyCode::Char('k'), _) if self.focus == Focus::Transcript => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(1);
            }
            (KeyCode::Down | KeyCode::Char('j'), _) if self.focus == Focus::Transcript => {
                self.transcript_scroll = self.transcript_scroll.saturating_add(1);
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
            (KeyCode::Char('r'), _) => return Some(AppAction::ToggleRecording),
            (KeyCode::Char('m'), _) => return microphone_mute_action(self.recording.is_some()),
            (KeyCode::Char('t'), _) if self.recording.is_none() && !self.pipeline_active => {
                if let Some(meeting) = self.meetings.get(self.selected_meeting) {
                    return Some(AppAction::TranscribeMeeting(meeting.path.clone()));
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
            PickerType::Language,
            "Language",
            settings
                .language_options()
                .into_iter()
                .map(|(value, label)| (value, label, String::new()))
                .collect(),
            &settings.config().transcription.language,
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
        self.recording = Some(ActiveRecording {
            session,
            meeting_dir,
        });
        self.input_levels = Some((0.0, 0.0));
        self.message = None;
        Ok(())
    }

    fn stop_recording(&mut self) -> anyhow::Result<Option<PathBuf>> {
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
        Ok(Some(outcome.path))
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

        let (columns, recording_area) = if self.recording.is_some() {
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
            (columns, Some(rows[1]))
        } else {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(sidebar_width),
                    Constraint::Min(MINIMUM_TRANSCRIPT_WIDTH),
                ])
                .split(content_area);
            (columns, None)
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
            render_picker(frame, &picker.modal, centered_rect(58, 72, area));
        } else if let Some(settings) = &self.settings {
            render_settings(frame, settings, centered_rect(58, 78, area));
        } else if self.confirm_quit_processing {
            render_quit_processing_confirmation(frame, centered_rect(54, 28, area));
        } else if let Some(meeting) = &self.delete_confirmation {
            render_delete_confirmation(frame, &meeting.name, centered_rect(54, 28, area));
        }
        render_status_bar(frame, area, self);
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

#[derive(Clone, Copy)]
enum PickerType {
    Language,
    Model,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AppAction {
    ToggleRecording,
    ToggleMicrophoneMute,
    StopRecording,
    StopRecordingAndQuit,
    TranscribeMeeting(PathBuf),
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
    let mut input = InputThread::spawn().context("start terminal input reader")?;
    let mut pipeline = pipeline::Worker::spawn().context("start pipeline worker")?;
    let (pipeline_tx, mut pipeline_rx) = mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    while !app.should_quit {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            _ = tick.tick() => {
                if app.pipeline_active {
                    app.processing_spinner_frame =
                        (app.processing_spinner_frame + 1) % PROCESSING_DOTS.len();
                }
                if let Err(error) = app.pump_recording() {
                    let finalize_error = app.stop_recording().err();
                    app.error = Some(match finalize_error {
                        Some(finalize) => format!("{error:#}; finalization also failed: {finalize:#}"),
                        None => format!("{error:#}"),
                    });
                }
            }
            maybe_event = input.recv() => {
                match maybe_event {
                    Some(UiEvent::Terminal(Event::Key(key))) if key.is_press() => {
                        match app.handle_key(key) {
                            Some(AppAction::ToggleRecording) if app.recording.is_some() => {
                                match app.stop_recording() {
                                    Ok(Some(path)) => {
                                        launch_pipeline(&mut app, path, &pipeline_tx)
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
                            Some(AppAction::ToggleMicrophoneMute) => {
                                if let Some(active) = &mut app.recording {
                                    active.session.toggle_microphone_muted();
                                }
                            }
                            Some(AppAction::StopRecording) => {
                                match app.stop_recording() {
                                    Ok(Some(path)) => {
                                        launch_pipeline(&mut app, path, &pipeline_tx)
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
                            Some(AppAction::TranscribeMeeting(path)) => {
                                launch_pipeline(&mut app, path, &pipeline_tx);
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
        let result = TokioCommand::new(std::env::current_exe().expect("current executable"))
            .args(["resume"])
            .arg(recording_path)
            .args(["--config"])
            .arg(config_path)
            .args(["--output-dir"])
            .arg(output_dir)
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
        if let Some(stderr) = child.stderr.take() {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let stage = if line.starts_with("Preparing transcription") {
                    Some("Preparing transcription")
                } else if line.starts_with("Reading recording") {
                    Some("Reading recording")
                } else if line.starts_with("Loading transcriber") {
                    Some("Loading transcriber")
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
                };
                if let Some(stage) = stage {
                    let _ = events.send(PipelineEvent::Stage(stage.to_owned()));
                }
            }
        }
        match child.wait().await {
            Ok(status) if status.success() => {
                let _ = events.send(PipelineEvent::WorkCompleted);
            }
            Ok(status) => {
                let _ = events.send(PipelineEvent::WorkFailed(format!(
                    "pipeline exited with {status}"
                )));
            }
            Err(error) => {
                let _ = events.send(PipelineEvent::WorkFailed(error.to_string()));
            }
        }
    });
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
    events: mpsc::UnboundedReceiver<UiEvent>,
    join: Option<JoinHandle<()>>,
}

impl InputThread {
    fn spawn() -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (event_tx, events) = mpsc::unbounded_channel();
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

fn input_loop(stop: &AtomicBool, events: &mpsc::UnboundedSender<UiEvent>) {
    while !stop.load(Ordering::Acquire) {
        match event::poll(Duration::from_millis(25)) {
            Ok(true) => match event::read() {
                Ok(event) => {
                    if events.send(UiEvent::Terminal(event)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if events.send(UiEvent::InputError(error.to_string())).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            },
            Ok(false) => {}
            Err(error) => {
                if events.send(UiEvent::InputError(error.to_string())).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
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
        Line::from("m                Mute / unmute microphone"),
        Line::from("t                Transcribe selected recording"),
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
            "Enter: choose language/model or save · Esc cancel",
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
        let line = Line::from(format!("{}  {}", item.label, item.detail));
        content.push(if index == picker.selected_index() {
            line.style(theme::selected_row())
        } else {
            line.style(theme::primary_text())
        });
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

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = if let Some(active) = &app.recording {
        let elapsed = active.session.elapsed_seconds() as u64;
        let microphone_status = if active.session.microphone_muted() {
            "·  MIC MUTED  ·  m to unmute"
        } else {
            "·  m to mute"
        };
        Line::from(vec![
            Span::styled(" ●", theme::recording_indicator()),
            Span::raw(format!(
                " Recording  {:02}:{:02}  ·  r to stop {microphone_status}",
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
    fn microphone_mute_shortcut_only_dispatches_while_recording() {
        assert_eq!(microphone_mute_action(false), None);
        assert_eq!(
            microphone_mute_action(true),
            Some(AppAction::ToggleMicrophoneMute)
        );
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
            Some(AppAction::TranscribeMeeting(PathBuf::from("/tmp/meeting")))
        );

        app.pipeline_active = true;
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            None
        );
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
