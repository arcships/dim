//! The `/connect` panel: the provider list and the credentials form of a
//! static provider. The management snapshot supplies safe connection fields
//! and the record version used for partial writes.

use crate::model::session::{
    ACCOUNT_NAMESPACE, Account, OauthEvent, ProviderAction, ProviderEntry, Usage,
};
use crate::ui::i18n::{t, tf};
use crate::ui::line_input::{LineInput, window};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::oauth_view::OauthView;
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{IDLE_MARKER, SELECTED_MARKER, bordered, key_hints};
use crate::ui::rows::title_plain;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::{display_width, truncate_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};

const PROVIDERS_KEY: &str = "__providers__";
const ADD_CUSTOM_KEY: &str = "__add_custom__";
const LIST_HINT: &str = "Enter open · Esc clear/close";
const FORM_HINT: &str = "↑↓/Tab move · Enter activate · Ctrl+R reread · Esc back";

pub struct ProvidersPanel {
    list: ListPanel,
    providers: Vec<ProviderEntry>,
    form: Option<Form>,
    oauth: Option<OauthView>,
    attempts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    BaseUrl,
    ApiKey,
    Apply,
    Disconnect,
    ToggleEnabled,
}

struct Form {
    provider_id: String,
    version: Option<u32>,
    original_base_url: String,
    /// `Add custom provider`: the display name field; the record does not
    /// exist until its connection has been probed successfully.
    name: Option<LineInput>,
    connected: bool,
    enabled: bool,
    /// Line editors, like the composer's: cursor keys, Home/End, Delete,
    /// pastes; the hardware cursor sits in the focused one.
    base_url: LineInput,
    api_key: LineInput,
    api_key_preview: Option<String>,
    active: usize,
    /// The last outcome: a notice, or an error shown in the danger colour.
    message: Option<Result<String, String>>,
    busy: bool,
}

