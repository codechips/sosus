//! Terminal UI state and event loop.

mod modals;
mod panes;
mod theme;
mod widgets;

use std::{
    collections::VecDeque,
    io::{self, Stdout},
    panic,
    path::PathBuf,
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
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

use crate::{
    archive::{self, Meeting, Segment},
    audio,
    paths::AppPaths,
    pipeline::{self, AppEvent as PipelineEvent},
};

const MINIMUM_WIDTH: u16 = 80;
const MINIMUM_HEIGHT: u16 = 24;
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
    show_settings: bool,
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
}

impl App {
    fn new(startup: Startup) -> Self {
        Self {
            archive_dir: startup.archive_dir,
            error: None,
            focus: Focus::Meetings,
            show_help: false,
            show_settings: false,
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
        self.selected_meeting = if delta.is_negative() {
            self.selected_meeting.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected_meeting + delta as usize).min(last)
        };
        if let Some(meeting) = self.meetings.get(self.selected_meeting) {
            self.transcript = meeting.transcript.clone();
            self.transcript_scroll = 0;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<AppAction> {
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

        if key.code == KeyCode::Esc && (self.show_help || self.show_settings) {
            self.show_help = false;
            self.show_settings = false;
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
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('r'), _) => return Some(AppAction::ToggleRecording),
            (KeyCode::Char('?'), _) => self.show_help = !self.show_help,
            (KeyCode::F(2), _) => self.show_settings = !self.show_settings,
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => {
                self.focus = self.focus.previous();
            }
            (KeyCode::Tab, _) => self.focus = self.focus.next(),
            _ => {}
        }
        None
    }

    async fn start_recording(&mut self) -> anyhow::Result<()> {
        let context = self
            .recording_context
            .as_ref()
            .context("recording is not configured")?;
        audio::ensure_capture_permissions().await?;
        let started_at = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let meeting_dir = context.app_paths.create_meeting_dir(started_at)?;
        let session = audio::RecordingSession::start(meeting_dir.join("recording.wav"))?;
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
                self.pipeline_status = Some("Preparing recording".to_owned());
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
        let content_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));

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
                .constraints([Constraint::Length(28), Constraint::Min(40)])
                .split(rows[0]);
            (columns, Some(rows[1]))
        } else {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(28), Constraint::Min(40)])
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
        } else if self.show_settings {
            render_settings_preview(frame, centered_rect(64, 50, area));
        }
        render_status_bar(frame, area, self);
    }
}

pub struct Startup {
    pub archive_dir: String,
    pub warnings: Vec<String>,
    pub recording: Option<RecordingStartup>,
}

pub struct RecordingStartup {
    pub app_paths: AppPaths,
}

struct ActiveRecording {
    session: audio::RecordingSession,
    meeting_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppAction {
    ToggleRecording,
    StopRecording,
    StopRecordingAndQuit,
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
                                        launch_pipeline(&app, path, &pipeline_tx)
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
                            Some(AppAction::StopRecording) => {
                                match app.stop_recording() {
                                    Ok(Some(path)) => {
                                        launch_pipeline(&app, path, &pipeline_tx)
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
                            None => {}
                        }
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
    app: &App,
    recording_path: PathBuf,
    events: &mpsc::UnboundedSender<PipelineEvent>,
) {
    let Some(context) = &app.recording_context else {
        return;
    };
    let output_dir = context.app_paths.output_dir().to_path_buf();
    let events = events.clone();
    tokio::spawn(async move {
        let _ = events.send(PipelineEvent::WorkStarted);
        let result = TokioCommand::new(std::env::current_exe().expect("current executable"))
            .args(["resume"])
            .arg(recording_path)
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
                let stage = if line.starts_with("Transcribing ") {
                    Some("Transcribing")
                } else if line.starts_with("Diarizing ") {
                    Some("Diarizing")
                } else if line.starts_with("Saved transcript:") {
                    Some("Exporting")
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

type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<(Self, AppTerminal)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
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
    let screen_result = execute!(io::stdout(), LeaveAlternateScreen);
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
        Line::from("F2               Settings preview"),
        Line::from("r                Start / stop recording"),
        Line::from("?                Toggle help"),
        Line::from("q                Stop recording and quit"),
        Line::from("Ctrl+C           Stop recording, otherwise quit"),
        Line::from("Esc              Close overlay"),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Help")
        .style(theme::overlay());
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_settings_preview(frame: &mut Frame<'_>, area: Rect) {
    let content = Text::from(vec![
        Line::styled("Settings arrive later in M0.", theme::primary_text()),
        Line::styled("Press Esc to return.", theme::secondary_text()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Settings")
        .style(theme::overlay());
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
        .style(theme::overlay());
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
        Line::from(vec![
            Span::styled(" ●", theme::recording_indicator()),
            Span::raw(format!(
                " Recording  {:02}:{:02}  ·  r to stop",
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
    fn recording_key_requests_a_toggle() {
        let mut app = app();
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(AppAction::ToggleRecording)
        );
    }

    #[test]
    fn pipeline_status_keeps_the_current_user_facing_stage() {
        let mut app = app();
        app.handle_pipeline_event(PipelineEvent::WorkStarted);
        assert!(app.pipeline_active);
        assert_eq!(app.pipeline_status.as_deref(), Some("Preparing recording"));

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
                transcript: Vec::new(),
            },
            Meeting {
                path: PathBuf::from("/tmp/two"),
                name: "two".to_owned(),
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
        for content in ["No recordings", "Choose a recording"] {
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
