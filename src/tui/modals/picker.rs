use crossterm::event::{KeyCode, KeyEvent};

#[derive(Clone)]
pub struct PickerItem {
    pub value: String,
    pub label: String,
}

pub enum PickerAction {
    None,
    Cancel,
    Choose(String),
}

pub struct PickerModal {
    title: &'static str,
    items: Vec<PickerItem>,
    filter: String,
    selected: usize,
}

impl PickerModal {
    pub fn new(title: &'static str, items: Vec<PickerItem>, selected: &str) -> Self {
        let selected = items
            .iter()
            .position(|item| item.value == selected)
            .unwrap_or(0);
        Self {
            title,
            items,
            filter: String::new(),
            selected,
        }
    }
    pub fn title(&self) -> &'static str {
        self.title
    }
    pub fn filter(&self) -> &str {
        &self.filter
    }
    pub fn visible(&self) -> Vec<&PickerItem> {
        let query = self.filter.to_lowercase();
        self.items
            .iter()
            .filter(|item| query.is_empty() || item.label.to_lowercase().contains(&query))
            .collect()
    }
    pub fn selected_index(&self) -> usize {
        self.selected
    }
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        let count = self.visible().len();
        match key.code {
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Enter => self
                .visible()
                .get(self.selected)
                .map(|item| PickerAction::Choose(item.value.clone()))
                .unwrap_or(PickerAction::None),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                PickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
                PickerAction::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                PickerAction::None
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.filter.push(character);
                self.selected = 0;
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }
}
