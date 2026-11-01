//! Sign-in by browser or device code (port of `oauthView` / `oauthRows`):
//! the `/login` panel for Dim, and a described account's step of `/connect`.
//!
//! Dim's panel opens on a menu that depends on the account the host reports:
//!
//! ```text
//! signed out                      signed in
//! ╭──────────────────────────╮    ╭──────────────────────────────────────╮
//! │ DimAgent OAuth           │    │ DimAgent OAuth                       │
//! │                          │    │                                      │
//! │   Not signed in          │    │   golden · a@b.com                   │
//! │                          │    │   Internal · ¥0.10/month · active …  │
//! │ › Login with DimAgent    │    │                                      │
//! │   Enterprise login       │    │   Credits  25,000 / 300,000  8% …    │
//! │   Device login           │    │   ███░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
//! │                          │    │                                      │
//! │ ↑↓ move · enter · esc    │    │   Web search  211 / 1,500  14% …     │
//! ╰──────────────────────────╯    │   █████░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
//!                                 │                                      │
//!                                 │ › Open models                        │
//!                                 │   View plan                          │
//!                                 │   Sign out                           │
//!                                 │                                      │
//!                                 │ ↑↓ move · enter · ctrl+r · esc       │
//!                                 ╰──────────────────────────────────────╯
//! ```
//!
//! Each entry goes straight into its flow; the flow shows only its own
//! rows, and `Esc` after an error or a cancel returns to the menu. Signed
//! in, the account's usage is asked for once and again on `Ctrl+R`. A
//! described account starts its only flow on open when it is signed out, and
//! shows its state and a sign-out entry when it is signed in.

use crate::model::session::{
    ACCOUNT_NAMESPACE, Account as AccountInfo, AccountDescriptor, AccountTarget, OauthEvent,
    OauthStart, Usage,
};
use crate::ui::i18n::{t, tf};
use crate::ui::line_input::LineInput;
use crate::ui::prompts::Effect;
use crate::ui::providers_panel::{action_row, field_room, input_row};
use crate::ui::render::panel::{IDLE_MARKER, SELECTED_MARKER, bordered};
use crate::ui::rows::title_plain;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::{display_width as width_of, truncate_cells, wrap_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing started: the Dim entry rows, or a described account's state.
    Idle,
    /// The enterprise email prompt.
    Email,
    Starting,
    AwaitingCallback,
    AwaitingDevice,
    Exchanging,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Submit,
    Enterprise,
    DeviceTab,
    Models,
    Plan,
    Logout,
    BrowserTab,
    Email,
    Link,
    Input,
    RestartDevice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// A muted line of status.
    Section,
    /// A line drawn as given (already coloured).
    Styled,
    Action(Focus),
}

/// One usage bar: its label, used and total, whether the plan grants it
/// without limit, and when it resets or expires.
type Meter = (String, Option<(u64, u64)>, bool, Option<String>);

/// What the host reported about Dim's account.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Account {
    SignedOut,
    SignedIn(AccountInfo),
    Error(String),
}

pub struct OauthView {
    provider_id: String,
    /// The namespace the sign-in and sign-out methods belong to.
    namespace: String,
    /// The Dim account's panel: its account menu, usage and plan.
    dim: bool,
    name: String,
    /// The described account is signed in: it offers sign-out, not sign-in.
    signed_in: bool,
    device: bool,
    phase: Phase,
    focus: Focus,
    attempt: u64,
    /// Line editors, like the composer's; the hardware cursor sits in the
    /// focused one.
    email: LineInput,
    /// Dim's account, once the host reports it.
    account: Option<Account>,
    /// The account's usage: unasked, loading, or the host's answer.
    usage: Option<Result<Usage, String>>,
    usage_loading: bool,
    callback: LineInput,
    auth_url: Option<String>,
    device_text: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

impl OauthView {
    /// The Dim account's panel (`/login`), for the described account whose
    /// namespace is `account`.
    pub fn dim(provider_id: &str, name: &str, attempt: u64) -> Self {
        Self::new(
            provider_id,
            ACCOUNT_NAMESPACE,
            name,
            true,
            false,
            false,
            attempt,
        )
    }

    /// A provider's described account: one sign-in flow, chosen by its
    /// methods, and sign-out when it is signed in.
    pub fn described(
        provider_id: &str,
        account: &AccountDescriptor,
        name: &str,
        signed_in: bool,
        attempt: u64,
    ) -> Self {
        let device = !account
            .login_methods
            .iter()
            .any(|method| method == "browser");
        Self::new(
            provider_id,
            &account.namespace,
            name,
            false,
            signed_in,
            device,
            attempt,
        )
    }

