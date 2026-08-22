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
    meetings: &[Meeting],
    selected: usize,
) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let lines = if meetings.is_empty() {
        vec![Line::styled("No recordings", theme::secondary_text())]
    } else {
        meetings
            .iter()
            .enumerate()
            .map(|(index, meeting)| {
                let label = meeting_label(&meeting.name);
                Line::styled(
                    format!("{label:<inner_width$}"),
                    if index == selected {
                        theme::selected_row()
                    } else {
                        theme::secondary_text()
                    },
                )
            })
            .collect()
    };
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused));

    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn meeting_label(name: &str) -> String {
    let Some((date, remainder)) = name.split_once('_') else {
        return name.to_owned();
    };
    let (time, suffix) = remainder.split_once('_').unwrap_or((remainder, ""));
    if date.len() != 10 || time.len() != 4 {
        return name.to_owned();
    }
    let label = format!("{date}  {}:{}", &time[..2], &time[2..]);
    if suffix.is_empty() {
        label
    } else {
        format!("{label} · {suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::meeting_label;

    #[test]
    fn turns_meeting_directories_into_readable_labels() {
        assert_eq!(meeting_label("2026-08-22_1436_2"), "2026-08-22  14:36 · 2");
        assert_eq!(meeting_label("unexpected"), "unexpected");
    }
}
