//! The `/theme` panel: the registry themes of the terminal's background
//! family, previewed while moving and persisted on Enter (port of
//! `openThemePanel` / `handleThemePanelKey`).

use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::KeyEvent;
use crate::ui::theme::themes::THEMES;
use crate::ui::theme::{Appearance, ColorMode, Palette};

pub const SYSTEM: &str = "system";
const HINT: &str = "↑↓ preview · enter apply · esc cancel";

/// `(id, label)` of the themes this terminal may use.
pub fn entries(mode: ColorMode, appearance: Appearance) -> Vec<(&'static str, &'static str)> {
    let mut list = vec![(SYSTEM, "System")];
    if matches!(mode, ColorMode::TrueColor | ColorMode::Ansi256) {
        let dark = matches!(appearance, Appearance::Dark);
        list.extend(
            THEMES
                .iter()
                .filter(|t| t.dark == dark)
                .map(|t| (t.id, t.label)),
        );
    }
    list
}

pub struct ThemePanel {
    list: ListPanel,
    /// The theme in effect when the panel opened (restored on Esc).
    original: String,
}

impl ThemePanel {
    pub fn new(mode: ColorMode, appearance: Appearance, active: &str) -> Self {
        let entries = entries(mode, appearance);
        let mut list = ListPanel::new("Theme", HINT, "", "", "", false).without_filter();
        list.set_items(
            Ok(entries
                .iter()
                .map(|(id, label)| Item::new(*id, *label, ""))
                .collect()),
            Some(active),
        );
        Self {
            list,
            original: active.to_owned(),
        }
    }

    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let before = self.list.selected_id();
        match self.list.handle_key(key) {
            Event::Close => Effect::ThemeRestore(self.original.clone()),
            Event::Pick(id) => Effect::ThemeApply(id),
            Event::None | Event::Refresh => {
                let now = self.list.selected_id();
                match now {
                    Some(id) if now != before => Effect::ThemePreview(id),
                    _ => Effect::None,
                }
            }
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}

/// The theme id saved in `config.json` (`themeId`), if any.
pub fn saved_theme_id(home: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(config_path(home)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["themeId"].as_str().map(str::to_owned)
}

/// Writes `themeId` into `config.json`, keeping the other keys; the error
/// text on failure.
pub fn save_theme_id(home: &std::path::Path, id: &str) -> Option<String> {
    let path = config_path(home);
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    value["themeId"] = id.into();
    let text = match serde_json::to_string_pretty(&value) {
        Ok(text) => text + "\n",
        Err(error) => return Some(error.to_string()),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, text)
        .err()
        .map(|e| format!("Could not save the theme: {e}"))
}

fn config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    #[test]
    fn a_terminal_gets_system_and_the_themes_of_its_background_family() {
        let dark = entries(ColorMode::TrueColor, Appearance::Dark);
        assert_eq!(dark.first(), Some(&(SYSTEM, "System")));
        assert_eq!(dark.len(), 11);
        assert!(dark.iter().any(|(id, _)| *id == "dim-dark"));
        assert!(!dark.iter().any(|(id, _)| *id == "dim-light"));
        let light = entries(ColorMode::Ansi256, Appearance::Light);
        assert!(light.iter().any(|(id, _)| *id == "dim-light"));
        // 16 colours: only `system`.
        assert_eq!(entries(ColorMode::Ansi16, Appearance::Dark).len(), 1);
    }

    #[test]
    fn a_theme_of_the_other_family_falls_back_to_system() {
        let system = Theme::system(ColorMode::TrueColor, Appearance::Light, None);
        let named = Theme::named("dim-dark", ColorMode::TrueColor, Appearance::Light, None);
        assert_eq!(named.palette, system.palette);
        let own = Theme::named("dim-dark", ColorMode::TrueColor, Appearance::Dark, None);
        assert_ne!(
            own.palette,
            Theme::system(ColorMode::TrueColor, Appearance::Dark, None).palette
        );
    }
}
