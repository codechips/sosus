//! Recording controls and status pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::tui::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    elapsed_seconds: Option<f64>,
    last_recording: Option<&str>,
) {
    let title = if focused {
        "▶ Recording"
    } else {
        "Recording"
    };
    let body = if let Some(elapsed) = elapsed_seconds {
        let minutes = elapsed as u64 / 60;
        let seconds = elapsed as u64 % 60;
        Text::from(vec![
            Line::from(vec![
                Span::styled("Status  ", theme::secondary_text()),
                Span::styled("RECORDING", theme::warning_text()),
            ]),
            Line::styled(
                format!("Elapsed {minutes:02}:{seconds:02}"),
                theme::primary_text(),
            ),
            Line::styled("r or Ctrl+C to stop", theme::secondary_text()),
        ])
    } else {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Status  ", theme::secondary_text()),
                Span::styled("READY", theme::primary_text()),
            ]),
            Line::styled("r to start recording", theme::secondary_text()),
        ];
        if let Some(path) = last_recording {
            lines.push(Line::styled(
                format!("Saved {path}"),
                theme::secondary_text(),
            ));
        }
        Text::from(lines)
    };
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
