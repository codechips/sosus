//! Transcript reader pane.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::archive::Segment;
use crate::tui::theme;

pub(crate) struct RenderState<'a> {
    pub(crate) scroll: u16,
    pub(crate) active_segment: Option<usize>,
    pub(crate) selected_segment: Option<usize>,
    pub(crate) processing_status: Option<&'a str>,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    segments: &[Segment],
    state: RenderState<'_>,
) {
    if segments.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::pane_border(focused));
        frame.render_widget(block, area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Length(2),
                Constraint::Percentage(60),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(empty_state_message(state.processing_status))
                .style(theme::secondary_text())
                .alignment(Alignment::Center),
            rows[1],
        );
        return;
    }
    let mut lines = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let speaker = segment.speaker.as_deref().unwrap_or("Unknown");
        let style = if Some(index) == state.active_segment {
            theme::meter_signal()
        } else if Some(index) == state.selected_segment && focused {
            theme::selected_row()
        } else {
            theme::secondary_text()
        };
        lines.push(Line::styled(
            format!("{}  {speaker}", timestamp(segment.start_s)),
            style,
        ));
        lines.push(Line::styled(
            segment.text.clone(),
            if Some(index) == state.active_segment {
                theme::primary_text()
            } else if Some(index) == state.selected_segment && focused {
                theme::selected_row()
            } else {
                theme::primary_text()
            },
        ));
        lines.push(Line::raw(""));
    }
    let body = Text::from(lines);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused));
    frame.render_widget(
        Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: true })
            .scroll((state.scroll, 0)),
        area,
    );
}

fn empty_state_message(processing_status: Option<&str>) -> String {
    processing_status.map_or_else(
        || "Choose a recording to read its transcript".to_owned(),
        |status| format!("Processing recording\n{status}"),
    )
}

fn timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::empty_state_message;

    #[test]
    fn empty_reader_shows_the_live_processing_stage() {
        assert_eq!(
            empty_state_message(Some("Diarizing")),
            "Processing recording\nDiarizing"
        );
    }

    #[test]
    fn empty_reader_prompts_for_a_recording_when_idle() {
        assert_eq!(
            empty_state_message(None),
            "Choose a recording to read its transcript"
        );
    }
}
