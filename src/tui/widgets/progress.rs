//! Pipeline progress widget.

use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::theme;

/// Render only active or actionable pipeline work. Completed work is intentionally hidden.
pub fn render(frame: &mut Frame<'_>, area: Rect, status: Option<&str>) {
    let Some(status) = status else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Pipeline")
        .border_style(theme::pane_border(false))
        .title_style(theme::pane_title(false));
    frame.render_widget(
        Paragraph::new(Line::styled(status, theme::secondary_text())).block(block),
        area,
    );
}