    fn new(
        provider_id: &str,
        namespace: &str,
        name: &str,
        dim: bool,
        signed_in: bool,
        device: bool,
        attempt: u64,
    ) -> Self {
        let focus = if dim {
            Focus::Submit
        } else if signed_in {
            Focus::Logout
        } else if device {
            Focus::DeviceTab
        } else {
            Focus::BrowserTab
        };
        Self {
            provider_id: provider_id.to_owned(),
            namespace: namespace.to_owned(),
            dim,
            name: name.to_owned(),
            signed_in,
            device,
            phase: Phase::Idle,
            focus,
            attempt,
            email: LineInput::new(""),
            account: None,
            usage: None,
            usage_loading: false,
            callback: LineInput::new(""),
            auth_url: None,
            device_text: None,
            message: None,
            error: None,
        }
    }

    /// A described account that is not signed in begins its sign-in on
    /// open. The Dim panel keeps its menu.
    pub fn auto_start(&mut self) -> Effect {
        if self.dim || self.signed_in {
            Effect::None
        } else {
            self.start(self.device, None)
        }
    }

    /// The account state the host reported for `/login`: on open, and
    /// again after a sign-out from the panel. The menu follows it.
    pub fn set_account(&mut self, account: Result<Option<AccountInfo>, String>) {
        let account = match account {
            Ok(Some(info)) => Account::SignedIn(info),
            Ok(None) => Account::SignedOut,
            Err(error) => Account::Error(error),
        };
        if self.phase == Phase::Idle {
            self.focus = match account {
                Account::SignedIn(_) => Focus::Models,
                _ => Focus::Submit,
            };
        }
        self.usage_loading = matches!(account, Account::SignedIn(_));
        self.usage = None;
        self.account = Some(account);
    }

    /// The usage the host reported for the signed-in account.
    pub fn set_usage(&mut self, usage: Result<Usage, String>) {
        self.usage_loading = false;
        self.usage = Some(usage);
    }

    /// The signed-in account's plan page.
    fn plan_url(&self) -> Option<String> {
        match &self.account {
            Some(Account::SignedIn(info)) => Some(format!(
                "{}/console/subscription",
                info.issuer.trim_end_matches('/')
            )),
            _ => None,
        }
    }

    fn dim_signed_in(&self) -> bool {
        matches!(self.account, Some(Account::SignedIn(_)))
    }

    fn start(&mut self, device: bool, email: Option<String>) -> Effect {
        self.attempt += 1;
        self.device = device;
        self.phase = Phase::Starting;
        self.auth_url = None;
        self.device_text = None;
        self.message = None;
        self.error = None;
        self.callback.clear();
        self.focus = if device {
            Focus::DeviceTab
        } else {
            Focus::BrowserTab
        };
        Effect::OauthStart(OauthStart {
            attempt: self.attempt,
            provider_id: self.provider_id.clone(),
            namespace: self.namespace.clone(),
            device,
            email,
        })
    }

    /// The panel's rows at `width` columns; `p` colours the usage bars
    /// (without it the status rows are left out, which the focus order
    /// does not need).
    fn rows(&self, width: usize, p: Option<&Palette>) -> Vec<(Row, String)> {
        let mut rows: Vec<(Row, String)> = Vec::new();
        let dim = self.dim;
        if dim && self.phase == Phase::Idle {
            match (&self.account, p) {
                (_, None) => {}
                (None, _) => rows.push((Row::Section, t("Checking account…").to_owned())),
                (Some(Account::SignedOut), _) => {
                    rows.push((Row::Section, t("Not signed in").to_owned()))
                }
                (Some(Account::Error(error)), _) => {
                    rows.push((Row::Section, tf("Error: {error}", &[("error", error)])))
                }
                (Some(Account::SignedIn(info)), Some(p)) => {
                    rows.push((Row::Styled, fg(&identity_line(info), &p.text)));
                    rows.extend(self.usage_rows(width, p));
                }
            }
            rows.push((Row::Section, String::new()));
            if self.dim_signed_in() {
                rows.push((Row::Action(Focus::Models), t("Open models").into()));
                rows.push((Row::Action(Focus::Plan), t("View plan").into()));
                rows.push((Row::Action(Focus::Logout), t("Sign out").into()));
            } else {
                rows.push((Row::Action(Focus::Submit), t("Login with DimAgent").into()));
                rows.push((Row::Action(Focus::Enterprise), t("Enterprise login").into()));
                rows.push((Row::Action(Focus::DeviceTab), t("Device login").into()));
            }
            return rows;
        }
        if dim && self.phase == Phase::Email {
            rows.push((Row::Action(Focus::Email), t("Email").into()));
            rows.push((Row::Action(Focus::Submit), t("Continue").into()));
            return rows;
        }
        if !dim && self.signed_in && self.phase == Phase::Idle {
            rows.push((Row::Section, t("Signed in").to_owned()));
            rows.push((Row::Section, String::new()));
            rows.push((Row::Action(Focus::Logout), t("Sign out").into()));
            return rows;
        }
        if self.device {
            rows.push((Row::Action(Focus::DeviceTab), t("Device login").into()));
        } else {
            rows.push((Row::Action(Focus::BrowserTab), t("Browser login").into()));
        }
        if self.device {
            if let Some(text) = &self.device_text {
                rows.push((Row::Section, text.clone()));
            }
            if let Some(url) = &self.auth_url {
                rows.push((Row::Action(Focus::Link), url.clone()));
            }
            rows.push((
                Row::Action(Focus::RestartDevice),
                t("Restart device login").into(),
            ));
        } else if self.phase == Phase::AwaitingCallback {
            if let Some(url) = &self.auth_url {
                rows.push((Row::Action(Focus::Link), url.clone()));
            }
            rows.push((Row::Action(Focus::Input), t("Callback URL").into()));
            rows.push((Row::Action(Focus::Submit), t("Submit callback").into()));
        }
        rows
    }

