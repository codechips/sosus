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
                let label = meeting_row(meeting, inner_width);
                Line::styled(
                    label,
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

fn meeting_row(meeting: &Meeting, width: usize) -> String {
    let label = meeting_label(&meeting.name);
    let duration = meeting.duration_seconds.map(format_duration);
    match duration {
        Some(duration) => {
            let label_width = width.saturating_sub(duration.len() + 1);
            format!("{label:<label_width$} {duration}")
        }
        None => format!("{label:<width$}"),
    }
}

fn meeting_label(name: &str) -> String {
    let Some((date, remainder)) = name.split_once('_') else {
        return name.to_owned();
    };
    let time = remainder
        .split_once('_')
        .map_or(remainder, |(time, _)| time);
    if date.len() != 10 || time.len() != 4 {
        return name.to_owned();
    }
    let month = match &date[5..7] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return name.to_owned(),
    };
    let day = date[8..10].parse::<u8>().unwrap_or(0);
    if day == 0 {
        return name.to_owned();
    }
    format!("{month} {day:02}  {}:{}", &time[..2], &time[2..])
}

fn format_duration(seconds: f64) -> String {
    let minutes = (seconds.max(0.0) / 60.0).round();
    if minutes < 60.0 {
        return format!("{}m", minutes.max(1.0) as u64);
    }
    let hours = minutes / 60.0;
    if (hours - hours.round()).abs() < 0.05 {
        format!("{}h", hours.round() as u64)
    } else {
        format!("{hours:.1}h")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration, meeting_label};

    #[test]
    fn turns_meeting_directories_into_readable_labels() {
        assert_eq!(meeting_label("2026-08-22_1436_2"), "Aug 22  14:36");
        assert_eq!(meeting_label("unexpected"), "unexpected");
    }

    #[test]
    fn formats_recording_duration_for_a_compact_sidebar() {
        assert_eq!(format_duration(1_200.0), "20m");
        assert_eq!(format_duration(3_600.0), "1h");
        assert_eq!(format_duration(5_400.0), "1.5h");
    }
}
