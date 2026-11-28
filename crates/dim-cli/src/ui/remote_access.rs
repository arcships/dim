use super::{
    i18n::t,
    list_panel::{Event, Item, ListPanel},
    prompts::Effect,
    render::panel::{confirm_panel, wrap},
    terminal::{KeyCode, KeyEvent},
    theme::Palette,
};
use crate::sharing::Status;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, channel},
};

enum Page {
    Settings,
    Name(String),
    Reset,
    Token,
}
pub struct RemoteAccess {
    home: PathBuf,
    list: ListPanel,
    status: Option<Status>,
    error: Option<String>,
    page: Page,
    loading: Option<Receiver<Result<Status, String>>>,
}
impl RemoteAccess {
    pub fn new(home: PathBuf) -> Self {
        let mut panel = Self {
            home,
            list: Self::list(),
            status: None,
            error: None,
            page: Page::Settings,
            loading: None,
        };
        panel.request(vec!["status".into()]);
        panel
    }
    fn list() -> ListPanel {
        ListPanel::new(
            "Remote access",
            t("↑↓ move · Enter select · Esc close"),
            "Loading…",
            "",
            "",
            false,
        )
        .without_filter()
    }
    fn request(&mut self, args: Vec<String>) {
        let home = self.home.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = crate::sharing::setting(&home, &args).map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        self.loading = Some(rx);
        self.page = Page::Settings;
    }
    pub fn tick(&mut self) -> bool {
        let Some(rx) = &self.loading else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.loading = None;
        match result {
            Ok(status) => {
                self.error = status.error.clone();
                self.status = Some(status)
            }
            Err(err) => self.error = Some(err),
        }
        let selected = self.list.selected_id();
        let mut items = Vec::new();
        let mut notes = Vec::new();
        if let Some(s) = &self.status {
            items.push(Item::new(
                "toggle",
                t("Allow connections to this computer"),
                if s.enabled { t("on") } else { t("off") },
            ));
            items.push(Item::new("name", t("Computer name"), &s.name));
            if let Some(token) = &s.token {
                items.push(Item::new("token", t("Sharing token"), "›"));
                notes.push(token.clone());
            }
            items.push(Item::new("reset", t("Reset sharing token"), "›"));
            notes.extend(s.addresses.iter().cloned());
        }
        if let Some(message) = &self.error {
            notes.insert(0, message.clone());
            items.push(Item::new("reload", t("Reload"), ""));
        }
        self.list = Self::list().with_notes(notes);
        self.list.set_items(Ok(items), selected.as_deref());
        true
    }
    pub fn paste(&mut self, text: &str) {
        if let Page::Name(name) = &mut self.page {
            name.extend(text.chars().filter(|c| !c.is_control()));
        }
    }
    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match &mut self.page {
            Page::Name(name) => match key.code {
                KeyCode::Esc => self.page = Page::Settings,
                KeyCode::Enter => {
                    let name = name.clone();
                    self.request(vec!["rename".into(), name])
                }
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) if !c.is_control() => name.push(c),
                _ => {}
            },
            Page::Reset => match key.code {
                KeyCode::Char('y') => self.request(vec!["reset".into()]),
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Enter => self.page = Page::Settings,
                _ => {}
            },
            Page::Token => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    self.page = Page::Settings
                }
            }
            Page::Settings => match self.list.handle_key(key) {
                Event::Close => return Effect::Close,
                Event::Pick { id, .. } if self.loading.is_none() => match id.as_str() {
                    "toggle" => {
                        let on = self.status.as_ref().is_some_and(|s| s.enabled);
                        self.request(vec![if on { "disable" } else { "enable" }.into()])
                    }
                    "name" => {
                        if let Some(s) = &self.status {
                            self.page = Page::Name(s.name.clone())
                        }
                    }
                    "reset" => self.page = Page::Reset,
                    "token" => self.page = Page::Token,
                    "reload" => self.request(vec!["status".into()]),
                    _ => {}
                },
                _ => {}
            },
        }
        Effect::None
    }
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let (title, body, hint) = match &self.page {
            Page::Settings if self.loading.is_some() => return Self::list().render(width, rows, p),
            Page::Settings => return self.list.render(width, rows, p),
            Page::Name(name) => (
                t("Computer name"),
                name.as_str(),
                t("Enter save · Esc cancel"),
            ),
            Page::Reset => (
                t("Reset sharing token"),
                t("Disconnect all shared terminals and issue a new token?"),
                t("y confirm · Esc cancel"),
            ),
            Page::Token => (
                t("Sharing token"),
                self.status
                    .as_ref()
                    .and_then(|s| s.token.as_deref())
                    .unwrap_or(""),
                t("Esc back"),
            ),
        };
        confirm_panel(
            title,
            &wrap(body, width.saturating_sub(4).max(1)),
            &[],
            0,
            hint,
            width,
            p,
        )
    }
}
