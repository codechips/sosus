//! Recording controls and status pane.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::tui::theme;

#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focused: bool,
    elapsed_seconds: Option<f64>,
    _last_recording: Option<&str>,
    input_levels: Option<(f32, f32)>,
) {
    let meter_width = area.width.saturating_sub(10) as usize;
    let body = if let Some(_elapsed) = elapsed_seconds {
        let lines = vec![
            meter_line(
                "System",
                input_levels.map_or(0.0, |levels| levels.0),
                meter_width,
            ),
            meter_line(
                "Mic",
                input_levels.map_or(0.0, |levels| levels.1),
                meter_width,
            ),
        ];
        Text::from(lines)
    } else {
        Text::from(Line::styled("r to record", theme::secondary_text()))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::pane_border(focused));

    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn meter_line(label: &str, level: f32, width: usize) -> Line<'static> {
    let filled = (meter_level(level) * width as f32).round() as usize;
    Line::from(vec![
        Span::styled(format!("{label:<8}"), theme::secondary_text()),
        Span::styled("█".repeat(filled), theme::meter_signal()),
        Span::styled("░".repeat(width - filled), theme::meter_track()),
    ])
}

fn meter_level(peak: f32) -> f32 {
    if peak <= 0.0001 {
        return 0.0;
    }
    // Display a useful speech range: -50 dBFS is empty, 0 dBFS is full.
    ((20.0 * peak.log10() + 50.0) / 50.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::meter_level;

    #[test]
    fn meter_uses_speech_friendly_db_scaling() {
        assert_eq!(meter_level(0.0), 0.0);
        assert!(meter_level(0.03) > 0.3);
        assert_eq!(meter_level(1.0), 1.0);
    }
}
