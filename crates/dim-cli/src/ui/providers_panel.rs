//! The `/connect` panel: the provider list and the credentials form of a
//! built-in provider (port of the first two views of `ProviderPanel`; OAuth
//! sign-in, custom providers and model editing are not built yet).

use crate::model::session::{OauthEvent, ProviderAction, ProviderEntry};
use crate::ui::composer::palette::{footer, title_plain};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::oauth_view::OauthView;
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{IDLE_MARKER, SELECTED_MARKER, bordered};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::transcript::blocks::{display_width, truncate_cells};

const DIM_OAUTH_ID: &str = "dimcode-api-oauth";
const SUGGESTED_KEY: &str = "__suggested__";
const DIM_OAUTH_KEY: &str = "__dim_oauth__";
const PROVIDERS_KEY: &str = "__providers__";
const ADD_CUSTOM_KEY: &str = "__add_custom__";
const LIST_HINT: &str = "type filter · enter open · esc clear/close";
const FORM_HINT: &str = "type value · ↑↓/tab move · enter activate · esc back";

pub struct ProvidersPanel {
    list: ListPanel,
    providers: Vec<ProviderEntry>,
    form: Option<Form>,
    oauth: Option<OauthView>,
    attempts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    BaseUrl,
    ApiKey,
    Apply,
    Disconnect,
    ToggleEnabled,
}

struct Form {
    provider_id: String,
    connected: bool,
    enabled: bool,
    base_url: String,
    api_key: String,
    api_key_preview: Option<String>,
    active: usize,
    message: Option<String>,
    busy: bool,
}

impl Form {
    fn rows(&self) -> Vec<Field> {
        let mut rows = vec![Field::BaseUrl, Field::ApiKey];
        if !self.api_key.trim().is_empty() {
            rows.push(Field::Apply);
        }
        if self.connected {
            rows.push(Field::Disconnect);
        }
        rows.push(Field::ToggleEnabled);
        rows
    }
}

impl ProvidersPanel {
    pub fn new() -> Self {
        Self {
            list: ListPanel::new(
                "Providers",
                LIST_HINT,
                "Loading providers...",
                "No matching providers.",
                "No matching providers.",
                false,
            )
            .with_inline_rows(),
            providers: Vec::new(),
            form: None,
            oauth: None,
            attempts: 0,
        }
    }

    pub fn set_providers(&mut self, providers: Result<Vec<ProviderEntry>, String>) {
        let items = providers
            .as_ref()
            .map(|list| build_rows(list))
            .map_err(Clone::clone);
        if let Ok(providers) = providers {
            self.providers = providers;
            // A form of a provider that changed shows its new state.
            if let Some(form) = &mut self.form
                && let Some(entry) = self.providers.iter().find(|p| p.id == form.provider_id)
            {
                form.connected = entry.status == "connected";
                form.enabled = entry.enabled;
                form.api_key_preview = entry.api_key_preview.clone();
                form.busy = false;
            }
        }
        let preselect = None;
        self.list.set_items(items, preselect);
    }

    /// The outcome of an action: shown in the form, or a refreshed list.
    pub fn set_result(&mut self, result: Result<String, String>) {
        if let Some(form) = &mut self.form {
            form.busy = false;
            let failed = result.is_err();
            form.message = Some(match result {
                Ok(message) => message,
                Err(error) => format!("Error: {error}"),
            });
            // The key is kept on a failure (retry) and cleared once stored.
            if !failed {
                form.api_key.clear();
            }
        }
    }

    /// A sign-in update: shown by the OAuth step if it is the live attempt.
    pub fn oauth_event(&mut self, attempt: u64, event: OauthEvent) -> Effect {
        match &mut self.oauth {
            Some(view) => view.event(attempt, event),
            None => Effect::None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some(view) = &mut self.oauth {
            return match view.handle_key(key) {
                Effect::OauthBack => {
                    self.oauth = None;
                    Effect::None
                }
                Effect::OauthCancel => {
                    self.oauth = None;
                    Effect::OauthCancel
                }
                other => other,
            };
        }
        if self.form.is_some() {
            return self.form_key(key);
        }
        match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(id) => self.open(&id),
        }
    }