    /// The plan line, then one bar per credit bucket, token window and
    /// feature meter, as the old TypeScript panel drew them.
    fn usage_rows(&self, width: usize, p: &Palette) -> Vec<(Row, String)> {
        let mut rows = Vec::new();
        let usage = match (&self.usage, self.usage_loading) {
            (Some(Ok(usage)), _) => usage,
            (Some(Err(reason)), _) => {
                rows.push((
                    Row::Section,
                    tf(
                        "Usage: {reason}",
                        &[("reason", &crate::sanitize::safe(reason))],
                    ),
                ));
                return rows;
            }
            (None, true) => {
                rows.push((Row::Section, t("Checking usage…").to_owned()));
                return rows;
            }
            (None, false) => return rows,
        };
        rows.extend(
            Self::render_usage(usage, width, p)
                .into_iter()
                .map(|text| (Row::Styled, text)),
        );
        rows
    }

    pub(super) fn render_usage(usage: &Usage, width: usize, p: &Palette) -> Vec<String> {
        let mut rows = Vec::new();
        if let Some(plan) = &usage.plan {
            let mut parts: Vec<String> = Vec::new();
            parts.extend(plan.name.clone());
            if let Some(amount) = plan.price_amount {
                let major = amount / 100.0;
                let mut price = match plan.price_currency.as_deref() {
                    Some("CNY") => format!("¥{major:.2}"),
                    Some("USD") => format!("${major:.2}"),
                    Some("EUR") => format!("€{major:.2}"),
                    Some(currency) => format!("{major:.2} {currency}"),
                    None => format!("{major:.2}"),
                };
                match plan.price_interval.as_deref() {
                    Some("month") => price.push_str(t("/month")),
                    Some("year") => price.push_str(t("/year")),
                    Some(interval) => {
                        price.push('/');
                        price.push_str(interval);
                    }
                    None => {}
                }
                parts.push(price);
            }
            parts.extend(plan.status.clone());
            if let Some(date) = &plan.expires_at {
                parts.push(tf("expires {date}", &[("date", short_date(date))]));
            }
            if !parts.is_empty() {
                let text = crate::sanitize::safe(&parts.join(" · ")).replace(['\n', '\t'], " ");
                rows.push(fg(&truncate_cells(&text, width), &p.muted));
            }
        }
        let mut meters: Vec<Meter> = Vec::new();
        if let Some(credits) = &usage.credits {
            let used = credits.used_units.or_else(|| {
                Some(
                    credits
                        .total_units?
                        .saturating_sub(credits.remaining_units?),
                )
            });
            let amount = used.zip(credits.total_units);
            let resets = credits
                .resets_at
                .as_deref()
                .map(|date| tf("resets {date}", &[("date", short_date(date))]));
            if amount.is_some() || credits.unlimited == Some(true) {
                meters.push((
                    t("Credits").to_owned(),
                    amount,
                    credits.unlimited == Some(true),
                    resets,
                ));
            }
        }
        for window in &usage.windows {
            let expires = window
                .expires_at
                .as_deref()
                .map(|date| tf("expires {date}", &[("date", short_date(date))]));
            meters.push((
                tf(
                    "{hours}h window",
                    &[("hours", &window.duration_hours.to_string())],
                ),
                Some((window.token_used, window.token_cap)),
                false,
                expires,
            ));
        }
        for feature in &usage.features {
            let used = feature
                .used
                .or_else(|| Some(feature.allowance?.saturating_sub(feature.remaining?)));
            let amount = used.zip(feature.allowance);
            let resets = feature
                .period_end
                .as_deref()
                .map(|date| tf("resets {date}", &[("date", short_date(date))]));
            if amount.is_some() || feature.unlimited == Some(true) {
                meters.push((
                    feature_label(&crate::sanitize::safe(&feature.key).replace(['\n', '\t'], " ")),
                    amount,
                    feature.unlimited == Some(true),
                    resets,
                ));
            }
        }
        if meters.is_empty() {
            return rows;
        }
        let label_width = meters
            .iter()
            .map(|(label, ..)| width_of(label))
            .max()
            .unwrap_or(0)
            .min(width);
        let gap = " ".repeat(width.saturating_sub(label_width).min(2));
        let room = width.saturating_sub(label_width + gap.len());
        for (label, amount, unlimited, when) in meters {
            rows.push(String::new());
            let label = truncate_cells(&label, label_width);
            let label = format!("{label}{}", " ".repeat(label_width - width_of(&label)));
            let Some((used, total)) = amount.filter(|_| !unlimited) else {
                rows.push(format!(
                    "{}{gap}{}",
                    fg(&label, &p.text),
                    fg(&truncate_cells(t("unlimited"), room), &p.muted)
                ));
                continue;
            };
            let ratio = if total == 0 {
                0.0
            } else {
                (used as f64 / total as f64).clamp(0.0, 1.0)
            };
            let mut tail = format!("{} / {}  {:.0}%", count(used), count(total), ratio * 100.0);
            if let Some(when) = when {
                tail.push_str(" · ");
                tail.push_str(&when);
            }
            let tail = crate::sanitize::safe(&tail).replace(['\n', '\t'], " ");
            rows.push(format!(
                "{}{gap}{}",
                fg(&label, &p.text),
                fg(&truncate_cells(&tail, room), &p.muted)
            ));
            let bar_width = width.min(40);
            let filled = (ratio * bar_width as f64).round() as usize;
            let tone = if ratio >= 0.9 { &p.danger } else { &p.accent };
            rows.push(format!(
                "{}{}",
                fg(&"█".repeat(filled), tone),
                fg(&"░".repeat(bar_width - filled), &p.faint)
            ));
        }
        rows
    }

