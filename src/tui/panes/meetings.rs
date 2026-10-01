//! Meeting archive pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph},
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
    let capacity = usize::from(area.height.saturating_sub(2) / 2);
    let start = visible_start(meetings.len(), selected, capacity);
    let lines = if meetings.is_empty() {
        vec![Line::styled("No recordings", theme::secondary_text())]
    } else {
        meetings
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .flat_map(|(index, meeting)| {
                let [title, details] = meeting_rows(meeting, inner_width);
                let selected = index == selected;
                [
                    Line::styled(
                        title,
                        if selected {
                            theme::selected_row()
                        } else {
                            theme::primary_text()
                        },
                    ),
                    Line::styled(
                        details,
                        if selected {
                            theme::selected_row()
                        } else {
                            theme::secondary_text()
                        },
                    ),
                ]
            })
            .collect()
    };
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused));

    frame.render_widget(Paragraph::new(body).block(block), area);
}

pub fn visible_start(count: usize, selected: usize, capacity: usize) -> usize {
    if capacity == 0 || count <= capacity {
        return 0;
    }
    selected
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(count - capacity)
}

fn meeting_rows(meeting: &Meeting, width: usize) -> [String; 2] {
    let timestamp = meeting_label(&meeting.name);
    let (title, show_timestamp_in_details) = match meeting.title.as_deref() {
        Some(title) => (title, true),
        None => (timestamp.as_str(), false),
    };
    let title = padded(title, width);
    let duration = meeting.duration_seconds.map(format_duration);
    let details = match (show_timestamp_in_details, duration) {
        (true, Some(duration)) => {
            let timestamp_width = width.saturating_sub(duration.len() + 1);
            format!("{} {duration}", padded(&timestamp, timestamp_width))
        }
        (true, None) => padded(&timestamp, width),
        (false, Some(duration)) => format!("{duration:>width$}"),
        (false, None) => " ".repeat(width),
    };
    [title, details]
}

fn padded(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let display_width = Line::from(value).width();
    if display_width <= width {
        return format!("{value}{}", " ".repeat(width - display_width));
    }
    let mut truncated = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = Line::from(character.to_string()).width();
        if used + character_width >= width {
            break;
        }
        truncated.push(character);
        used += character_width;
    }
    truncated.push('…');
    format!("{truncated}{}", " ".repeat(width - used - 1))
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
    use std::path::PathBuf;

    use crate::archive::Meeting;

    use super::{format_duration, meeting_label, meeting_rows, visible_start};

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

    #[test]
    fn renders_title_above_timestamp_and_duration() {
        let meeting = Meeting {
            path: PathBuf::new(),
            name: "2026-09-30_1432".to_owned(),
            title: Some("Product roadmap".to_owned()),
            duration_seconds: Some(2_820.0),
            transcript: Vec::new(),
        };

        assert_eq!(
            meeting_rows(&meeting, 24),
            [
                "Product roadmap         ".to_owned(),
                "Sep 30  14:32        47m".to_owned(),
            ]
        );
    }

    #[test]
    fn falls_back_to_timestamp_when_title_is_missing() {
        let meeting = Meeting {
            path: PathBuf::new(),
            name: "2026-09-30_1432".to_owned(),
            title: None,
            duration_seconds: Some(2_820.0),
            transcript: Vec::new(),
        };

        assert_eq!(
            meeting_rows(&meeting, 20),
            [
                "Sep 30  14:32       ".to_owned(),
                "                 47m".to_owned(),
            ]
        );
    }

    #[test]
    fn keeps_selected_recording_in_the_two_line_viewport() {
        assert_eq!(visible_start(10, 0, 3), 0);
        assert_eq!(visible_start(10, 3, 3), 1);
        assert_eq!(visible_start(10, 9, 3), 7);
    }
}
