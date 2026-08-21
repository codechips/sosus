//! Meeting archive pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::tui::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, focused: bool, archive_dir: &str) {
    let title = if focused { "▶ Meetings" } else { "Meetings" };
    let body = Text::from(vec![
        Line::styled("No meetings yet", theme::primary_text()),
        Line::styled(
            "Recordings will appear newest first.",
            theme::secondary_text(),
        ),
        Line::styled(archive_dir, theme::secondary_text()),
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
