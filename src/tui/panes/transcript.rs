//! Transcript reader pane.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::archive::Segment;
use crate::tui::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    segments: &[Segment],
    scroll: u16,
    active_segment: Option<usize>,
    selected_segment: Option<usize>,
) {
    if segments.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::pane_border(focused));
        frame.render_widget(block, area);
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
    for (index, segment) in segments.iter().take(100).enumerate() {
        let speaker = segment.speaker.as_deref().unwrap_or("Unknown");
        let style = if Some(index) == active_segment {
            theme::meter_signal()
        } else if Some(index) == selected_segment && focused {
            theme::selected_row()
        } else {
            theme::secondary_text()
        };
        lines.push(Line::styled(
            format!("{}  {speaker}", timestamp(segment.start_s)),
            style,
        ));
        lines.push(Line::styled(
            segment.text.clone(),
            if Some(index) == active_segment {
                theme::primary_text()
            } else if Some(index) == selected_segment && focused {
                theme::selected_row()
            } else {
                theme::primary_text()
            },
        ));
        lines.push(Line::raw(""));
    }
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused));
    frame.render_widget(
        Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        area,
    );
}

fn timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}
