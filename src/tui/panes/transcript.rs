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
) {
    let title = if focused {
        "▶ Transcript"
    } else {
        "Transcript"
    };
    let mut lines = Vec::new();
    if let Some(summary) = summary {
        lines.push(Line::styled(summary.body.clone(), theme::primary_text()));
    }
    if !segments.is_empty() {
        lines.push(Line::styled("Transcript", theme::secondary_text()));
        for segment in segments.iter().take(12) {
            let speaker = segment.speaker.as_deref().unwrap_or("Unknown");
            lines.push(Line::styled(
                format!("{speaker}: {}", segment.text),
                theme::primary_text(),
            ));
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
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}