    fn open(&mut self, id: &str) -> Effect {
        match id {
            DIM_OAUTH_KEY => self.open_oauth(DIM_OAUTH_ID, "DimAgent"),
            ADD_CUSTOM_KEY => Effect::Notice("Custom providers are not available yet".to_owned()),
            provider_id => {
                let Some(entry) = self.providers.iter().find(|p| p.id == provider_id) else {
                    return Effect::None;
                };
                if matches!(entry.id.as_str(), "openai-codex" | "xai-grok-build")
                    && entry.status == "disconnected"
                {
                    let (id, name) = (entry.id.clone(), entry.name.clone());
                    return self.open_oauth(&id, &name);
                }
                self.form = Some(Form {
                    provider_id: entry.id.clone(),
                    connected: entry.status == "connected",
                    enabled: entry.enabled,
                    base_url: entry.base_url.clone().unwrap_or_default(),
                    api_key: String::new(),
                    api_key_preview: entry.api_key_preview.clone(),
                    active: 1,
                    message: None,
                    busy: false,
                });
                Effect::None
            }
        }
    }

    fn open_oauth(&mut self, provider_id: &str, name: &str) -> Effect {
        let mut view = OauthView::new(provider_id, name, self.attempts);
        let effect = view.auto_start();
        self.attempts += 1_000;
        self.oauth = Some(view);
        effect
    }

