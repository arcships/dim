//! The OAuth step of the `/connect` panel: Dim / Codex / Grok sign-in by
//! browser or device code (port of `oauthView` / `oauthRows`).

use crate::model::session::{OauthEvent, OauthStart};
use crate::ui::composer::palette::title_plain;
use crate::ui::i18n::{t, tf};
use crate::ui::prompts::Effect;
use crate::ui::providers_panel::{action_row, input_row};
use crate::ui::render::panel::{IDLE_MARKER, bordered};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;
use crate::ui::transcript::blocks::truncate_cells;

const DIM_OAUTH_ID: &str = "dimcode-api-oauth";
const CODEX_ID: &str = "openai-codex";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing started: the Dim entry rows, or the mode rows of Codex/Grok.
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
    BrowserTab,
    Email,
    Copy,
    Input,
    RestartDevice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Section,
    Action(Focus),
}

pub struct OauthView {
    provider_id: String,
    name: String,
    device: bool,
    /// Codex signs in by browser only, Grok by device code only.
    modes: (bool, bool),
    phase: Phase,
    focus: Focus,
    attempt: u64,
    email: String,
    callback: String,
    auth_url: Option<String>,
    device_text: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

impl OauthView {
    pub fn new(provider_id: &str, name: &str, attempt: u64) -> Self {
        let dim = provider_id == DIM_OAUTH_ID;
        let modes = match provider_id {
            CODEX_ID => (true, false),
            DIM_OAUTH_ID => (true, true),
            _ => (false, true),
        };
        let mut view = Self {
            provider_id: provider_id.to_owned(),
            name: name.to_owned(),
            device: !modes.0,
            modes,
            phase: Phase::Idle,
            focus: Focus::Submit,
            attempt,
            email: String::new(),
            callback: String::new(),
            auth_url: None,
            device_text: None,
            message: None,
            error: None,
        };
        if dim {
            view.focus = Focus::Submit;
        } else {
            // Codex and Grok start at once, the way the reference does.
            view.focus = if view.device {
                Focus::DeviceTab
            } else {
                Focus::BrowserTab
            };
        }
        view
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Codex and Grok have no entry rows: the sign-in begins on open.
    pub fn auto_start(&mut self) -> Effect {
        if self.provider_id == DIM_OAUTH_ID {
            Effect::None
        } else {
            self.start(self.device, None)
        }
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
            device,
            email,
        })
    }

    fn rows(&self) -> Vec<(Row, String)> {
        let mut rows: Vec<(Row, String)> = Vec::new();
        let dim = self.provider_id == DIM_OAUTH_ID;
        if dim && self.phase == Phase::Idle {
            rows.push((Row::Action(Focus::Submit), t("Login with DimAgent").into()));
            rows.push((Row::Action(Focus::Enterprise), t("Enterprise login").into()));
            rows.push((Row::Action(Focus::DeviceTab), t("Device login").into()));
            return rows;
        }
        if dim && self.phase == Phase::Email {
            rows.push((Row::Action(Focus::Email), t("Email").into()));
            rows.push((Row::Action(Focus::Submit), t("Continue").into()));
            return rows;
        }
        if self.modes.0 && self.modes.1 {
            rows.push((
                Row::Action(Focus::BrowserTab),
                format!(
                    "{} {}",
                    if self.device { "[ ]" } else { "[x]" },
                    t("Browser mode")
                ),
            ));
            rows.push((
                Row::Action(Focus::DeviceTab),
                format!(
                    "{} {}",
                    if self.device { "[x]" } else { "[ ]" },
                    t("Device mode")
                ),
            ));
        } else if self.device {
            rows.push((Row::Action(Focus::DeviceTab), t("Device login").into()));
        } else {
            rows.push((Row::Action(Focus::BrowserTab), t("Browser login").into()));
        }
        if self.device {
            if let Some(text) = &self.device_text {
                rows.push((Row::Section, text.clone()));
            }
            rows.push((
                Row::Action(Focus::Copy),
                t("Print verification URL + pairing code again").into(),
            ));
            rows.push((
                Row::Action(Focus::RestartDevice),
                t("Restart device login").into(),
            ));
        } else if self.phase == Phase::AwaitingCallback {
            rows.push((Row::Action(Focus::Copy), t("Print OAuth URL again").into()));
            rows.push((Row::Action(Focus::Input), t("Callback URL").into()));
            rows.push((Row::Action(Focus::Submit), t("Submit callback").into()));
        }
        rows
    }

