//! The `/resume` picker: a filterable list of stored sessions (port of
//! `sessionsPanelItems` / `SessionsPanel`).

use crate::model::session::{Command, SessionItem, SessionsScope, stored_title};
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::tilde;
use std::time::{Duration, Instant};

const HINT: &str = "Enter open · Ctrl+S scope · Ctrl+R refresh · Esc close";
const TITLE_WORKSPACE: &str = "Sessions · This workspace";
const TITLE_REPOSITORY: &str = "Sessions · This repository";
const TITLE_ALL: &str = "Sessions · All workspaces";
const EMPTY_WORKSPACE: &str = "No saved sessions in this workspace. Ctrl+S shows more.";

pub struct SessionsPanel {
    list: ListPanel,
    request_id: Option<String>,
    paged: Option<bool>,
    due: Option<Instant>,
    append: bool,
    next_cursor: Option<String>,
    show_acp: bool,
    scope: SessionsScope,
    /// The launch workspace's repository workspaces, when the daemon can read
    /// a set and it holds more than one; the scope is offered while it does.
    repository: Option<Vec<String>>,
}

impl SessionsPanel {
    pub fn new(show_acp: bool) -> Self {
        Self {
            request_id: None,
            paged: None,
            due: Some(Instant::now()),
            append: false,
            next_cursor: None,
            show_acp,
            scope: SessionsScope::Workspace,
            repository: None,
            list: ListPanel::new(
                TITLE_WORKSPACE,
                HINT,
                "Loading sessions...",
                EMPTY_WORKSPACE,
                "No matching sessions.",
                true,
            ),
        }
    }

    pub fn set_items(&mut self, items: Result<Vec<SessionItem>, String>) {
        if self.paged.is_none() {
            self.paged = Some(false);
        }
        let workspaces = self.names_workspaces();
        self.list
            .set_items(items.map(|items| session_rows(items, workspaces)), None);
    }

    pub fn page(
        &mut self,
        id: &str,
        paged: bool,
        result: Result<(Vec<SessionItem>, Option<String>), String>,
    ) {
        if self.request_id.as_deref() != Some(id) {
            return;
        }
        self.request_id = None;
        self.paged = Some(paged);
        self.list.remote_filter(paged);
        match result {
            Ok((items, next_cursor)) => {
                self.next_cursor = next_cursor;
                if self.append {
                    self.list
                        .append_items(session_rows(items, self.names_workspaces()));
                } else {
                    self.set_items(Ok(items));
                }
            }
            Err(error) => self.set_items(Err(error)),
        }
    }

    pub fn request(&mut self) -> Option<Command> {
        let reset = self.due.is_some_and(|due| Instant::now() >= due);
        if self.due.is_some() && !reset {
            return None;
        }
        if !reset
            && (self.request_id.is_some() || !self.list.near_end() || self.next_cursor.is_none())
        {
            return None;
        }
        self.due = None;
        self.append = !reset;
        let id = uuid::Uuid::new_v4().to_string();
        self.request_id = Some(id.clone());
        Some(Command::ListSessions {
            id,
            query: self.list.query().trim().to_owned(),
            cursor: if reset { None } else { self.next_cursor.take() },
            show_acp: self.show_acp,
            scope: self.scope.clone(),
        })
    }

    /// The launch workspace's repository set arrived: the scope joins the
    /// cycle, without touching the read already under way.
    pub fn repository(&mut self, workspaces: Vec<String>) {
        self.repository = Some(workspaces);
    }

    pub fn refresh(&mut self) {
        self.request_id = None;
        self.next_cursor = None;
        self.due = Some(Instant::now());
        self.list.loading();
    }

    fn filter_changed(&mut self) {
        if self.paged != Some(false) {
            self.refresh();
            self.due = Some(Instant::now() + Duration::from_millis(150));
        }
    }

    /// A paste goes into the filter.
    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
        self.filter_changed();
    }

    fn names_workspaces(&self) -> bool {
        !matches!(self.scope, SessionsScope::Workspace)
    }

    fn next_scope(&self) -> SessionsScope {
        match &self.scope {
            SessionsScope::Workspace => match &self.repository {
                Some(workspaces) => SessionsScope::Repository(workspaces.clone()),
                None => SessionsScope::All,
            },
            SessionsScope::Repository(_) => SessionsScope::All,
            SessionsScope::All => SessionsScope::Workspace,
        }
    }

    /// Ctrl+S widens and narrows the list like a new query: the scope
    /// changes and the first page is read again.
    fn cycle_scope(&mut self) {
        self.scope = self.next_scope();
        let (title, empty) = match &self.scope {
            SessionsScope::Workspace => (TITLE_WORKSPACE, EMPTY_WORKSPACE),
            SessionsScope::Repository(_) => (TITLE_REPOSITORY, "No saved sessions yet."),
            SessionsScope::All => (TITLE_ALL, "No saved sessions yet."),
        };
        self.list.set_title(t(title));
        self.list.set_empty_text(empty);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if key.code == KeyCode::Char('s') && key.mods.contains(Modifiers::CTRL) {
            self.cycle_scope();
            return Effect::Refresh;
        }
        let before = self.list.query().to_owned();
        let event = self.list.handle_key(key);
        if before != self.list.query() {
            self.filter_changed();
        }
        match event {
            Event::None => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(id) => Effect::Resume(id),
            Event::Refresh => Effect::Refresh,
        }
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.list.cursor(width)
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}

