use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const MAX_TITLE_CHARS: usize = 120;

#[derive(Debug, PartialEq, Eq)]
pub enum TitleAction {
    None,
    Cancel,
    Save(String),
}

pub struct TitleModal {
    value: String,
    cursor: usize,
}

impl TitleModal {
    pub fn new(value: &str) -> Self {
        let value: String = value.chars().take(MAX_TITLE_CHARS).collect();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TitleAction {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => TitleAction::Cancel,
            (KeyCode::Enter, _) => TitleAction::Save(self.value.clone()),
            (KeyCode::Home, _) => {
                self.cursor = 0;
                TitleAction::None
            }
            (KeyCode::End, _) => {
                self.cursor = self.value.chars().count();
                TitleAction::None
            }
            (KeyCode::Left, _) => {
                self.cursor = self.cursor.saturating_sub(1);
                TitleAction::None
            }
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.value.chars().count());
                TitleAction::None
            }
            (KeyCode::Backspace, _) if self.cursor > 0 => {
                self.cursor -= 1;
                self.remove_at_cursor();
                TitleAction::None
            }
            (KeyCode::Delete, _) => {
                self.remove_at_cursor();
                TitleAction::None
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
                TitleAction::None
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.value.chars().count() < MAX_TITLE_CHARS =>
            {
                let byte = byte_index(&self.value, self.cursor);
                self.value.insert(byte, character);
                self.cursor += 1;
                TitleAction::None
            }
            _ => TitleAction::None,
        }
    }

    fn remove_at_cursor(&mut self) {
        let start = byte_index(&self.value, self.cursor);
        let end = byte_index(&self.value, self.cursor + 1);
        if start < end {
            self.value.replace_range(start..end, "");
        }
    }
}

fn byte_index(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_titles_at_the_cursor() {
        let mut modal = TitleModal::new("Möte");
        modal.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        modal.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        modal.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        assert_eq!(modal.value(), "Möae");
        assert_eq!(modal.cursor(), 3);
    }

    #[test]
    fn enter_saves_and_control_u_clears() {
        let mut modal = TitleModal::new("Existing");
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            TitleAction::None
        );
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            TitleAction::Save(String::new())
        );
    }
}