    fn focus_order(&self) -> Vec<Focus> {
        self.rows(0, None)
            .into_iter()
            .filter_map(|(row, _)| match row {
                Row::Action(focus) => Some(focus),
                Row::Section | Row::Styled => None,
            })
            .collect()
    }

    pub fn native_selection(&self) -> bool {
        self.focus_order().contains(&Focus::Link)
    }

    /// This is the Dim account's panel, not one described account's step.
    pub fn dim_panel(&self) -> bool {
        self.dim
    }

    /// The provider id this panel's account belongs to.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn move_focus(&mut self, delta: isize) {
        let order = self.focus_order();
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(order.len() as isize) as usize;
        self.focus = order[next];
    }

    pub fn event(&mut self, attempt: u64, event: OauthEvent) -> Effect {
        if attempt != self.attempt {
            return Effect::None;
        }
        match event {
            OauthEvent::Url { url, opened } => {
                self.phase = Phase::AwaitingCallback;
                self.auth_url = Some(url);
                self.focus = Focus::Link;
                if !opened {
                    self.message = Some(
                        t("The browser could not be opened. Open the link manually, then return here.")
                            .to_owned(),
                    );
                }
                Effect::None
            }
            OauthEvent::Device {
                verification_uri,
                verification_uri_complete,
                user_code,
            } => {
                self.phase = Phase::AwaitingDevice;
                self.focus = Focus::Link;
                let target = verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| verification_uri.clone());
                self.device_text = Some(tf("Enter code:    {code}", &[("code", &user_code)]));
                self.auth_url = Some(target);
                self.message = Some(t("Waiting for authorization").to_owned());
                Effect::None
            }
            OauthEvent::Exchanging => {
                self.phase = Phase::Exchanging;
                self.message = None;
                Effect::None
            }
            OauthEvent::SlowDown(_) => {
                self.message = Some(t("Waiting after server slow-down").to_owned());
                Effect::None
            }
            OauthEvent::Done(Ok(_)) => Effect::Connected(self.provider_id.clone()),
            OauthEvent::Done(Err(None)) => {
                self.phase = Phase::Cancelled;
                self.error = Some(t("OAuth login cancelled").to_owned());
                self.focus = Focus::Submit;
                Effect::None
            }
            OauthEvent::Done(Err(Some(message))) => {
                self.phase = Phase::Error;
                self.error = Some(message);
                self.focus = Focus::Submit;
                Effect::None
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let busy = matches!(self.phase, Phase::Starting | Phase::Exchanging);
        let ctrl = key.mods.contains(Modifiers::CTRL);
        match key.code {
            KeyCode::Esc => {
                return match self.phase {
                    Phase::Email => {
                        self.phase = Phase::Idle;
                        self.focus = Focus::Enterprise;
                        Effect::None
                    }
                    Phase::Idle => Effect::OauthBack,
                    Phase::Error | Phase::Cancelled if self.dim => {
                        self.phase = Phase::Idle;
                        self.error = None;
                        self.focus = if self.dim_signed_in() {
                            Focus::Models
                        } else {
                            Focus::Submit
                        };
                        Effect::None
                    }
                    Phase::Error | Phase::Cancelled => Effect::OauthBack,
                    _ => Effect::OauthCancel,
                };
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.move_focus(-1);
                return Effect::None;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.move_focus(1);
                return Effect::None;
            }
            KeyCode::Char('r') if ctrl && self.phase == Phase::Idle && self.dim_signed_in() => {
                self.usage_loading = true;
                return Effect::LoginUsage(self.provider_id.clone());
            }
            _ => {}
        }
        if busy {
            return Effect::None;
        }
        if self.editing().is_some_and(|field| field.handle_key(key)) {
            return Effect::None;
        }
        match key.code {
            KeyCode::Enter => self.activate(),
            _ => Effect::None,
        }
    }

    /// The field the keyboard edits in this phase, if any.
    fn editing(&mut self) -> Option<&mut LineInput> {
        match (self.phase, self.focus) {
            (Phase::Email, Focus::Email) => Some(&mut self.email),
            (Phase::AwaitingCallback, Focus::Input) => Some(&mut self.callback),
            _ => None,
        }
    }

    /// A paste goes into the focused field.
    pub fn paste(&mut self, text: &str) {
        if let Some(field) = self.editing() {
            field.paste(text);
        }
    }

    fn link_height(&self, height: usize) -> usize {
        let reserved = 5
            + self.rows(0, None).len()
            + usize::from(self.message.is_some())
            + usize::from(self.error.is_some());
        height.saturating_sub(reserved).max(1)
    }

    /// The focused field's caret: its row among the rows drawn and its
    /// column after the border, the marker and the label.
    pub fn cursor(&self, width: usize, height: usize) -> Option<(usize, usize)> {
        let (phase, focus) = (self.phase, self.focus);
        let field = match (phase, focus) {
            (Phase::Email, Focus::Email) => &self.email,
            (Phase::AwaitingCallback, Focus::Input) => &self.callback,
            _ => return None,
        };
        let inner = width.saturating_sub(4).max(1);
        let rows = self.rows(inner.saturating_sub(2), None);
        let (index, label) = rows.iter().enumerate().find_map(|(i, (row, label))| {
            matches!(row, Row::Action(f) if *f == focus).then_some((i, label))
        })?;
        let before: usize = rows[..index]
            .iter()
            .map(|(row, label)| {
                if *row == Row::Action(Focus::Link) {
                    wrap_cells(label, inner).len().min(self.link_height(height))
                } else {
                    1
                }
            })
            .sum();
        let column = field.view(field_room(inner, label)).1;
        Some((
            3 + before,
            2 + width_of(SELECTED_MARKER) + width_of(label) + 2 + column,
        ))
    }

    fn activate(&mut self) -> Effect {
        match self.focus {
            Focus::Submit => match self.phase {
                Phase::Idle => self.start(false, None),
                Phase::Email => {
                    let email = self.email.text().trim().to_owned();
                    if email.is_empty() {
                        self.error = Some(t("Enter your work email").to_owned());
                        Effect::None
                    } else {
                        self.start(false, Some(email))
                    }
                }
                Phase::AwaitingCallback => {
                    let url = self.callback.text().trim().to_owned();
                    let valid = self
                        .auth_url
                        .as_deref()
                        .is_some_and(|auth| crate::ui::oauth_view::callback_matches(&url, auth));
                    if url.is_empty() {
                        Effect::None
                    } else if !valid {
                        self.error =
                            Some(t("Invalid OAuth state - possible CSRF attempt").to_owned());
                        Effect::None
                    } else {
                        self.callback.clear();
                        self.error = None;
                        Effect::OauthPaste(url)
                    }
                }
                _ => {
                    let device = self.device;
                    self.start(device, None)
                }
            },
            Focus::Enterprise => {
                self.phase = Phase::Email;
                self.focus = Focus::Email;
                self.error = None;
                Effect::None
            }
            Focus::Email => {
                self.focus = Focus::Submit;
                Effect::None
            }
            Focus::DeviceTab if self.phase == Phase::Idle => self.start(true, None),
            Focus::DeviceTab | Focus::BrowserTab => Effect::None,
            Focus::Models => Effect::OpenModels,
            Focus::Plan => match self.plan_url() {
                Some(url) => Effect::OpenUrl(url),
                None => Effect::None,
            },
            Focus::Logout => Effect::Logout(AccountTarget {
                provider_id: self.provider_id.clone(),
                namespace: self.namespace.clone(),
            }),
            Focus::RestartDevice => self.start(true, None),
            Focus::Link => match &self.auth_url {
                Some(url) => Effect::OpenUrl(url.clone()),
                None => Effect::None,
            },
            Focus::Input => {
                self.focus = Focus::Submit;
                Effect::None
            }
        }
    }

    fn hint(&self) -> &'static str {
        t(match self.phase {
            Phase::Starting => "Preparing login... · esc cancel",
            Phase::Exchanging => "Completing login... · esc cancel",
            Phase::Error | Phase::Cancelled => "enter restart · esc back",
            _ if matches!(self.focus, Focus::Email | Focus::Input) => {
                "type value · ↑↓/tab move · enter submit · esc cancel"
            }
            _ if self.focus == Focus::Link => "Select to copy · Enter opens link",
            Phase::Idle if self.dim && self.dim_signed_in() => {
                "↑↓ move · enter activate · ctrl+r refresh · esc close"
            }
            Phase::Idle => "↑↓ move · enter activate · esc close",
            _ => "↑↓ move · enter activate · esc back",
        })
    }