    fn form_key(&mut self, key: KeyEvent) -> Effect {
        let Some(form) = &mut self.form else {
            return Effect::None;
        };
        if form.busy {
            return Effect::None;
        }
        let rows = form.rows();
        let count = rows.len();
        let ctrl = key.mods.contains(Modifiers::CTRL);
        match key.code {
            KeyCode::Esc => {
                self.form = None;
                Effect::None
            }
            KeyCode::Up | KeyCode::BackTab => {
                form.active = (form.active + count - 1) % count;
                Effect::None
            }
            KeyCode::Down | KeyCode::Tab => {
                form.active = (form.active + 1) % count;
                Effect::None
            }
            KeyCode::Backspace => {
                match rows.get(form.active) {
                    Some(Field::BaseUrl) => {
                        form.base_url.pop();
                    }
                    Some(Field::ApiKey) => {
                        form.api_key.pop();
                    }
                    _ => {}
                }
                Effect::None
            }
            KeyCode::Char(c) if !ctrl && !key.mods.contains(Modifiers::ALT) => {
                match rows.get(form.active) {
                    Some(Field::BaseUrl) => form.base_url.push(c),
                    Some(Field::ApiKey) => form.api_key.push(c),
                    _ => {}
                }
                Effect::None
            }
            KeyCode::Enter => {
                let Some(field) = rows.get(form.active).copied() else {
                    return Effect::None;
                };
                match field {
                    Field::BaseUrl | Field::ApiKey => {
                        form.active = (form.active + 1) % count;
                        Effect::None
                    }
                    Field::Apply => {
                        form.busy = true;
                        form.message = Some("Testing connection...".to_owned());
                        Effect::Provider(ProviderAction::Configure {
                            provider_id: form.provider_id.clone(),
                            api_key: Some(form.api_key.trim().to_owned()),
                            base_url: Some(form.base_url.trim().to_owned())
                                .filter(|u| !u.is_empty()),
                            enabled: Some(true),
                        })
                    }
                    Field::Disconnect => {
                        form.busy = true;
                        Effect::Provider(ProviderAction::Disconnect(form.provider_id.clone()))
                    }
                    Field::ToggleEnabled => {
                        form.busy = true;
                        Effect::Provider(ProviderAction::Configure {
                            provider_id: form.provider_id.clone(),
                            api_key: None,
                            base_url: None,
                            enabled: Some(!form.enabled),
                        })
                    }
                }
            }
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        if let Some(view) = &self.oauth {
            return view.render(width, p);
        }
        let Some(form) = &self.form else {
            return self.list.render(width, rows, p);
        };
        let inner = width.saturating_sub(4).max(1);
        let fields = form.rows();
        let mut content = vec![
            title_plain(&format!("Connect {}", form.provider_id), inner, p),
            String::new(),
        ];
        let last_input = fields
            .iter()
            .rposition(|f| matches!(f, Field::BaseUrl | Field::ApiKey))
            .unwrap_or(0);
        for (index, field) in fields.iter().enumerate() {
            let focused = index == form.active;
            content.push(match field {
                Field::BaseUrl => input_row(
                    "Base URL",
                    &form.base_url,
                    "(default)",
                    focused,
                    false,
                    inner,
                    p,
                ),
                Field::ApiKey => {
                    let shown = if form.api_key.is_empty() {
                        form.api_key_preview.clone().unwrap_or_default()
                    } else {
                        mask(&form.api_key)
                    };
                    input_row("API Key", &shown, "(required)", focused, true, inner, p)
                }
                Field::Apply => action_row("Apply + test", focused, inner, p),
                Field::Disconnect => action_row("Disconnect", focused, inner, p),
                Field::ToggleEnabled => action_row(
                    if form.enabled {
                        "Disable provider"
                    } else {
                        "Enable provider"
                    },
                    focused,
                    inner,
                    p,
                ),
            });
            if index == last_input {
                content.push(String::new());
            }
        }
        if let Some(message) = &form.message {
            // An error is one more row under the actions, in the danger colour.
            let color = if message.starts_with("Error:") {
                &p.danger
            } else {
                &p.muted
            };
            content.push(format!(
                "{IDLE_MARKER}{}",
                fg(&truncate_cells(message, inner.saturating_sub(2)), color)
            ));
        }
        content.push(String::new());
        content.push(fg(&truncate_cells(FORM_HINT, inner), &p.muted));
        let _ = (footer, display_width);
        bordered(&content, width, None, &p.border, None)
    }
}

fn build_rows(providers: &[ProviderEntry]) -> Vec<Item> {
    let mut rows = Vec::new();
    let dim = providers.iter().find(|p| p.id == DIM_OAUTH_ID);
    let mut suggested = Item::heading("Suggested");
    suggested.id = SUGGESTED_KEY.into();
    suggested.sticky = true;
    rows.push(suggested);
    let detail = match dim {
        Some(entry) if entry.status == "connected" => {
            if entry.enabled {
                "Connected"
            } else {
                "disabled"
            }
        }
        _ => "Browser sign-in · no API key",
    };
    let mut oauth = Item::new(DIM_OAUTH_KEY, "Connect with DimAgent (OAuth)", detail);
    oauth.search = "dim dimagent oauth connect".into();
    rows.push(oauth);
    let mut heading = Item::heading("Providers");
    heading.id = PROVIDERS_KEY.into();
    heading.sticky = true;
    rows.push(heading);
    // A repeated display name gets a ` (n)` suffix, counted over the full
    // list so the alias stays put while the filter narrows the rows.
    let mut seen: std::collections::HashMap<&str, usize> = Default::default();
    for provider in providers.iter().filter(|p| p.id != DIM_OAUTH_ID) {
        let count = seen.entry(provider.name.as_str()).or_insert(0);
        *count += 1;
        let label = if *count > 1 {
            format!("{} ({count})", provider.name)
        } else {
            provider.name.clone()
        };
        let detail = if provider.is_current {
            "current"
        } else if matches!(provider.id.as_str(), "openai-codex" | "xai-grok-build")
            && provider.status == "disconnected"
        {
            "Sign in"
        } else {
            match provider.status.as_str() {
                "connected" => "ready",
                "disabled" => "disabled",
                "error" => "error",
                _ => "off",
            }
        };
        let mut row = Item::new(provider.id.clone(), label, detail);
        row.search = format!("{} {}", provider.id, provider.driver);
        rows.push(row);
    }
    let mut custom = Item::new(ADD_CUSTOM_KEY, "Add custom provider", "");
    custom.sticky = true;
    rows.push(custom);
    rows
}

fn mask(value: &str) -> String {
    "•".repeat(value.chars().count().clamp(4, 12))
}

pub(crate) fn input_row(
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
    secret_preview: bool,
    inner: usize,
    p: &Palette,
) -> String {
    let _ = secret_preview;
    let marker = if focused {
        paint(
            SELECTED_MARKER,
            Paint {
                fg: Some(&p.accent),
                bold: true,
                ..Paint::default()
            },
        )
    } else {
        IDLE_MARKER.to_owned()
    };
    let text = format!("{label}: ");
    let label = paint(
        &text,
        Paint {
            fg: Some(if focused { &p.accent } else { &p.muted }),
            bold: focused,
            ..Paint::default()
        },
    );
    let room = inner
        .saturating_sub(2 + display_width(&text) + usize::from(focused))
        .max(1);
    let caret = if focused {
        paint(
            " ",
            Paint {
                bg: Some(&p.accent),
                ..Paint::default()
            },
        )
    } else {
        String::new()
    };
    if value.is_empty() {
        let hint = fg(&truncate_cells(placeholder, room), &p.faint);
        return format!("{marker}{label}{caret}{hint}");
    }
    let chars: Vec<char> = value.chars().collect();
    let shown: String = if display_width(value) <= room {
        value.to_owned()
    } else {
        let tail: String = chars[chars.len().saturating_sub(room.saturating_sub(1))..]
            .iter()
            .collect();
        format!("…{tail}")
    };
    format!("{marker}{label}{}{caret}", fg(&shown, &p.text))
}

pub(crate) fn action_row(label: &str, focused: bool, inner: usize, p: &Palette) -> String {
    let marker = if focused {
        paint(
            SELECTED_MARKER,
            Paint {
                fg: Some(&p.accent),
                bold: true,
                ..Paint::default()
            },
        )
    } else {
        IDLE_MARKER.to_owned()
    };
    let text = truncate_cells(label, inner.saturating_sub(2).max(1));
    let styled = paint(
        &text,
        Paint {
            fg: Some(if focused { &p.accent } else { &p.text }),
            bold: focused,
            ..Paint::default()
        },
    );
    format!("{marker}{styled}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str, status: &str, current: bool) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            name: name.into(),
            driver: "openai".into(),
            status: status.into(),
            is_current: current,
            enabled: status != "disabled",
            custom: false,
            base_url: None,
            api_key_preview: None,
        }
    }