impl Form {
    fn rows(&self) -> Vec<Field> {
        if let Some(name) = &self.name {
            let mut rows = vec![Field::Name, Field::BaseUrl, Field::ApiKey];
            if [name, &self.base_url, &self.api_key]
                .iter()
                .all(|field| !field.text().trim().is_empty())
            {
                rows.push(Field::Apply);
            }
            return rows;
        }
        let mut rows = vec![Field::BaseUrl, Field::ApiKey];
        if self.version.is_none() {
            return rows;
        }
        if !self.api_key.text().trim().is_empty()
            || self.base_url.text().trim() != self.original_base_url
        {
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
                t("Providers"),
                t(LIST_HINT),
                t("Loading providers..."),
                t("No matching providers."),
                t("No matching providers."),
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
        if let Err(error) = &providers
            && let Some(form) = &mut self.form
        {
            form.busy = false;
            form.message = Some(Err(error.clone()));
        }
        if let Ok(providers) = providers {
            self.providers = providers;
            if let Some(form) = &mut self.form
                && let Some(entry) = self.providers.iter().find(|p| p.id == form.provider_id)
            {
                let failed = matches!(form.message, Some(Err(_)));
                let edited = !form.api_key.text().is_empty()
                    || form.base_url.text().trim() != form.original_base_url;
                if failed && form.version != Some(entry.version) {
                    form.message = Some(Err(t(
                        "Provider changed; review the retained draft and press Apply again",
                    )
                    .into()));
                }
                form.connected = entry.status == "connected";
                form.enabled = entry.enabled;
                form.version = Some(entry.version);
                form.original_base_url = entry.base_url.clone().unwrap_or_default();
                if !failed && !edited {
                    form.base_url = LineInput::new(&form.original_base_url);
                }
                if form.name.take().is_some() {
                    form.active = 2;
                }
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
            let failed = result.is_err();
            form.busy = false;
            form.message = Some(result);
            if !failed {
                form.api_key.clear();
                if form.name.take().is_some() {
                    form.connected = true;
                    form.active = 1;
                }
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

    pub fn native_selection(&self) -> bool {
        self.oauth.as_ref().is_some_and(OauthView::native_selection)
    }

    /// The Dim account's state from the host, shown while its panel is open.
    pub fn set_account(&mut self, account: Result<Option<Account>, String>) {
        if let Some(view) = self.oauth.as_mut().filter(|view| view.dim_panel()) {
            view.set_account(account);
        }
    }

    /// The Dim account's usage from the host.
    pub fn set_usage(&mut self, usage: Result<Usage, String>) {
        if let Some(view) = self.oauth.as_mut().filter(|view| view.dim_panel()) {
            view.set_usage(usage);
        }
    }

    /// The provider id of the open Dim panel.
    pub fn dim_provider_id(&self) -> Option<&str> {
        self.oauth
            .as_ref()
            .filter(|view| view.dim_panel())
            .map(OauthView::provider_id)
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
                Effect::Logout(target) => {
                    self.oauth = None;
                    Effect::Logout(target)
                }
                other => other,
            };
        }
        if self.form.is_some() {
            return self.form_key(key);
        }
        match self.list.handle_key(key) {
            Event::None => Effect::None,
            Event::Refresh => Effect::Refresh,
            Event::Close => Effect::Close,
            Event::Pick(id) => self.open(&id),
        }
    }

    fn open(&mut self, id: &str) -> Effect {
        match id {
            ADD_CUSTOM_KEY => {
                self.form = Some(Form {
                    provider_id: uuid::Uuid::new_v4().to_string(),
                    version: None,
                    original_base_url: String::new(),
                    name: Some(LineInput::new("")),
                    connected: false,
                    enabled: true,
                    base_url: LineInput::new(""),
                    api_key: LineInput::new(""),
                    api_key_preview: None,
                    active: 0,
                    message: None,
                    busy: false,
                });
                Effect::None
            }
            provider_id => {
                let Some(entry) = self.providers.iter().find(|p| p.id == provider_id) else {
                    return Effect::None;
                };
                if entry.account.is_some() {
                    let entry = entry.clone();
                    return self.open_oauth(&entry);
                }
                self.form = Some(Form {
                    provider_id: entry.id.clone(),
                    version: Some(entry.version),
                    original_base_url: entry.base_url.clone().unwrap_or_default(),
                    name: None,
                    connected: entry.status == "connected",
                    enabled: entry.enabled,
                    base_url: LineInput::new(entry.base_url.as_deref().unwrap_or_default()),
                    api_key: LineInput::new(""),
                    api_key_preview: entry.api_key_preview.clone(),
                    active: 1,
                    message: None,
                    busy: false,
                });
                Effect::None
            }
        }
    }

    fn open_oauth(&mut self, entry: &ProviderEntry) -> Effect {
        let account = entry
            .account
            .as_ref()
            .expect("only a described account opens the sign-in step");
        let dim = account.namespace == ACCOUNT_NAMESPACE;
        let mut view = if dim {
            OauthView::dim(&entry.id, &entry.name, self.attempts)
        } else {
            OauthView::described(
                &entry.id,
                account,
                &entry.name,
                entry.status == "connected",
                self.attempts,
            )
        };
        let effect = if dim {
            Effect::LoginStatus(entry.id.clone())
        } else {
            view.auto_start()
        };
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
        if key.mods == Modifiers::CTRL && key.code == KeyCode::Char('r') {
            form.busy = true;
            return Effect::Refresh;
        }
        let rows = form.rows();
        let count = rows.len();
        let edited = match rows.get(form.active) {
            Some(Field::Name) => form.name.as_mut().is_some_and(|name| name.handle_key(key)),
            Some(Field::BaseUrl) => form.base_url.handle_key(key),
            Some(Field::ApiKey) => form.api_key.handle_key(key),
            _ => false,
        };
        if edited {
            return Effect::None;
        }
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
            KeyCode::Enter => {
                let Some(field) = rows.get(form.active).copied() else {
                    return Effect::None;
                };
                match field {
                    Field::Name | Field::BaseUrl | Field::ApiKey => {
                        form.active = (form.active + 1) % count;
                        Effect::None
                    }
                    Field::Apply if form.name.is_some() => {
                        let name = form
                            .name
                            .as_ref()
                            .map_or("", |n| n.text().trim())
                            .to_owned();
                        let provider_id = form.provider_id.clone();
                        form.busy = true;
                        form.message = Some(Ok(t("Testing connection...").to_owned()));
                        Effect::Provider(ProviderAction::Create {
                            provider_id,
                            name,
                            base_url: form.base_url.text().trim().to_owned(),
                            api_key: form.api_key.text().trim().to_owned(),
                        })
                    }
                    Field::Apply => {
                        form.busy = true;
                        form.message = Some(Ok(t("Testing connection...").to_owned()));
                        Effect::Provider(ProviderAction::Configure {
                            provider_id: form.provider_id.clone(),
                            expected_version: form
                                .version
                                .expect("Existing form has a record version"),
                            api_key: Some(form.api_key.text().trim().to_owned())
                                .filter(|key| !key.is_empty()),
                            base_url: Some(form.base_url.text().trim().to_owned())
                                .filter(|url| url != &form.original_base_url),
                            enabled: (!form.enabled).then_some(true),
                        })
                    }
                    Field::Disconnect => {
                        form.busy = true;
                        Effect::Provider(ProviderAction::Disconnect {
                            provider_id: form.provider_id.clone(),
                            expected_version: form
                                .version
                                .expect("Existing form has a record version"),
                        })
                    }
                    Field::ToggleEnabled => {
                        form.busy = true;
                        Effect::Provider(ProviderAction::Configure {
                            provider_id: form.provider_id.clone(),
                            expected_version: form
                                .version
                                .expect("Existing form has a record version"),
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

    /// A paste goes into the focused field.
    pub fn paste(&mut self, text: &str) {
        if let Some(view) = &mut self.oauth {
            view.paste(text);
            return;
        }
        if let Some(form) = &mut self.form
            && !form.busy
        {
            match form.rows().get(form.active) {
                Some(Field::Name) => {
                    if let Some(name) = &mut form.name {
                        name.paste(text);
                    }
                }
                Some(Field::BaseUrl) => form.base_url.paste(text),
                Some(Field::ApiKey) => form.api_key.paste(text),
                _ => {}
            }
        }
    }

    /// The list's filter caret, or the form's focused field's; the OAuth
    /// step draws its own.
    pub fn cursor(&self, width: usize, rows: usize) -> Option<(usize, usize)> {
        if let Some(view) = &self.oauth {
            return view.cursor(width, rows);
        }
        let Some(form) = &self.form else {
            return self.list.cursor(width);
        };
        let inner = width.saturating_sub(4).max(1);
        let fields = form.rows();
        let (label, column) = match fields.get(form.active)? {
            Field::Name => (
                "Name",
                form.name.as_ref()?.view(field_room(inner, "Name")).1,
            ),
            Field::BaseUrl => (
                "Base URL",
                form.base_url.view(field_room(inner, "Base URL")).1,
            ),
            Field::ApiKey => (
                "API Key",
                masked(&form.api_key, field_room(inner, "API Key")).1,
            ),
            _ => return None,
        };
        Some((
            2 + form.active,
            2 + display_width(SELECTED_MARKER) + display_width(label) + 2 + column,
        ))
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        if let Some(view) = &self.oauth {
            return view.render(width, rows, p);
        }
        let Some(form) = &self.form else {
            return self.list.render(width, rows, p);
        };
        let inner = width.saturating_sub(4).max(1);
        let fields = form.rows();
        let title = title_plain(
            &if form.name.is_some() {
                t("Add custom provider").to_owned()
            } else {
                tf("Connect {providerId}", &[("providerId", &form.provider_id)])
            },
            inner,
            p,
        );
        let mut content = vec![String::new()];
        let last_input = fields
            .iter()
            .rposition(|f| matches!(f, Field::Name | Field::BaseUrl | Field::ApiKey))
            .unwrap_or(0);
        for (index, field) in fields.iter().enumerate() {
            let focused = index == form.active;
            content.push(match field {
                Field::Name => {
                    let shown = form
                        .name
                        .as_ref()
                        .map(|name| name.view(field_room(inner, "Name")).0)
                        .unwrap_or_default();
                    input_row("Name", &shown, t("(required)"), focused, inner, p)
                }
                Field::BaseUrl => {
                    let (shown, _) = form.base_url.view(field_room(inner, "Base URL"));
                    let placeholder = t(if form.name.is_some() {
                        "(required)"
                    } else {
                        "(default)"
                    });
                    input_row("Base URL", &shown, placeholder, focused, inner, p)
                }
                Field::ApiKey => {
                    let (shown, _) = if form.api_key.is_empty() {
                        (form.api_key_preview.clone().unwrap_or_default(), 0)
                    } else {
                        masked(&form.api_key, field_room(inner, "API Key"))
                    };
                    input_row("API Key", &shown, t("(required)"), focused, inner, p)
                }
                Field::Apply => action_row(t("Test + apply"), focused, inner, p),
                Field::Disconnect => action_row(t("Disconnect"), focused, inner, p),
                Field::ToggleEnabled => action_row(
                    t(if form.enabled {
                        "Disable provider"
                    } else {
                        "Enable provider"
                    }),
                    focused,
                    inner,
                    p,
                ),
            });
            if index == last_input {
                content.push(String::new());
            }
        }
        if form.api_key_preview.is_some()
            && form.api_key.is_empty()
            && form.base_url.text().trim() != form.original_base_url
        {
            content.push(truncate_cells(
                t("Testing sends the stored credential to this URL."),
                inner,
            ));
        }
        if let Some(message) = &form.message {
            let (text, color) = match message {
                Ok(notice) => (notice.clone(), &p.muted),
                Err(error) => (tf("Error: {error}", &[("error", error)]), &p.danger),
            };
            content.push(format!(
                "{IDLE_MARKER}{}",
                fg(&truncate_cells(&text, inner.saturating_sub(2)), color)
            ));
        }
        let hint = key_hints(t(FORM_HINT), p);
        bordered(&content, width, None, &p.border, Some(&title), Some(&hint))
    }
}

fn build_rows(providers: &[ProviderEntry]) -> Vec<Item> {
    let mut rows = Vec::new();
    let mut heading = Item::heading(t("Providers"));
    heading.id = PROVIDERS_KEY.into();
    heading.sticky = true;
    rows.push(heading);
    let mut seen: std::collections::HashMap<&str, usize> = Default::default();
    for provider in providers {
        let count = seen.entry(provider.name.as_str()).or_insert(0);
        *count += 1;
        let label = if *count > 1 {
            format!("{} ({count})", provider.name)
        } else {
            provider.name.clone()
        };
        let signs_in = provider
            .account
            .as_ref()
            .is_some_and(|account| !account.login_methods.is_empty());
        let detail = t(if provider.is_current {
            "current"
        } else if signs_in && provider.status == "disconnected" {
            "Sign in"
        } else {
            match provider.status.as_str() {
                "connected" => "ready",
                "disabled" => "disabled",
                "error" => "error",
                _ => "not connected",
            }
        });
        let mut row = Item::new(provider.id.clone(), label, detail);
        row.search = format!("{} {}", provider.id, provider.driver);
        rows.push(row);
    }
    let mut custom = Item::new(ADD_CUSTOM_KEY, t("Add custom provider"), "");
    custom.sticky = true;
    rows.push(custom);
    rows
}

/// Cells a field's text may use: the row less the marker and the label.
pub(crate) fn field_room(inner: usize, label: &str) -> usize {
    inner
        .saturating_sub(display_width(SELECTED_MARKER) + display_width(label) + 2)
        .max(1)
}

/// A secret's window: one bullet per character, the cursor among them.
fn masked(input: &LineInput, room: usize) -> (String, usize) {
    let bullets = "•".repeat(input.text().chars().count());
    let before = input.text()[..input.cursor()].chars().count();
    window(&bullets, before * "•".len(), room)
}

pub(crate) fn input_row(
    label: &str,
    shown: &str,
    placeholder: &str,
    focused: bool,
    inner: usize,
    p: &Palette,
) -> String {
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
    let room = inner.saturating_sub(2 + display_width(&text)).max(1);
    if shown.is_empty() {
        let hint = fg(&truncate_cells(placeholder, room), &p.faint);
        return format!("{marker}{label}{hint}");
    }
    format!(
        "{marker}{label}{}",
        fg(&truncate_cells(shown, room), &p.text)
    )
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
    use crate::ui::terminal::Modifiers;
    use crate::ui::theme::palette::{Appearance, ColorMode, terminal_palette};

    fn entry(id: &str, name: &str, status: &str, current: bool) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            version: 3,
            name: name.into(),
            driver: "openai".into(),
            status: status.into(),
            is_current: current,
            enabled: status != "disabled",
            custom: false,
            base_url: None,
            api_key_preview: None,
            account: None,
        }
    }

    fn described(
        id: &str,
        name: &str,
        namespace: &str,
        methods: &[&str],
        signed_in: bool,
    ) -> ProviderEntry {
        ProviderEntry {
            account: Some(crate::model::session::AccountDescriptor {
                namespace: namespace.into(),
                login_methods: methods.iter().map(|method| (*method).to_owned()).collect(),
            }),
            ..entry(
                id,
                name,
                if signed_in {
                    "connected"
                } else {
                    "disconnected"
                },
                false,
            )
        }
    }

    #[test]
    fn rows_follow_the_reference_layout_and_details() {
        let rows = build_rows(&[
            entry("mock", "mock", "connected", true),
            entry("a", "Alpha", "disabled", false),
            entry("b", "Alpha", "connected", false),
            described(
                "openai-codex",
                "Codex",
                "account.codex",
                &["browser"],
                false,
            ),
            described(
                "xai-grok-build",
                "Grok Build",
                "account.grok",
                &["device"],
                false,
            ),
            described("readonly", "Read-only", "account.none", &[], false),
            entry("api", "API key", "disconnected", false),
        ]);
        let labels: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.label.as_str(), r.detail.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Providers", ""),
                ("mock", "current"),
                ("Alpha", "disabled"),
                ("Alpha (2)", "ready"),
                ("Codex", "Sign in"),
                ("Grok Build", "Sign in"),
                ("Read-only", "not connected"),
                ("API key", "not connected"),
                ("Add custom provider", ""),
            ]
        );
        assert!(rows[0].heading);
    }

    /// A described account opens its sign-in step, signed in or not; a
    /// provider without a descriptor keeps the connection form.
    #[test]
    fn a_described_account_opens_sign_in_and_others_keep_the_form() {
        let mut panel = ProvidersPanel::new();
        panel.set_providers(Ok(vec![
            described(
                "xai-grok-build",
                "Grok Build",
                "account.grok",
                &["device"],
                false,
            ),
            described("openai-codex", "Codex", "account.codex", &["browser"], true),
            entry("api", "API key", "connected", false),
        ]));
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        assert_eq!(panel.list.selected_id().as_deref(), Some("xai-grok-build"));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::OauthStart(start) if start.provider_id == "xai-grok-build"
                && start.namespace == "account.grok"
                && start.device
        ));
        panel.oauth = None;
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::Logout(target) if target.provider_id == "openai-codex"
        ));
        assert!(panel.oauth.is_none());
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(panel.form.is_some());
    }

    /// The managed `account` namespace opens the Dim account's own panel;
    /// every other namespace opens the generic sign-in step.
    #[test]
    fn the_account_namespace_opens_the_dim_panel() {
        let mut panel = ProvidersPanel::new();
        panel.set_providers(Ok(vec![
            described(
                "dimcode-api-oauth",
                "DimAgent",
                "account",
                &["device"],
                false,
            ),
            described(
                "openai-codex",
                "Codex",
                "account.codex",
                &["browser"],
                false,
            ),
        ]));
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        assert_eq!(
            panel.list.selected_id().as_deref(),
            Some("dimcode-api-oauth")
        );
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::LoginStatus(id) if id == "dimcode-api-oauth"
        ));
        let p = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let rendered = panel.render(60, 16, &p).join("\n");
        assert!(rendered.contains("Login with DimAgent"), "{rendered}");
        assert!(rendered.contains("Enterprise login"), "{rendered}");
        panel.oauth = None;
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::OauthStart(start) if start.provider_id == "openai-codex"
                && start.namespace == "account.codex"
                && !start.device
        ));
    }

    #[test]
    fn the_form_shows_the_actions_the_state_allows() {
        let mut form = Form {
            provider_id: "p".into(),
            version: Some(3),
            original_base_url: String::new(),
            name: None,
            connected: false,
            enabled: false,
            base_url: LineInput::new(""),
            api_key: LineInput::new(""),
            api_key_preview: None,
            active: 1,
            message: None,
            busy: false,
        };
        assert_eq!(
            form.rows(),
            vec![Field::BaseUrl, Field::ApiKey, Field::ToggleEnabled]
        );
        form.api_key = LineInput::new("k");
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

    #[test]
    fn a_custom_provider_is_named_then_tested_then_connected() {
        let mut panel = ProvidersPanel::new();
        panel.set_providers(Ok(vec![entry("mock", "mock", "connected", true)]));
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        assert!(matches!(panel.handle_key(key(KeyCode::End)), Effect::None));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        let form = panel.form.as_ref().unwrap();
        let allocated_id = form.provider_id.clone();
        assert!(uuid::Uuid::parse_str(&allocated_id).is_ok());
        assert_eq!(
            form.rows(),
            vec![Field::Name, Field::BaseUrl, Field::ApiKey]
        );
        assert_eq!(panel.cursor(40, 24), Some((2, 2 + 2 + 4 + 2)));
        for c in "My LLM!".chars() {
            panel.handle_key(key(KeyCode::Char(c)));
        }
        panel.handle_key(key(KeyCode::Enter));
        panel.paste("http://localhost:8080/v1\n");
        panel.handle_key(key(KeyCode::Enter));
        panel.paste("k");
        assert_eq!(
            panel.form.as_ref().unwrap().rows(),
            vec![Field::Name, Field::BaseUrl, Field::ApiKey, Field::Apply]
        );
        let p = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let rendered = panel.render(60, 12, &p).join("\n");
        assert!(rendered.contains("Add custom provider"), "{rendered}");
        assert!(rendered.contains("http://localhost:8080/v1"), "{rendered}");
        panel.handle_key(key(KeyCode::Enter));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Provider(ProviderAction::Create {
                provider_id,
                name,
                base_url,
                api_key,
            }) => {
                assert_eq!(provider_id, allocated_id);
                assert_eq!(name, "My LLM!");
                assert_eq!(base_url, "http://localhost:8080/v1");
                assert_eq!(api_key, "k");
            }
            other => panic!("{other:?}"),
        }
        panel.set_result(Ok("Added My LLM!".into()));
        let mut saved = entry(&allocated_id, "My LLM!", "connected", false);
        saved.version = 1;
        saved.base_url = Some("http://localhost:8080/v1".into());
        panel.set_providers(Ok(vec![saved]));
        let form = panel.form.as_ref().unwrap();
        assert_eq!(form.version, Some(1));
        assert!(form.name.is_none());
        assert_eq!(form.provider_id, allocated_id);
        assert_eq!(
            form.rows(),
            vec![
                Field::BaseUrl,
                Field::ApiKey,
                Field::Disconnect,
                Field::ToggleEnabled
            ]
        );

        panel.handle_key(key(KeyCode::Up));
        panel.handle_key(KeyEvent::new(KeyCode::Char('u'), Modifiers::CTRL));
        panel.paste("http://draft.test/v1");
        panel.handle_key(key(KeyCode::Enter));
        panel.paste("retry-key");
        panel.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::Provider(_)
        ));
        panel.set_result(Err("Version conflict".into()));
        panel.set_providers(Err("List reply lost".into()));
        assert_eq!(
            panel.handle_key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CTRL)),
            Effect::Refresh
        );
        let mut reread = entry(&allocated_id, "My LLM!", "connected", false);
        reread.version = 2;
        reread.base_url = Some("http://other-client.test/v1".into());
        reread.api_key_preview = Some("stored".into());
        panel.set_providers(Ok(vec![reread]));
        assert!(
            panel
                .render(100, 15, &p)
                .join("\n")
                .contains("Provider changed")
        );
        assert!(matches!(panel.handle_key(key(KeyCode::Enter)),
            Effect::Provider(ProviderAction::Configure {expected_version: 2, api_key: Some(key), base_url: Some(url), ..})
            if key == "retry-key" && url == "http://draft.test/v1"));
    }
}