fn session_rows(items: Vec<SessionItem>, workspaces: bool) -> Vec<Item> {
    let now = now_ms();
    items
        .iter()
        .map(|item| {
            let detail = if workspaces {
                format!(
                    "{} · {}",
                    relative_time(item.updated_at, now),
                    tilde(&item.cwd)
                )
            } else {
                relative_time(item.updated_at, now)
            };
            let mut row = Item::new(
                item.id.clone(),
                match tag(item.origin.as_deref()) {
                    Some(tag) => format!("{tag} {}", label(item)),
                    None => label(item),
                },
                detail,
            );
            row.search = search_text(item);
            row
        })
        .collect()
}

fn search_text(item: &SessionItem) -> String {
    [
        item.title.as_deref(),
        Some(item.id.as_str()),
        Some(item.cwd.as_str()),
        Some(item.model.as_str()),
        item.preview.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

/// The row's class, by the origin the daemon reports. A fixed token, not
/// prose: the three labels stay one column wide (#557).
fn tag(origin: Option<&str>) -> Option<&'static str> {
    match origin {
        Some("acp") => Some("[ACP ]"),
        Some("internal") => Some("[Agent]"),
        Some("client") | Some("unknown") => Some("[Chat]"),
        _ => None,
    }
}

/// The stored title unless it is the placeholder, else the first words, else
/// the id.
pub fn label(item: &SessionItem) -> String {
    if let Some(title) = stored_title(item.title.as_deref()) {
        return title.to_owned();
    }
    let preview = item.preview.as_deref().map(str::trim).unwrap_or("");
    if !preview.is_empty() {
        return preview.replace('\n', " ");
    }
    item.title
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| item.id.clone(), str::to_owned)
}

pub fn relative_time(updated_at: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(updated_at);
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if elapsed < MINUTE {
        t("now").to_owned()
    } else if elapsed < HOUR {
        tf(
            "{count}m ago",
            &[("count", &(elapsed / MINUTE).to_string())],
        )
    } else if elapsed < DAY {
        tf("{count}h ago", &[("count", &(elapsed / HOUR).to_string())])
    } else {
        tf("{count}d ago", &[("count", &(elapsed / DAY).to_string())])
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::terminal::{KeyCode, Modifiers};

    fn item(id: &str, title: Option<&str>, preview: Option<&str>) -> SessionItem {
        SessionItem {
            id: id.into(),
            title: title.map(str::to_owned),
            preview: preview.map(str::to_owned),
            cwd: "/w".into(),
            updated_at: 0,
            model: "p/m".into(),
            origin: Some("client".into()),
        }
    }

    #[test]
    fn label_prefers_a_real_name_then_the_first_message() {
        assert_eq!(label(&item("a", Some("Fix bug"), Some("hi"))), "Fix bug");
        assert_eq!(
            label(&item("a", Some("New session"), Some("hello there"))),
            "hello there"
        );
        assert_eq!(label(&item("abc", None, None)), "abc");
    }

    #[test]
    fn moving_up_inside_the_window_moves_the_highlight_not_the_list() {
        use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};
        let p = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let mut panel = SessionsPanel::new(false);
        let items = (0..30)
            .map(|i| item(&format!("s{i}"), Some(&format!("session {i:02}")), None))
            .collect();
        panel.set_items(Ok(items));
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        let shown = |panel: &SessionsPanel| -> Vec<String> {
            panel
                .render(60, 10, &p)
                .iter()
                .map(|r| {
                    let plain = crate::ui::render::panel::strip_escapes(r);
                    if r.contains("\x1b[30;43m") {
                        plain.replacen("  [Chat] session", "› [Chat] session", 1)
                    } else {
                        plain
                    }
                })
                .filter(|r| r.contains("session "))
                .collect()
        };
        shown(&panel);
        for _ in 0..7 {
            panel.handle_key(key(KeyCode::Down));
        }
        let rows = shown(&panel);
        assert!(
            rows[0].contains("session 03") && rows[4].contains("› [Chat] session 07"),
            "{rows:?}"
        );
        panel.handle_key(key(KeyCode::Up));
        panel.handle_key(key(KeyCode::Up));
        let rows = shown(&panel);
        assert!(
            rows[0].contains("session 03") && rows[2].contains("› [Chat] session 05"),
            "{rows:?}"
        );
        for _ in 0..3 {
            panel.handle_key(key(KeyCode::Up));
        }
        let rows = shown(&panel);
        assert!(rows[0].contains("› [Chat] session 02"), "{rows:?}");
    }

    #[test]
    fn filter_narrows_and_enter_resumes_the_selected_row() {
        let mut panel = SessionsPanel::new(false);
        panel.set_items(Ok(vec![
            item("one", None, Some("alpha topic")),
            item("two", None, Some("beta topic")),
        ]));
        for c in "beta".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
        }
        match panel.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)) {
            Effect::Resume(id) => assert_eq!(id, "two"),
            _ => panic!("expected resume"),
        }
    }
}