    #[test]
    fn rows_follow_the_reference_layout_and_details() {
        let rows = build_rows(&[
            entry("mock", "mock", "connected", true),
            entry("a", "Alpha", "disabled", false),
            entry("b", "Alpha", "connected", false),
            entry("openai-codex", "Codex", "disconnected", false),
        ]);
        let labels: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.label.as_str(), r.detail.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Suggested", ""),
                (
                    "Connect with DimAgent (OAuth)",
                    "Browser sign-in · no API key"
                ),
                ("Providers", ""),
                ("mock", "current"),
                ("Alpha", "disabled"),
                ("Alpha (2)", "ready"),
                ("Codex", "Sign in"),
                ("Add custom provider", ""),
            ]
        );
        assert!(rows[0].heading && rows[2].heading);
    }

    #[test]
    fn the_form_shows_the_actions_the_state_allows() {
        let mut form = Form {
            provider_id: "p".into(),
            connected: false,
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            api_key_preview: None,
            active: 1,
            message: None,
            busy: false,
        };
        assert_eq!(
            form.rows(),
            vec![Field::BaseUrl, Field::ApiKey, Field::ToggleEnabled]
        );
        form.api_key = "k".into();
        form.connected = true;
        assert_eq!(
            form.rows(),
            vec![
                Field::BaseUrl,
                Field::ApiKey,
                Field::Apply,
                Field::Disconnect,
                Field::ToggleEnabled
            ]
        );
    }
}
