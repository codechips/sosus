//! Meeting archive pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::archive::Meeting;
use crate::tui::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    archive_dir: &str,
    meetings: &[Meeting],
    selected: usize,
) {
    let mut lines = if meetings.is_empty() {
        vec![
            Line::styled("No meetings yet", theme::primary_text()),
            Line::styled(
                "Recordings will appear newest first.",
                theme::secondary_text(),
            ),
        ]
    } else {
        meetings
            .iter()
            .enumerate()
            .map(|(index, meeting)| {
                let marker = if index == selected { "▶ " } else { "  " };
                let title = &meeting.name;
                Line::styled(
                    format!("{marker}{title}"),
                    if index == selected {
                        theme::primary_text()
                    } else {
                        theme::secondary_text()
                    },
                )
            })
            .collect()
    };
    lines.push(Line::styled(archive_dir, theme::secondary_text()));
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused))
        .title_style(theme::pane_title(focused));

    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}
