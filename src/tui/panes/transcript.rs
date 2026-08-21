//! Transcript reader pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::tui::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, focused: bool) {
    let title = if focused {
        "▶ Transcript"
    } else {
        "Transcript"
    };
    let body = Text::from(vec![
        Line::styled(
            "Select a meeting to read its transcript.",
            theme::primary_text(),
        ),
        Line::styled(
            "Speaker labels and timestamps will appear here.",
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
