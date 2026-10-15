//! The `/language` panel (port of the TS language dialog): the system
//! default, then every locale. Moving previews the language, Enter saves it,
//! Esc puts the one in effect back.

use crate::ui::i18n::{self, LOCALES, Locale, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::KeyEvent;
use crate::ui::theme::Palette;

const SYSTEM: &str = "system";

pub struct LanguagePanel {
    list: ListPanel,
    /// The saved choice when the panel opened; `None` is the system default.
    original: Option<Locale>,
}

/// The choice a row stands for.
fn choice(id: &str) -> Option<Locale> {
    Locale::from_code(id)
}

impl LanguagePanel {
    pub fn new(saved: Option<Locale>) -> Self {
        let mut list = ListPanel::new(
            "Language",
            "↑↓ preview · enter save · esc restore",
            "",
            "",
            "",
            false,
        )
        .without_filter();
        let system = tf(
            "System default ({locale})",
            &[("locale", i18n::system_locale().label())],
        );
        let mut items = vec![Item::new(SYSTEM, system, "")];
        items.extend(
            LOCALES
                .iter()
                .map(|(_, code, label)| Item::new(*code, *label, "")),
        );
        list.set_items(Ok(items), Some(saved.map_or(SYSTEM, Locale::code)));
        Self {
            list,
            original: saved,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let before = self.list.selected_id();
        match self.list.handle_key(key) {
            Event::Close => Effect::LanguageRestore(self.original),
            Event::Pick(id) => Effect::LanguageApply(choice(&id)),
            Event::None | Event::Refresh => match self.list.selected_id() {
                Some(id) if Some(&id) != before.as_ref() => Effect::LanguagePreview(choice(&id)),
                _ => Effect::None,
            },
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}