    pub fn render(&self, width: usize, height: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let mut content = vec![
            title_plain(&tf("{name} OAuth", &[("name", &self.name)]), inner, p),
            String::new(),
        ];
        let restart = matches!(self.phase, Phase::Error | Phase::Cancelled);
        let rows = self.rows(inner.saturating_sub(2), Some(p));
        let mut shortened = false;
        for (row, label) in &rows {
            match row {
                Row::Section => content.push(format!(
                    "{IDLE_MARKER}{}",
                    fg(&truncate_cells(label, inner.saturating_sub(2)), &p.muted)
                )),
                Row::Styled => content.push(format!("{IDLE_MARKER}{label}")),
                Row::Action(Focus::Link) => {
                    let mut lines = wrap_cells(label, inner);
                    let limit = self.link_height(height);
                    if lines.len() > limit {
                        shortened = true;
                        lines.truncate(limit);
                        let last = lines.last_mut().unwrap();
                        *last = truncate_cells(&format!("{last}…"), inner);
                    }
                    for line in lines {
                        content.push(fg(
                            &crate::ui::render::osc8::hyperlink(label, &line),
                            &p.accent,
                        ));
                    }
                }
                Row::Action(focus) => {
                    let focused = *focus == self.focus && !restart;
                    content.push(match focus {
                        Focus::Email => input_row(
                            label,
                            &self.email.view(field_room(inner, label)).0,
                            "you@example.com",
                            focused,
                            inner,
                            p,
                        ),
                        Focus::Input => input_row(
                            label,
                            &self.callback.view(field_room(inner, label)).0,
                            t("(paste here)"),
                            focused,
                            inner,
                            p,
                        ),
                        _ => action_row(label, focused, inner, p),
                    });
                }
            }
        }
        if let Some(message) = &self.message {
            content.push(format!(
                "{IDLE_MARKER}{}",
                fg(&truncate_cells(message, inner.saturating_sub(2)), &p.muted)
            ));
        }
        if let Some(error) = &self.error {
            content.push(format!(
                "{IDLE_MARKER}{}",
                fg(
                    &truncate_cells(
                        &tf("Error: {error}", &[("error", error)]),
                        inner.saturating_sub(2)
                    ),
                    &p.danger
                )
            ));
        }
        content.push(String::new());
        let hint = if shortened && self.focus == Focus::Link {
            t("Link shortened · Enter opens link")
        } else {
            self.hint()
        };
        content.push(fg(&truncate_cells(hint, inner), &p.muted));
        bordered(&content, width, None, &p.border, None, None)
    }
}

