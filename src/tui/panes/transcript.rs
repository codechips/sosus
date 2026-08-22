//! Transcript reader pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::db::{Segment, Summary};
use crate::tui::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    summary: Option<&Summary>,
    segments: &[Segment],
    scroll: u16,
) {
    let title = if focused {
        "▶ Transcript"
    } else {
        "Transcript"
    };
    let mut lines = Vec::new();
    if let Some(summary) = summary {
        lines.push(Line::styled("Summary", theme::accent_text()));
        for line in summary.body.lines() {
            if !line.is_empty() && !line.starts_with("## ") {
                lines.push(Line::styled(line, theme::primary_text()));
            }
        }
        lines.push(Line::raw(""));
    }
    if !segments.is_empty() {
        lines.push(Line::styled("Transcript", theme::accent_text()));
        for segment in segments.iter().take(100) {
            let speaker = segment.speaker.as_deref().unwrap_or("Unknown");
            lines.push(Line::styled(
                format!(
                    "{}–{}  {speaker}",
                    timestamp(segment.start_s),
                    timestamp(segment.end_s)
                ),
                theme::secondary_text(),
            ));
            lines.push(Line::styled(segment.text.clone(), theme::primary_text()));
            lines.push(Line::raw(""));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "Select a meeting to read its transcript.",
            theme::primary_text(),
        ));
    }
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(theme::pane_border(focused))
        .title_style(theme::pane_title(focused));

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
