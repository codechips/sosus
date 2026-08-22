//! Transcript reader pane.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Text},
    widgets::{Paragraph, Wrap},
};

use crate::archive::Segment;
use crate::tui::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    _focused: bool,
    segments: &[Segment],
    scroll: u16,
) {
    if segments.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(45),
                Constraint::Length(1),
                Constraint::Percentage(55),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new("Choose a recording to read its transcript")
                .style(theme::secondary_text())
                .alignment(Alignment::Center),
            rows[1],
        );
        return;
    }
    let mut lines = Vec::new();
    for segment in segments.iter().take(100) {
        let speaker = segment.speaker.as_deref().unwrap_or("Unknown");
        lines.push(Line::styled(
            format!("{}  {speaker}", timestamp(segment.start_s)),
            theme::secondary_text(),
        ));
        lines.push(Line::styled(segment.text.clone(), theme::primary_text()));
        lines.push(Line::raw(""));
    }
    let body = Text::from(lines);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        area,
    );
}

fn timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}
