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
//! │   Default send mode      queue               │
//! │   Tool details           collapsed           │
//! │   Thinking details       collapsed           │
//! │   Mouse                  on                  │
//! │   Notify when agent ...  on                  │
//! │   Show ACP sessions      off                 │
//! │   Prompt suggestion      off                 │
//! │   Permissions                                │
//! │   Permission             Workspace write ›   │
//! │   Process isolation      ›                   │
//! │ 1 / 11 · ↑↓ move · enter select · ←→/space change · esc close │
//! ╰──────────────────────────────────────────────╯
//! ```

use crate::model::session::{PromptField, PromptSetting, SubmitMode};
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
    pub default_send_mode: SubmitMode,
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
    /// The `prompts` source's fields: `Some(custom)` is an override, and
    /// `None` is a read that has not happened or failed.
    pub prompts: [Option<bool>; 3],
    pub permission: String,
}

/// One row: a switch flips in place, a link opens its picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Language,
    StartupForm,
    DefaultSendMode,
    ToolDetails,
    ThinkingDetails,
    Mouse,
    Notify,
    AcpSessions,
    PromptSuggestion,
    BasePrompt,
    SummaryPrompt,
    SkillsPrompt,
    Permission,
    Isolation,
}

const ROWS: [(Setting, &str); 15] = [
    (Setting::Theme, "theme"),
    (Setting::Language, "language"),
    (Setting::StartupForm, "startup-form"),
    (Setting::DefaultSendMode, "default-send-mode"),
    (Setting::ToolDetails, "tool-details"),
    (Setting::ThinkingDetails, "thinking-details"),
    (Setting::Mouse, "mouse"),
    (Setting::Notify, "notify"),
    (Setting::AcpSessions, "acp-sessions"),
    (Setting::PromptSuggestion, "prompt-suggestion"),
    (Setting::BasePrompt, "base-prompt"),
    (Setting::SummaryPrompt, "summary-prompt"),
    (Setting::SkillsPrompt, "skills-prompt"),
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
                | Self::DefaultSendMode
                | Self::ToolDetails
                | Self::ThinkingDetails
                | Self::Mouse
                | Self::Notify
                | Self::AcpSessions
                | Self::PromptSuggestion
        )
    }

    fn prompt_field(self) -> Option<PromptField> {
        match self {
            Self::BasePrompt => Some(PromptField::Base),
            Self::SummaryPrompt => Some(PromptField::Summary),
            Self::SkillsPrompt => Some(PromptField::Skills),
            _ => None,
        }
    }
}

fn prompt_row(field: PromptField) -> Setting {
    match field {
        PromptField::Base => Setting::BasePrompt,
        PromptField::Summary => Setting::SummaryPrompt,
        PromptField::Skills => Setting::SkillsPrompt,
    }
}

fn prompt_label(field: PromptField) -> &'static str {
    match field {
        PromptField::Base => "Base prompt",
        PromptField::Summary => "Compaction prompt",
        PromptField::Skills => "Skills instructions",
    }
}

const HINT: &str = "↑↓ move · Enter select · ←→/Space change · r reset · Esc close";
const SKILLS_HINT: &str =
    "skills replaces the marker and call shape; the skill tool and list stay · r reset";
const PROMPT_HINT: &str = "↑↓ move · Enter edit · r reset · Esc close";

fn hint_for(setting: Setting) -> &'static str {
    if setting == Setting::SkillsPrompt {
        SKILLS_HINT
    } else if setting.prompt_field().is_some() {
        PROMPT_HINT
    } else {
        HINT
    }
}

fn prompt_origin(prompt: Option<bool>) -> String {
    match prompt {
        Some(true) => format!("{} ›", t("custom")),
        Some(false) => format!("{} ›", t("built-in")),
        None => t("Not available").to_owned(),
    }
}

pub struct SettingsPanel {
    list: ListPanel,
    prompts: [Option<bool>; 3],
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
                Setting::DefaultSendMode,
                "Default send mode",
                values.default_send_mode.as_str().to_owned(),
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
        items.push(Item::heading(t("Prompts")));
        for (index, field) in PromptField::ALL.into_iter().enumerate() {
            items.push(row(
                prompt_row(field),
                prompt_label(field),
                prompt_origin(values.prompts[index]),
            ));
        }
        items.extend([
            Item::heading(t("Permissions")),
            row(Setting::Permission, "Permission", link(&values.permission)),
            row(Setting::Isolation, "Process isolation", link("")),
        ]);
        let mut list = ListPanel::new("Settings", HINT, "", "", "", false).without_filter();
        list.set_items(Ok(items), Some(select.id()));
        list.set_hint(t(hint_for(select)).to_owned());
        Self {
            list,
            prompts: values.prompts,
        }
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

    /// Shows each prompt text's origin once the source has been read.
    pub fn set_prompts(&mut self, setting: &PromptSetting) {
        for (index, field) in PromptField::ALL.into_iter().enumerate() {
            self.prompts[index] = Some(setting.text(field).is_some());
            self.list
                .set_detail(prompt_row(field).id(), prompt_origin(self.prompts[index]));
        }
    }

    /// Shows that the prompts source could not be read.
    pub fn set_prompts_unavailable(&mut self) {
        self.prompts = [None; 3];
        for field in PromptField::ALL {
            self.list
                .set_detail(prompt_row(field).id(), t("Not available"));
        }
    }

    pub fn selected(&self) -> Setting {
        self.list
            .selected_id()
            .and_then(|id| Setting::of(&id))
            .unwrap()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let setting = self.selected();
        if let Some(field) = setting.prompt_field() {
            let custom = PromptField::ALL
                .iter()
                .position(|candidate| *candidate == field)
                .and_then(|index| self.prompts[index])
                == Some(true);
            if key.mods.0 == 0 && key.code == KeyCode::Char('r') && custom {
                return Effect::ResetPrompt(field);
            }
        }
        if setting.switch()
            && key.mods.0 == 0
            && matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            )
        {
            return Effect::Setting(setting);
        }
        let event = self.list.handle_key(key);
        self.set_hint();
        match event {
            Event::Close => Effect::Close,
            Event::Pick { id, .. } => match Setting::of(&id) {
                Some(setting) => match setting.prompt_field() {
                    Some(field) => Effect::EditPrompt(field),
                    None => Effect::Setting(setting),
                },
                None => Effect::None,
            },
            Event::None | Event::Refresh => Effect::None,
        }
    }

    fn set_hint(&mut self) {
        self.list.set_hint(t(hint_for(self.selected())).to_owned());
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}