/// The callback must carry exactly the attempt's `state` and come from the
/// listener's origin.
fn state_matches(callback: &str, authorization_url: &str, origin: &str) -> bool {
    fn query(url: &str) -> Vec<(String, String)> {
        let Some((_, q)) = url.split_once('?') else {
            return Vec::new();
        };
        let q = q.split('#').next().unwrap_or("");
        q.split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(k), percent_decode(v))
            })
            .collect()
    }
    let want: Vec<String> = query(authorization_url)
        .into_iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v)
        .collect();
    let got: Vec<String> = query(callback)
        .into_iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v)
        .collect();
    callback.starts_with(&format!("{origin}/auth/callback?"))
        && want.len() == 1
        && !want[0].is_empty()
        && got == want
}

/// `name · email`, whichever the account carries, else its subject.
fn identity_line(info: &AccountInfo) -> String {
    let parts: Vec<&str> = [info.name.as_deref(), info.email.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        info.subject.clone()
    } else {
        parts.join(" · ")
    }
}

/// `web_search` reads `Web search`.
fn feature_label(key: &str) -> String {
    let mut label = key.replace('_', " ");
    if let Some(first) = label.get(..1).map(str::to_ascii_uppercase) {
        label.replace_range(..1, &first);
    }
    label
}

/// The day of an ISO 8601 timestamp; other text as it is.
fn short_date(text: &str) -> &str {
    match text.get(..10) {
        Some(day) if text.len() > 10 && day.as_bytes()[4] == b'-' => day,
        _ => text,
    }
}