    fn focus_order(&self) -> Vec<Focus> {
        self.rows()
            .into_iter()
            .filter_map(|(row, _)| match row {
                Row::Action(focus) => Some(focus),
                Row::Section => None,
            })
            .collect()
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
                self.focus = Focus::Copy;
                if !opened {
                    self.message = Some(
                        t("The browser could not be opened. Open the link manually, then return here.")
                            .to_owned(),
                    );
                }
                // The link is also printed, so it is there to copy when the
                // browser could not open it.
                if !opened {
                    return Effect::Print(format!(
                        "{}\n{}",
                        tf("{name} OAuth URL", &[("name", &self.name)]),
                        self.auth_url.clone().unwrap_or_default()
                    ));
                }
                Effect::None
            }
            OauthEvent::Device {
                verification_uri,
                verification_uri_complete,
                user_code,
            } => {
                self.phase = Phase::AwaitingDevice;
                self.focus = Focus::Copy;
                let target = verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| verification_uri.clone());
                self.device_text = Some(tf(
                    "Enter code {code} at {url}",
                    &[("code", &user_code), ("url", &target)],
                ));
                self.auth_url = Some(target);
                self.message = Some(t("Waiting for authorization").to_owned());
                Effect::Print(format!(
                    "{}\n{}\n{}",
                    tf("{name} device login", &[("name", &self.name)]),
                    tf("Open this URL: {url}", &[("url", &verification_uri)]),
                    tf("Enter code:    {code}", &[("code", &user_code)]),
                ))
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
                    // Back from the email prompt to the entry rows.
                    Phase::Email => {
                        self.phase = Phase::Idle;
                        self.focus = Focus::Enterprise;
                        Effect::None
                    }
                    Phase::Idle | Phase::Error | Phase::Cancelled => Effect::OauthBack,
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
            _ => {}
        }
        if busy {
            return Effect::None;
        }
        let editing = match (self.phase, self.focus) {
            (Phase::Email, Focus::Email) => Some(&mut self.email),
            (Phase::AwaitingCallback, Focus::Input) => Some(&mut self.callback),
            _ => None,
        };
        match key.code {
            KeyCode::Backspace => {
                if let Some(text) = editing {
                    text.pop();
                }
                Effect::None
            }
            KeyCode::Char(c) if !ctrl && !key.mods.contains(Modifiers::ALT) => {
                if let Some(text) = editing {
                    text.push(c);
                }
                Effect::None
            }
            KeyCode::Enter => self.activate(),
            _ => Effect::None,
        }
    }

    fn activate(&mut self) -> Effect {
        match self.focus {
            Focus::Submit => match self.phase {
                Phase::Idle => self.start(false, None),
                Phase::Email => {
                    let email = self.email.trim().to_owned();
                    if email.is_empty() {
                        self.error = Some(t("Enter your work email").to_owned());
                        Effect::None
                    } else {
                        self.start(false, Some(email))
                    }
                }
                Phase::AwaitingCallback => {
                    let url = self.callback.trim().to_owned();
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
                // Restart after an error or a cancel.
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
            Focus::DeviceTab => {
                if self.phase == Phase::Idle || !self.device {
                    let abandon = self.phase != Phase::Idle;
                    let effect = self.start(true, None);
                    if abandon {
                        // The host replaces the earlier attempt on its own.
                    }
                    effect
                } else {
                    Effect::None
                }
            }
            Focus::BrowserTab => {
                if self.device && self.modes.0 {
                    self.start(false, None)
                } else {
                    Effect::None
                }
            }
            Focus::RestartDevice => self.start(true, None),
            Focus::Copy => match &self.auth_url {
                Some(url) => Effect::Print(format!(
                    "{}\n{url}",
                    tf("{name} OAuth URL", &[("name", &self.name)])
                )),
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
            _ => "↑↓ move · enter activate · esc back",
        })
    }

    pub fn render(&self, width: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let mut content = vec![
            title_plain(&tf("{name} OAuth", &[("name", &self.name)]), inner, p),
            String::new(),
        ];
        let restart = matches!(self.phase, Phase::Error | Phase::Cancelled);
        let rows = self.rows();
        for (row, label) in &rows {
            match row {
                Row::Section => content.push(format!(
                    "{IDLE_MARKER}{}",
                    fg(&truncate_cells(label, inner.saturating_sub(2)), &p.muted)
                )),
                Row::Action(focus) => {
                    let focused = *focus == self.focus && !restart;
                    content.push(match focus {
                        Focus::Email => input_row(
                            label,
                            &self.email,
                            "you@example.com",
                            focused,
                            false,
                            inner,
                            p,
                        ),
                        Focus::Input => input_row(
                            label,
                            &self.callback,
                            t("(paste here)"),
                            focused,
                            false,
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
        content.push(fg(&truncate_cells(self.hint(), inner), &p.muted));
        bordered(&content, width, None, &p.border, None)
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

    #[test]
    fn dim_entry_starts_a_browser_login_and_tags_the_attempt() {
        let mut view = OauthView::new(DIM_OAUTH_ID, "DimAgent", 0);
        let start = started(&mut view);
        assert!(!start.device);
        assert_eq!(start.attempt, 1);
        // An update of another attempt is ignored.
        assert!(matches!(
            view.event(7, OauthEvent::Exchanging),
            Effect::None
        ));
        assert_eq!(view.phase, Phase::Starting);
        view.event(1, OauthEvent::Exchanging);
        assert_eq!(view.phase, Phase::Exchanging);
    }

    #[test]
    fn codex_starts_on_open_and_grok_by_device() {
        let mut codex = OauthView::new("openai-codex", "OpenAI Codex", 0);
        assert!(matches!(codex.auto_start(), Effect::OauthStart(s) if !s.device));
        let mut grok = OauthView::new("xai-grok-build", "Grok Build", 0);
        assert!(matches!(grok.auto_start(), Effect::OauthStart(s) if s.device));
    }

    #[test]
    fn pasted_callback_with_the_wrong_state_is_refused() {
        let mut view = OauthView::new("openai-codex", "OpenAI Codex", 0);
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
        view.callback = "http://localhost:1455/auth/callback?code=1&state=bad".into();
        view.focus = Focus::Submit;
        assert!(matches!(view.handle_key(key(KeyCode::Enter)), Effect::None));
        assert_eq!(
            view.error.as_deref(),
            Some("Invalid OAuth state - possible CSRF attempt")
        );
        view.callback = "http://localhost:1455/auth/callback?code=1&state=ok".into();
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            Effect::OauthPaste(_)
        ));
    }

    #[test]
    fn failure_offers_a_restart_and_esc_goes_back() {
        let mut view = OauthView::new(DIM_OAUTH_ID, "DimAgent", 0);
        started(&mut view);
        view.event(1, OauthEvent::Done(Err(Some("denied".into()))));
        assert_eq!(view.phase, Phase::Error);
        assert!(
            matches!(view.handle_key(key(KeyCode::Enter)), Effect::OauthStart(s) if s.attempt == 2)
        );
        view.event(2, OauthEvent::Done(Err(Some("denied".into()))));
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            Effect::OauthBack
        ));
    }
}
