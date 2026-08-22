//! Recording controls and status pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::tui::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, focused: bool) {
    let title = if focused {
        "▶ Recording"
    } else {
        "Recording"
    };
    let body = Text::from(vec![
        Line::from(vec![
            Span::styled("Status  ", theme::secondary_text()),
            Span::styled("UNAVAILABLE", theme::warning_text()),
        ]),
        Line::styled(
            "Core recording is being built now.",
            theme::secondary_text(),
        ),
    ]);
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