/// `25000` reads `25,000`.
fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether a pasted callback URL belongs to the attempt that issued
/// `authorization_url`: same redirect origin and the attempt's `state`.
pub fn callback_matches(callback: &str, authorization_url: &str) -> bool {
    let redirect = authorization_url
        .split_once('?')
        .map(|(_, q)| q)
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "redirect_uri")
        .map(|(_, v)| percent_decode(v));
    match redirect {
        Some(uri) => state_matches(callback, authorization_url, origin_of(&uri)),
        None => false,
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn origin_of(redirect_uri: &str) -> &str {
    redirect_uri
        .strip_suffix("/auth/callback")
        .unwrap_or(redirect_uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::DIM_OAUTH_ID;
    use crate::ui::terminal::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn started(view: &mut OauthView) -> OauthStart {
        match view.handle_key(key(KeyCode::Enter)) {
            Effect::OauthStart(start) => start,
            _ => panic!("expected a start"),
        }
    }

    fn described(id: &str, namespace: &str, methods: &[&str], name: &str) -> OauthView {
        OauthView::described(
            id,
            &AccountDescriptor {
                namespace: namespace.into(),
                login_methods: methods.iter().map(|method| (*method).to_owned()).collect(),
            },
            name,
            false,
            0,
        )
    }

    #[test]
    fn dim_entry_starts_a_browser_login_and_tags_the_attempt() {
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        let start = started(&mut view);
        assert!(!start.device);
        assert_eq!(start.attempt, 1);
        assert!(matches!(
            view.event(7, OauthEvent::Exchanging),
            Effect::None
        ));
        assert_eq!(view.phase, Phase::Starting);
        view.event(1, OauthEvent::Exchanging);
        assert_eq!(view.phase, Phase::Exchanging);
    }

    #[test]
    fn the_described_methods_choose_the_flow() {
        let mut codex = described(
            "openai-codex",
            "account.codex",
            &["browser"],
            "OpenAI Codex",
        );
        assert!(matches!(
            codex.auto_start(),
            Effect::OauthStart(s) if !s.device && s.namespace == "account.codex"
        ));
        let mut grok = described("xai-grok-build", "account.grok", &["device"], "Grok Build");
        assert!(matches!(
            grok.auto_start(),
            Effect::OauthStart(s) if s.device && s.namespace == "account.grok"
        ));
        let mut signed_in = OauthView::described(
            "openai-codex",
            &AccountDescriptor {
                namespace: "account.codex".into(),
                login_methods: vec!["browser".into()],
            },
            "OpenAI Codex",
            true,
            0,
        );
        assert!(matches!(signed_in.auto_start(), Effect::None));
        assert_eq!(labels(&signed_in), ["Signed in", "", "Sign out"]);
        assert!(matches!(
            signed_in.handle_key(key(KeyCode::Enter)),
            Effect::Logout(t) if t.provider_id == "openai-codex" && t.namespace == "account.codex"
        ));
    }

    #[test]
    fn a_described_account_offers_no_entry_for_a_method_it_does_not_have() {
        let device = described("grok-build", "account.grok", &["device"], "Grok Build");
        let rows = labels(&device);
        assert_eq!(rows, ["Device login", "Restart device login"]);
        let browser = described(
            "openai-codex",
            "account.codex",
            &["browser"],
            "OpenAI Codex",
        );
        assert_eq!(labels(&browser), ["Browser login"]);
    }

    #[test]
    fn pasted_callback_with_the_wrong_state_is_refused() {
        let mut view = described(
            "openai-codex",
            "account.codex",
            &["browser"],
            "OpenAI Codex",
        );
        view.auto_start();
        view.event(
            1,
            OauthEvent::Url {
                url: "https://i/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=ok"
                    .into(),
                opened: true,
            },
        );
        view.focus = Focus::Input;
        view.callback = LineInput::new("http://localhost:1455/auth/callback?code=1&state=bad");
        view.focus = Focus::Submit;
        assert!(matches!(view.handle_key(key(KeyCode::Enter)), Effect::None));
        assert_eq!(
            view.error.as_deref(),
            Some("Invalid OAuth state - possible CSRF attempt")
        );
        view.callback = LineInput::new("http://localhost:1455/auth/callback?code=1&state=ok");
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            Effect::OauthPaste(_)
        ));
    }

    #[test]
    fn failure_offers_a_restart_and_esc_returns_to_the_menu() {
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        started(&mut view);
        view.event(1, OauthEvent::Done(Err(Some("denied".into()))));
        assert_eq!(view.phase, Phase::Error);
        assert!(
            matches!(view.handle_key(key(KeyCode::Enter)), Effect::OauthStart(s) if s.attempt == 2)
        );
        view.event(2, OauthEvent::Done(Err(Some("denied".into()))));
        assert!(matches!(view.handle_key(key(KeyCode::Esc)), Effect::None));
        assert_eq!(view.phase, Phase::Idle);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            Effect::OauthBack
        ));
        let mut codex = described(
            "openai-codex",
            "account.codex",
            &["browser"],
            "OpenAI Codex",
        );
        codex.auto_start();
        codex.event(1, OauthEvent::Done(Err(Some("denied".into()))));
        assert!(matches!(
            codex.handle_key(key(KeyCode::Esc)),
            Effect::OauthBack
        ));
    }

    fn palette() -> Palette {
        use crate::ui::theme::{Appearance, ColorMode, Theme};
        Theme::system(ColorMode::TrueColor, Appearance::Dark, None).palette
    }

    fn labels(view: &OauthView) -> Vec<String> {
        view.rows(60, Some(&palette()))
            .into_iter()
            .map(|(_, label)| crate::ui::render::panel::strip_escapes(&label))
            .collect()
    }

    fn signed_in() -> AccountInfo {
        AccountInfo {
            issuer: "https://dimagent.cn".into(),
            subject: "5".into(),
            email: Some("a@b.com".into()),
            name: Some("golden".into()),
        }
    }

    /// The signed-out menu offers the three ways in, each entering its
    /// own flow without a mode switch inside it.
    #[test]
    fn dim_menu_entries_go_straight_into_their_flows() {
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        view.set_account(Ok(None));
        assert_eq!(
            labels(&view),
            [
                "Not signed in",
                "",
                "Login with DimAgent",
                "Enterprise login",
                "Device login"
            ]
        );
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Down));
        let start = started(&mut view);
        assert!(start.device && start.email.is_none());
        assert_eq!(labels(&view)[0], "Device login");
        assert!(!labels(&view).iter().any(|l| l.contains("mode")));
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        view.set_account(Ok(None));
        view.handle_key(key(KeyCode::Down));
        assert!(matches!(view.handle_key(key(KeyCode::Enter)), Effect::None));
        assert_eq!(view.phase, Phase::Email);
        for c in "a@b.com".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        view.handle_key(key(KeyCode::Enter));
        let start = started(&mut view);
        assert!(!start.device);
        assert_eq!(start.email.as_deref(), Some("a@b.com"));
        assert_eq!(labels(&view), ["Browser login"]);
        assert!(!labels(&view).iter().any(|l| l.contains("mode")));
    }

    /// Signed in, the menu offers the models and a sign-out; the host's
    /// next account report turns it back into the sign-in menu.
    #[test]
    fn signed_in_menu_offers_models_and_sign_out() {
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        assert_eq!(labels(&view)[0], "Checking account…");
        view.set_account(Ok(Some(signed_in())));
        assert_eq!(
            labels(&view),
            [
                "golden · a@b.com",
                "Checking usage…",
                "",
                "Open models",
                "View plan",
                "Sign out"
            ]
        );
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            Effect::OpenModels
        ));
        view.handle_key(key(KeyCode::Down));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            Effect::OpenUrl(url) if url == "https://dimagent.cn/console/subscription"
        ));
        view.handle_key(key(KeyCode::Down));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            Effect::Logout(t) if t.provider_id == DIM_OAUTH_ID && t.namespace == ACCOUNT_NAMESPACE
        ));
        view.set_account(Ok(None));
        assert_eq!(labels(&view)[2], "Login with DimAgent");
        assert_eq!(view.focus, Focus::Submit);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            Effect::OauthBack
        ));
    }

    /// Usage draws the plan, one bar per meter, and asks again on Ctrl+R;
    /// a plugin without usage leaves one line saying so.
    #[test]
    fn usage_rows_show_plan_credits_windows_and_features() {
        use crate::model::session::{UsageCredits, UsageFeature, UsagePlan, UsageWindow};
        let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
        view.set_account(Ok(Some(signed_in())));
        view.set_usage(Ok(Usage {
            plan: Some(UsagePlan {
                name: Some("Internal".into()),
                status: Some("active".into()),
                price_amount: Some(10.0),
                price_currency: Some("CNY".into()),
                price_interval: Some("month".into()),
                expires_at: Some("2026-10-28T00:00:00Z".into()),
            }),
            credits: Some(UsageCredits {
                used_units: Some(25000),
                total_units: Some(300000),
                remaining_units: None,
                unlimited: None,
                resets_at: Some("2026-10-28T00:00:00Z".into()),
            }),
            windows: vec![UsageWindow {
                duration_hours: 5,
                token_used: 9500,
                token_cap: 10000,
                expires_at: None,
            }],
            features: vec![
                UsageFeature {
                    key: "web_search".into(),
                    used: Some(211),
                    allowance: Some(1500),
                    remaining: None,
                    unit: Some("call".into()),
                    unlimited: None,
                    period_end: Some("2026-10-28T00:00:00Z".into()),
                },
                UsageFeature {
                    key: "images".into(),
                    unlimited: Some(true),
                    ..Default::default()
                },
            ],
        }));
        let rows = labels(&view);
        assert_eq!(
            rows[1],
            "Internal · ¥0.10/month · active · expires 2026-10-28"
        );
        assert_eq!(rows[2], "");
        assert_eq!(
            rows[3],
            "Credits     25,000 / 300,000  8% · resets 2026-10-28"
        );
        assert_eq!(rows[4], format!("{}{}", "█".repeat(3), "░".repeat(37)));
        assert_eq!(rows[6], "5h window   9,500 / 10,000  95%");
        assert_eq!(rows[7], format!("{}{}", "█".repeat(38), "░".repeat(2)));
        assert_eq!(rows[9], "Web search  211 / 1,500  14% · resets 2026-10-28");
        assert_eq!(rows[12], "Images      unlimited");
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CTRL)),
            Effect::LoginUsage(id) if id == DIM_OAUTH_ID
        ));
        assert!(labels(&view)[3].starts_with("Credits"));
        assert_eq!(labels(&view)[12], "Images      unlimited");
        view.set_usage(Err("This account plugin does not report usage".into()));
        assert_eq!(
            labels(&view)[1],
            "Usage: This account plugin does not report usage"
        );
    }
}
