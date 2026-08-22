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
    last_recording: Option<&str>,
    input_levels: Option<(f32, f32)>,
    spectrum: &[f32],
    pipeline_status: Option<&str>,
) {
    let title = if focused {
        "▶ Recording"
    } else {
        "Recording"
    };
    let meter_width = area.width.saturating_sub(10).clamp(10, 28) as usize;
    let body = if let Some(elapsed) = elapsed_seconds {
        let minutes = elapsed as u64 / 60;
        let seconds = elapsed as u64 % 60;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Status  ", theme::secondary_text()),
                Span::styled("RECORDING", theme::warning_text()),
            ]),
            Line::styled(
                format!("Elapsed {minutes:02}:{seconds:02}"),
                theme::primary_text(),
            ),
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
            Line::styled("Spectrum", theme::secondary_text()),
        ];
        lines.extend(spectrum_lines(
            spectrum,
            area.width.saturating_sub(4) as usize,
            6,
        ));
        lines.extend([
            Line::styled(
                pipeline_status.map_or("Pipeline  recording", |status| status),
                theme::secondary_text(),
            ),
            Line::styled("r or Ctrl+C to stop", theme::secondary_text()),
        ]);
        Text::from(lines)
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

fn spectrum_lines(levels: &[f32], width: usize, height: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let height = height.max(1);
    let columns = (0..width)
        .map(|column| {
            let index = column.saturating_mul(levels.len()) / width;
            levels.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0)
        })
        .collect::<Vec<_>>();
    (0..height)
        .map(|row| {
            let threshold = (height - row) as f32 / height as f32;
            let bars = columns
                .iter()
                .map(|level| if *level >= threshold { '█' } else { ' ' })
                .collect::<String>();
            Line::from(Span::styled(bars, theme::accent_text()))
        })
        .collect()
}

fn meter_line(label: &str, level: f32, width: usize) -> Line<'static> {
    let filled = (meter_level(level) * width as f32).round() as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));
    Line::from(vec![
        Span::styled(format!("{label:<6}"), theme::secondary_text()),
        Span::styled(bar, theme::accent_text()),
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
