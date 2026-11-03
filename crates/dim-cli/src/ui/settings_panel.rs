//! The `/settings` panel (port of the TS scrollback settings list, limited to
//! what the native interface runs): sections of rows with their value in a
//! column on the right. A switch flips with Enter, ←/→ or Space and is
//! saved at once; a row ending in `›` opens its picker, and closing the
//! picker comes back here.
//!
//! ```text
//! ╭──────────────────────────────────────────────╮
//! │ Settings                           esc close │
//! │   Interface                                  │
//! │ › Theme                  System ›            │
//! │   Language               简体中文 ›           │
//! │   Startup UI form        fullscreen          │
//! │   Tool details           collapsed           │
//! │   Thinking details       collapsed           │
//! │   Mouse                  on                  │
//! │   Notify when agent ...  on                  │
//! │   Show ACP sessions      off                 │
//! │   Prompt suggestion      off                 │
//! │   Permissions                                │
//! │   Permission             Workspace write ›   │
//! │   Process isolation      ›                   │
//! │ 1 / 10 · ↑↓ move · enter select · ←→/space change · esc close │
//! ╰──────────────────────────────────────────────╯
//! ```

use crate::ui::i18n::t;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::theme::Palette;
use crate::ui::transcript::tool_group::Detail;

/// The values the rows show, read when the panel opens.
pub struct Values {
    pub theme: String,
    pub language: String,
    pub fullscreen_default: bool,
    pub tool_detail: Detail,
    pub thinking_expanded: bool,
    pub mouse: bool,
    pub notify: bool,
    pub acp_sessions: bool,
    /// Whether the daemon lists the suggestion methods: without them the row
    /// is absent.
    pub prompt_suggestion_supported: bool,
    /// `Some` is the switch as the daemon reported it; `None` is a read that
    /// has not happened or failed, which the row shows rather than a value.
    pub prompt_suggestion: Option<bool>,
    pub permission: String,
}

/// One row: a switch flips in place, a link opens its picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Language,
    StartupForm,
    ToolDetails,
    ThinkingDetails,
    Mouse,
    Notify,
    AcpSessions,
    PromptSuggestion,
    Permission,
    Isolation,
}

const ROWS: [(Setting, &str); 11] = [
    (Setting::Theme, "theme"),
    (Setting::Language, "language"),
    (Setting::StartupForm, "startup-form"),
    (Setting::ToolDetails, "tool-details"),
    (Setting::ThinkingDetails, "thinking-details"),
    (Setting::Mouse, "mouse"),
    (Setting::Notify, "notify"),
    (Setting::AcpSessions, "acp-sessions"),
    (Setting::PromptSuggestion, "prompt-suggestion"),
    (Setting::Permission, "permission"),
    (Setting::Isolation, "isolation"),
];

impl Setting {
    pub fn id(self) -> &'static str {
        ROWS.iter()
            .find(|(s, _)| *s == self)
            .map_or("", |(_, id)| id)
    }

    fn of(id: &str) -> Option<Self> {
        ROWS.iter().find(|(_, i)| *i == id).map(|(s, _)| *s)
    }

    /// Flips in place rather than opening a picker.
    fn switch(self) -> bool {
        matches!(
            self,
            Self::StartupForm
                | Self::ToolDetails
                | Self::ThinkingDetails
                | Self::Mouse
                | Self::Notify
                | Self::AcpSessions
                | Self::PromptSuggestion
        )
    }
}

pub struct SettingsPanel {
    list: ListPanel,
}

impl SettingsPanel {
    pub fn new(values: &Values, select: Setting) -> Self {
        let on_off = |on: bool| if on { t("on") } else { t("off") };
        let link = |value: &str| format!("{value} ›");
        let row = |setting: Setting, label: &'static str, value: String| {
            Item::new(setting.id(), t(label), value)
        };
        let mut items = vec![
            Item::heading(t("Interface")),
            row(Setting::Theme, "Theme", link(&values.theme)),
            row(Setting::Language, "Language", link(&values.language)),
            row(
                Setting::StartupForm,
                "Startup UI form",
                if values.fullscreen_default {
                    t("fullscreen")
                } else {
                    t("scrollback")
                }
                .to_owned(),
            ),
            row(
                Setting::ToolDetails,
                "Tool details",
                t(values.tool_detail.label()).to_owned(),
            ),
            row(
                Setting::ThinkingDetails,
                "Thinking details",
                if values.thinking_expanded {
                    t("expanded")
                } else {
                    t("collapsed")
                }
                .to_owned(),
            ),
            row(
                Setting::Mouse,
                "Mouse (fullscreen)",
                on_off(values.mouse).to_owned(),
            ),
            row(
                Setting::Notify,
                "Notify when agent needs you",
                on_off(values.notify).to_owned(),
            ),
            row(
                Setting::AcpSessions,
                "Show ACP sessions",
                on_off(values.acp_sessions).to_owned(),
            ),
        ];
        if values.prompt_suggestion_supported {
            items.push(row(
                Setting::PromptSuggestion,
                "Prompt suggestion",
                match values.prompt_suggestion {
                    Some(on) => on_off(on).to_owned(),
                    None => t("Not available").to_owned(),
                },
            ));
        }
        items.extend([
            Item::heading(t("Permissions")),
            row(Setting::Permission, "Permission", link(&values.permission)),
            row(Setting::Isolation, "Process isolation", link("")),
        ]);
        let mut list = ListPanel::new(
            "Settings",
            "↑↓ move · Enter select · ←→/Space change · Esc close",
            "",
            "",
            "",
            false,
        )
        .without_filter();
        list.set_items(Ok(items), Some(select.id()));
        Self { list }
    }

    /// A paste goes into the filter.
    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
    }

    /// Shows a value that was read after the panel opened.
    pub fn set_prompt_suggestion(&mut self, enabled: bool) {
        let value = if enabled { t("on") } else { t("off") };
        self.list.set_detail(Setting::PromptSuggestion.id(), value);
    }

    /// Shows that the source could not be read, rather than a value.
    pub fn set_prompt_suggestion_unavailable(&mut self) {
        self.list
            .set_detail(Setting::PromptSuggestion.id(), t("Not available"));
    }

    pub fn selected(&self) -> Setting {
        self.list
            .selected_id()
            .and_then(|id| Setting::of(&id))
            .unwrap()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let setting = self.selected();
        if setting.switch()
            && key.mods.0 == 0
            && matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            )
        {
            return Effect::Setting(setting);
        }
        match self.list.handle_key(key) {
            Event::Close => Effect::Close,
            Event::Pick(id) => Setting::of(&id).map_or(Effect::None, Effect::Setting),
            Event::None | Event::Refresh => Effect::None,
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}
