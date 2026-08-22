//! Terminal UI state and event loop.

mod modals;
mod panes;
mod theme;
mod widgets;

use std::{
    collections::VecDeque,
    io::{self, Stdout},
    panic,
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
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use crate::pipeline::{self, AppEvent as PipelineEvent};

const MINIMUM_WIDTH: u16 = 80;
const MINIMUM_HEIGHT: u16 = 24;
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Meetings,
    Transcript,
    Chat,
    Recording,
}

impl Focus {
    const ALL: [Self; 4] = [
        Self::Meetings,
        Self::Transcript,
        Self::Chat,
        Self::Recording,
    ];

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
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.error.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Enter, _) => {
                    self.error = None;
                    return;
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {}
                _ => return,
            }
        } else if !self.warnings.is_empty() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Enter, _) => {
                    self.warnings.pop_front();
                    return;
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {}
                _ => return,
            }
        }

        if key.code == KeyCode::Esc && (self.show_help || self.show_settings) {
            self.show_help = false;
            self.show_settings = false;
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('?'), _) => self.show_help = !self.show_help,
            (KeyCode::F(2), _) => self.show_settings = !self.show_settings,
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => {
                self.focus = self.focus.previous();
            }
            (KeyCode::Tab, _) => self.focus = self.focus.next(),
            _ => {}
        }
    }

    fn handle_pipeline_event(&mut self, event: PipelineEvent) {
        self.message = match event {
            PipelineEvent::WorkStarted => Some("Pipeline started".to_owned()),
            PipelineEvent::WorkProgress { completed, total } => {
                Some(format!("Pipeline {completed}/{total}"))
            }
            PipelineEvent::WorkCompleted => Some("Pipeline completed".to_owned()),
            PipelineEvent::WorkCancelled => Some("Pipeline cancelled".to_owned()),
            PipelineEvent::WorkerStopped => None,
        };
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < MINIMUM_WIDTH || area.height < MINIMUM_HEIGHT {
            render_too_small(frame, area);
            return;
        }

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(45),
                Constraint::Percentage(30),
            ])
            .split(area);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(columns[2]);

        panes::meetings::render(
            frame,
            columns[0],
            self.focus == Focus::Meetings,
            &self.archive_dir,
        );
        panes::transcript::render(frame, columns[1], self.focus == Focus::Transcript);
        panes::chat::render(frame, right[0], self.focus == Focus::Chat);
        panes::recording::render(frame, right[1], self.focus == Focus::Recording);

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
        } else if let Some(message) = &self.message {
            render_message(frame, message, area);
        }
    }
}

pub struct Startup {
    pub archive_dir: String,
    pub warnings: Vec<String>,
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
    let mut input = InputThread::spawn().context("start terminal input reader")?;
    let mut pipeline = pipeline::Worker::spawn().context("start pipeline worker")?;
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    while !app.should_quit {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            _ = tick.tick() => {}
            maybe_event = input.recv() => {
                match maybe_event {
                    Some(UiEvent::Terminal(Event::Key(key))) if key.is_press() => app.handle_key(key),
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
        }
    }

    input.stop().context("stop terminal input reader")?;
    pipeline.shutdown().context("stop pipeline worker")?;
    Ok(())
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
        Line::from("?                Toggle help"),
        Line::from("q / Ctrl+C       Quit"),
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

fn render_message(frame: &mut Frame<'_>, message: &str, area: Rect) {
    let message_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
    frame.render_widget(
        Paragraph::new(message).style(theme::secondary_text()),
        message_area,
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
        })
    }

    #[test]
    fn focus_cycles_in_both_directions() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Transcript);
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Meetings);
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Recording);
    }

    #[test]
    fn global_quit_keys_request_clean_shutdown() {
        for key in [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut app = app();
            app.handle_key(key);
            assert!(app.should_quit);
        }
    }

    #[test]
    fn config_warnings_are_preserved_and_dismissed_in_order() {
        let mut app = App::new(Startup {
            archive_dir: "/tmp/sosus-test-recordings".to_owned(),
            warnings: vec!["first".to_owned(), "second".to_owned()],
        });
        assert_eq!(app.warnings.front().map(String::as_str), Some("first"));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.warnings.front().map(String::as_str), Some("second"));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
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
    fn normal_size_renders_all_four_panes() {
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
        for content in [
            "Meetings",
            "Transcript",
            "Chat",
            "Recording",
            "No meetings yet",
            "Select a meeting",
            "Archive scope",
            "UNAVAILABLE",
        ] {
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
        assert!(rendered.contains("Transcript"));
        assert!(!rendered.contains("Terminal too small"));
    }
}
