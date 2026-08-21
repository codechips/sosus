//! Centralized colors and text styles.

use ratatui::style::{Color, Modifier, Style};

pub fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

pub fn pane_title(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

pub fn primary_text() -> Style {
    Style::default().fg(Color::White)
}

pub fn secondary_text() -> Style {
    Style::default().fg(Color::Gray)
}

pub fn accent_text() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn warning_text() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn overlay() -> Style {
    Style::default().bg(Color::Black).fg(Color::White)
}
