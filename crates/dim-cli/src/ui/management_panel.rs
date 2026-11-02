//! The management panel: one type draws every management area, on the list
//! every panel shares (cli-commands.md "Management surfaces").
//!
//! ```text
//! ╭─ Skills ──────────────────────────────────────╮
//! │ >                                             │
//! │                                               │
//! │   user                                        │
//! │ › commit      enabled                         │
//! │   review      disabled  shadowed by commit    │
//! │   project                                     │
//! │   deploy      enabled                         │
//! │   Issues                                      │
//! │     /w/.agents/skills/old/SKILL.md: no name   │
//! │   Writes commit messages from the staged diff.│
//! │   /home/u/.agents/skills/commit/SKILL.md      │
//! ╰───── Enter toggle · Ctrl+R reload · Esc close ┘
//! ```
//!
//! The entry's actions decide the keys it offers: the hint names only those,
//! and Enter, Space, Ctrl+E, Ctrl+N, Ctrl+R, Ctrl+T, Ctrl+X and Tab keep one
//! meaning in every area. A remove asks in the hint before it sends; an
//! inspect shows the adapter's detail in place of the list.

use std::cell::Cell;

use crate::management;
use crate::model::session::{
    ActionKind, Area, EntryState, ManagementAction, ManagementDetail, ManagementEntry,
    ManagementRefusal, ManagementReply, ManagementRequest, ManagementResult, ManagementView,
};
use crate::sanitize::{safe, safe_line};
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::{Effect, WriteTarget};
use crate::ui::render::panel::{bordered, key_hints, wrap_words};
use crate::ui::rows::title_plain;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

/// Every English string the panel draws through [`t`]; each locale
/// translates them all.
#[cfg(test)]
pub const STRINGS: &[&str] = &[
    "Skills",
    "Hooks",
    "Scheduler",
    "Esc close",
    "Enter toggle",
    "Ctrl+E edit",
    "Ctrl+N add",
    "Ctrl+R reload",
    "Ctrl+T run",
    "Ctrl+X remove",
    "Tab inspect",
    "Remove {name}? Enter confirm · Esc cancel",
    "↑↓ scroll · Esc back",
    "Loading...",
    "No entries.",
    "No matching entry.",
    "Issues",
    "sources",
    "skills",
    "hooks",
    "command rules",
    "scheduled jobs",
    "jobs",
    "enabled",
    "disabled",
    "invalid",
    "read-only",
    "shadowed by {name}",
    "Working...",
    "Reloaded.",
    "Saved.",
    "Saved {name}.",
    "Added.",
    "Removed {name}.",
    "Ran {name}.",
    "No change.",
    "Your text is kept in {path}.",
    "Enabled {name}.",
    "Disabled {name}.",
    "Reloaded {area}: {entries} entries, {disabled} disabled.",
    "Reloaded {area}: {entries} entries.",
    "The user's own document; always in effect.",
    "There is no hooks file there.",
    "Owned by plugin {plugin}.",
    "The user document turns every hook off (disableAllHooks).",
    "The user document changed since it was read; nothing was written.",
    "Your next edit opens the current document.",
    "Your next add reopens your text.",
    "Added {name}.",
    "Run {name}: {outcome}.",
    "Recent runs of {name}",
    "No run yet.",
    "No next run.",
    "Next run {instant}.",
    "Last run {outcome} at {instant}.",
    "every {minutes} minutes",
    "at {instant}",
    "cron {expression}",
    "cron {expression} in {zone}",
    "The job changed since it was read; nothing was written.",
    "The jobs changed since they were read; nothing was written.",
    "The job changed since it was read; nothing was removed.",
    "The Scheduler has no area document.",
    "No scheduled job has that id.",
    "The document changes no field; nothing was written.",
    "The document deleted {field}, which the owner cannot clear; nothing was written.",
    "This daemon cannot manage {area}.",
    "This daemon lists {area} but cannot toggle them.",
    "This daemon lists {area} but cannot edit them.",
    "This daemon lists {area} but cannot reload them.",
    "This daemon lists {area} but cannot add them.",
    "This daemon lists {area} but cannot remove them.",
    "This daemon lists {area} but cannot run them.",
    "This daemon lists {area} but cannot inspect them.",
    "This workspace's hook settings changed since they were read; nothing was written.",
    "MCP",
    "MCP servers",
    "Ctrl+T test",
    "sign-in required",
    "signed in",
    "sign-in expired",
    "missing scopes: {scopes}",
    "Executable: {command}",
    "Stored arguments (hidden): {count}",
    "Working directory: {cwd}",
    "Environment names: {names}",
    "Endpoint: {endpoint}",
    "Header names: {names}",
    "Bearer token: {state}",
    "OAuth: configured (client ID {id})",
    "OAuth: configured",
    "OAuth: not configured",
    "Metadata URL: {state}",
    "configured",
    "Owned by a plugin; change it through Plugins.",
    "MCP entry {id}",
    "Connection {number} · {status}",
    "Tools have not been discovered.",
    "No live session connections.",
    "Tools of {id}",
    "The server exposes no tools.",
    "The add document needs an id.",
    "The add document needs a source of user or project.",
    "The add document needs a configuration object.",
    "{id} is already in mcp list; remove it first or choose another id.",
    "{id} is already in mcp list; choose another id, or change it through its plugin.",
    "This entry's settings changed since they were read; nothing was written.",
    "The MCP entry changed since it was read; nothing was written.",
    "The MCP entries changed since they were read; nothing was written.",
    "The MCP entry changed since it was read; nothing was removed.",
    "MCP's documents belong to one entry; this save names none.",
    "No MCP server has that id.",
    "This workspace has no project MCP source.",
    "Enabled {id} ({source}).",
    "Disabled {id} ({source}).",
    "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions.",
    "{id} is not in mcp list; withdrew any stored enablement of it in this workspace.",
    "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged.",
    "MCP configuration: {error}",
    "The server rejected the stored credential; sign in again with mcp login.",
    "The server requires additional scopes: {scopes}.",
    "The server requires authorization; run mcp login ID.",
    "The probe ended: {status}.",
    "Command rules",
    "user",
    "Matches only this command.",
    "Matches commands whose first words are these.",
    "Allows a simple command that would otherwise ask.",
    "Denies the command under every preset.",
    "Applies in every Session, in every workspace.",
    "Rule added: {rule}.",
    "Rule already present: {rule}.",
    "Rule removed: {rule}.",
    "That command rule is no longer listed; nothing was removed.",
    "Command rules changed since they were read; nothing was removed.",
    "A command rule is DECISION MATCH COMMAND, such as deny prefix git push.",
    "Command: {command}",
    "Use the line prompt or dimcode permissions.",
];

pub struct ManagementPanel {
    area: Area,
    list: ListPanel,
    view: Option<ManagementView>,
    /// The entry a remove waits to confirm.
    confirming: Option<String>,
    /// A detail the adapter reported, shown in place of the list.
    detail: Option<ShownDetail>,
    /// A request is in flight: only Esc answers.
    busy: bool,
    /// What the last request came to.
    message: Option<Result<String, String>>,
    /// The line the panel was opened with: it stands until the user's first
    /// key here, whatever the opening read answers.
    opening: Option<String>,
    /// Why the read that follows an applied action failed: the action was
    /// done, and the area is not reported.
    read_error: Option<String>,
}

/// A detail in place of the list, with the rows it starts at.
struct ShownDetail {
    title: String,
    lines: Vec<String>,
    /// The first line drawn, kept between frames and clamped to the content.
    top: Cell<usize>,
    /// Lines the window shows; set from the screen height while rendering.
    page: Cell<usize>,
}

impl ShownDetail {
    fn new(detail: ManagementDetail) -> Self {
        Self {
            title: detail.title,
            lines: detail.lines,
            top: Cell::new(0),
            page: Cell::new(6),
        }
    }
}

impl ManagementPanel {
    /// The panel of `area`, opened holding `opening`.
    ///
    /// The line stands where the answers stand, but the opening read does
    /// not replace it: only the user's first key here does.
    pub fn new(area: Area, opening: Option<String>) -> Self {
        Self {
            area,
            list: ListPanel::new(
                title(area),
                hint(area, &[]),
                "Loading...",
                "No entries.",
                "No matching entry.",
                false,
            ),
            view: None,
            confirming: None,
            detail: None,
            busy: true,
            message: None,
            opening,
            read_error: None,
        }
    }

    /// The reply to this panel's request.
    pub fn set_result(&mut self, reply: &ManagementReply) {
        if reply.request.area != self.area {
            return;
        }
        self.busy = false;
        self.confirming = None;
        self.read_error = None;
        match &reply.result {
            ManagementResult::View { view, note } => {
                let view = &**view;
                let name = entry_name(Some(view), self.view.as_ref(), &reply.request.action);
                // The read that opens a panel says nothing; every later
                // answer names what it did.
                self.message = match (&reply.request.action, self.view.is_some()) {
                    (ManagementAction::List, false) => None,
                    (ManagementAction::List, true) => Some(Ok(t("Reloaded.").to_owned())),
                    (action, _) => Some(Ok(match note {
                        Some(note) => note.clone(),
                        None => management::applied_line(action, name.as_deref()),
                    })),
                };
                self.set_view(view.clone());
            }
            ManagementResult::Detail(detail) => {
                self.detail = Some(ShownDetail::new(detail.clone()));
                self.message = None;
            }
            ManagementResult::Applied { note, read_error } => {
                // The action was done, so its own line stands; what failed is
                // the read, which the footer reports once, wrapped.
                let name = entry_name(self.view.as_ref(), None, &reply.request.action);
                self.message = Some(Ok(note.clone().unwrap_or_else(|| {
                    management::applied_line(&reply.request.action, name.as_deref())
                })));
                self.lost_view(read_error);
            }
            ManagementResult::Refused(ManagementRefusal::Refused(error)) => {
                self.message = Some(Err(error.clone()));
                if self.view.is_none() {
                    // Nothing was shown yet: the list says why instead of
                    // keeping "Loading...".
                    self.list.set_items(Err(error.clone()), None);
                }
            }
            ManagementResult::Refused(ManagementRefusal::Conflict { message, view }) => {
                // The owner wrote nothing. A read that failed leaves no view,
                // so the next read retries the area; the link to the stale
                // revision is dropped on every conflict either way.
                self.message = Some(Err(message.clone()));
                match view {
                    Ok(view) => self.set_view((**view).clone()),
                    Err(error) => self.lost_view(error),
                }
            }
        }
        self.refresh_hint();
    }

    /// The area is not shown until the next read: the reply that could not
    /// report it says why, and Ctrl+R reads again. The view goes, so nothing
    /// stale is acted on.
    fn lost_view(&mut self, error: &str) {
        self.view = None;
        self.read_error = Some(error.to_owned());
        self.list.loading();
    }

    fn set_view(&mut self, view: ManagementView) {
        let items = items(&view);
        let selected = self.list.selected_id();
        self.list.set_items(Ok(items), selected.as_deref());
        self.view = Some(view);
        self.refresh_hint();
    }

    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
        self.refresh_hint();
    }

    fn entry(&self, id: &str) -> Option<&ManagementEntry> {
        self.view
            .as_ref()?
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .find(|entry| entry.id == id)
    }

    /// The label of the entry `action` names, from the view the panel shows:
    /// what a surface names the entry by once the panel is gone.
    pub fn label_of(&self, action: &ManagementAction) -> Option<String> {
        entry_name(self.view.as_ref(), None, action)
    }

    /// The area the panel manages: a reply for another area belongs to the
    /// surface, not here.
    pub fn area(&self) -> Area {
        self.area
    }

    fn selected(&self) -> Option<&ManagementEntry> {
        self.entry(&self.list.selected_id()?)
    }

    /// Whether the area or the selected entry offers `kind`.
    fn offers(&self, kind: ActionKind) -> bool {
        self.view
            .as_ref()
            .is_some_and(|view| view.actions.contains(&kind))
            || self
                .selected()
                .is_some_and(|entry| entry.actions.contains(&kind))
    }

    /// Whether the selected entry offers `kind`.
    fn entry_offers(&self, kind: ActionKind) -> bool {
        self.selected()
            .is_some_and(|entry| entry.actions.contains(&kind))
    }

    fn refresh_hint(&mut self) {
        if let Some(id) = &self.confirming {
            let label = self
                .entry(id)
                .map(|entry| safe_line(&entry.label))
                .unwrap_or_else(|| safe_line(id));
            self.list.set_hint(tf(
                "Remove {name}? Enter confirm · Esc cancel",
                &[("name", &label)],
            ));
            return;
        }
        if self.detail.is_some() {
            self.list.set_hint(t("↑↓ scroll · Esc back").to_owned());
            return;
        }
        let mut kinds = match &self.view {
            Some(view) => view.actions.clone(),
            // Nothing is shown yet: Ctrl+R repeats the read.
            None => vec![ActionKind::Reload],
        };
        if let Some(entry) = self.selected() {
            kinds.extend(entry.actions.iter().copied());
        }
        self.list.set_hint(hint(self.area, &kinds));
    }

    /// The editor has the terminal: the panel takes no key until the draft
    /// comes back.
    pub fn saving(&mut self) {
        self.busy = true;
        self.message = Some(Ok(t("Working...").to_owned()));
        self.read_error = None;
    }

    /// The edit could not go on: say why, and let the keys through again.
    pub fn failed(&mut self, error: String) {
        self.busy = false;
        self.message = Some(Err(error));
    }

    /// The document `target` names, which a draft is compared with.
    pub fn document(&self, target: &WriteTarget) -> Option<&serde_json::Value> {
        match target {
            WriteTarget::Create => self.view.as_ref()?.create.as_ref(),
            WriteTarget::Area { .. } => self.view.as_ref()?.document.as_ref(),
            WriteTarget::Entry { id, .. } => self
                .entry(id)?
                .document
                .as_ref()
                .map(|document| &document.document),
        }
    }

    /// The editor left the text as it was, so nothing is written.
    pub fn unchanged(&mut self) {
        self.busy = false;
        self.message = Some(Ok(t("No change.").to_owned()));
    }

    /// A draft was written but the owner kept the stored document: the
    /// message names where the user's text still is.
    pub fn kept(&mut self, area: Area, path: &str) {
        if area != self.area {
            return;
        }
        self.keep_text(path, &[]);
    }

    /// A conflict kept the user's text: the message names what the next
    /// attempt does with it and where the text is.
    pub fn conflicted(&mut self, area: Area, creating: bool, path: &str) {
        if area != self.area {
            return;
        }
        let next = if creating {
            t("Your next add reopens your text.")
        } else {
            t("Your next edit opens the current document.")
        };
        self.keep_text(path, &[next]);
    }

    /// Adds `sentences`, then where the text is, to the message the reply
    /// left, which stands first.
    fn keep_text(&mut self, path: &str, sentences: &[&str]) {
        let kept = tf("Your text is kept in {path}.", &[("path", path)]);
        let mut tail = sentences.join(" ");
        if !tail.is_empty() {
            tail.push(' ');
        }
        tail.push_str(&kept);
        self.message = Some(match self.message.take() {
            Some(Err(error)) => Err(format!("{error} {tail}")),
            _ => Err(tail),
        });
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        // The line the panel was opened with belongs to the panel, not to
        // the user: any key here takes it away.
        self.opening = None;
        if self.detail.is_some() {
            return self.detail_key(key);
        }
        // The confirmation takes every key: Enter removes, anything else
        // cancels and changes nothing.
        if let Some(id) = self.confirming.take() {
            self.refresh_hint();
            if key.code != KeyCode::Enter {
                return Effect::None;
            }
            self.busy = true;
            return Effect::Management(ManagementRequest {
                area: self.area,
                action: ManagementAction::Remove { id },
            });
        }
        if key.code == KeyCode::Esc {
            return match self.list.handle_key(key) {
                Event::Close => Effect::Close,
                _ => {
                    self.refresh_hint();
                    Effect::None
                }
            };
        }
        if self.busy {
            return Effect::None;
        }
        if key.code == KeyCode::Char('r') && key.mods.contains(Modifiers::CTRL) {
            // With a view the key is the area's reload, when it has one;
            // with none it repeats the read that failed, without asking the
            // owner to rescan anything.
            let action = if self.view.is_some() {
                if !self.offers(ActionKind::Reload) {
                    return Effect::None;
                }
                ManagementAction::Reload
            } else {
                ManagementAction::List
            };
            self.busy = true;
            return Effect::Management(ManagementRequest {
                area: self.area,
                action,
            });
        }
        // Ctrl holds the actions off the filter, which takes every plain
        // letter.
        if key.code == KeyCode::Char('e') && key.mods.contains(Modifiers::CTRL) {
            return self.edit();
        }
        if key.code == KeyCode::Char('n') && key.mods.contains(Modifiers::CTRL) {
            return self.add();
        }
        if key.code == KeyCode::Char('x') && key.mods.contains(Modifiers::CTRL) {
            return self.remove();
        }
        if key.code == KeyCode::Char('t') && key.mods.contains(Modifiers::CTRL) {
            return self.send_entry(ActionKind::Run, |id| ManagementAction::Run { id });
        }
        if key.code == KeyCode::Tab {
            // Tab moves the list in other panels; here it opens the entry.
            if !self.entry_offers(ActionKind::Inspect) {
                return Effect::None;
            }
            return self.send_entry(ActionKind::Inspect, |id| ManagementAction::Inspect { id });
        }
        // Space toggles as Enter does, as in `/settings > Execution`.
        let key = if matches!(key.code, KeyCode::Char(' ')) {
            KeyEvent::new(KeyCode::Enter, key.mods)
        } else {
            key
        };
        let event = self.list.handle_key(key);
        self.refresh_hint();
        let Event::Pick(id) = event else {
            return Effect::None;
        };
        let Some(entry) = self.entry(&id) else {
            return Effect::None;
        };
        if !entry.actions.contains(&ActionKind::Toggle) {
            return Effect::None;
        }
        // An entry that has no on/off state has nothing to toggle.
        let Some(enabled) = entry.enabled else {
            return Effect::None;
        };
        let request = ManagementRequest {
            area: self.area,
            action: ManagementAction::Toggle {
                id: entry.id.clone(),
                enabled: !enabled,
            },
        };
        self.busy = true;
        Effect::Management(request)
    }

    /// The key an open detail takes: the arrows and PageUp/PageDown scroll
    /// it, Esc returns to the list with the selection kept.
    fn detail_key(&mut self, key: KeyEvent) -> Effect {
        let Some(detail) = &self.detail else {
            return Effect::None;
        };
        let page = detail.page.get().max(1);
        let top = match key.code {
            KeyCode::Esc => {
                self.detail = None;
                self.refresh_hint();
                return Effect::None;
            }
            KeyCode::Up => detail.top.get().saturating_sub(1),
            KeyCode::Down => detail.top.get().saturating_add(1),
            KeyCode::PageUp => detail.top.get().saturating_sub(page),
            KeyCode::PageDown => detail.top.get().saturating_add(page),
            _ => return Effect::None,
        };
        detail.top.set(top);
        Effect::None
    }

    /// Asks for the selected entry's detail, which the adapter answers in
    /// place of the list.
    fn send_entry(
        &mut self,
        kind: ActionKind,
        action: impl FnOnce(String) -> ManagementAction,
    ) -> Effect {
        let Some(entry) = self.selected() else {
            return Effect::None;
        };
        if !entry.actions.contains(&kind) {
            return Effect::None;
        }
        let request = ManagementRequest {
            area: self.area,
            action: action(entry.id.clone()),
        };
        self.busy = true;
        Effect::Management(request)
    }

    /// Waits for the confirmation the next key answers.
    fn remove(&mut self) -> Effect {
        let Some(entry) = self.selected() else {
            return Effect::None;
        };
        if !entry.actions.contains(&ActionKind::Remove) {
            return Effect::None;
        }
        self.confirming = Some(entry.id.clone());
        self.refresh_hint();
        Effect::None
    }

    /// A new entry, from the document the adapter supplies.
    fn add(&mut self) -> Effect {
        if !self.offers(ActionKind::Add) {
            return Effect::None;
        }
        let Some(document) = self.view.as_ref().and_then(|view| view.create.clone()) else {
            return Effect::None;
        };
        self.open_editor(WriteTarget::Create, &document)
    }

    /// The selected entry's document for the external editor, or the area's,
    /// with the revision this panel read it at.
    fn edit(&mut self) -> Effect {
        if !self.offers(ActionKind::Edit) {
            return Effect::None;
        }
        let entry = self.selected().and_then(|entry| {
            let document = entry.document.as_ref()?;
            Some((
                WriteTarget::Entry {
                    id: entry.id.clone(),
                    revision: document.revision.clone(),
                },
                document.document.clone(),
            ))
        });
        let (target, document) = match entry {
            Some(found) => found,
            None => {
                let area = self
                    .view
                    .as_ref()
                    .and_then(|view| Some((view.revision.clone()?, view.document.clone()?)));
                match area {
                    Some((revision, document)) => (WriteTarget::Area { revision }, document),
                    None => return Effect::None,
                }
            }
        };
        self.open_editor(target, &document)
    }

    fn open_editor(&mut self, target: WriteTarget, document: &serde_json::Value) -> Effect {
        let Ok(json) = serde_json::to_string_pretty(document) else {
            return Effect::None;
        };
        self.busy = true;
        Effect::EditManagement {
            area: self.area,
            target,
            text: format!("{json}\n"),
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        if let Some(detail) = &self.detail {
            return render_detail(detail, width, rows, p);
        }
        let inner = width.saturating_sub(4).max(1);
        let mut footer = Vec::new();
        if let Some(entry) = self.selected() {
            for line in &entry.detail {
                for row in wrap_words(&safe(line), inner.saturating_sub(2).max(1)) {
                    footer.push(format!("  {}", fg(&row, &p.muted)));
                }
            }
        }
        if let Some(opening) = &self.opening {
            for row in wrap_words(&safe(opening), inner) {
                footer.push(fg(&row, &p.muted));
            }
        }
        if let Some(message) = &self.message {
            let (text, color) = match message {
                Ok(text) => (safe(text), &p.success),
                Err(text) => (safe(text), &p.danger),
            };
            for row in wrap_words(&text, inner) {
                footer.push(fg(&row, color));
            }
        }
        if let Some(error) = &self.read_error {
            for row in wrap_words(&safe(error), inner) {
                footer.push(fg(&row, &p.danger));
            }
        }
        if !footer.is_empty() {
            footer.insert(0, String::new());
        }
        footer.truncate(rows.saturating_sub(2 + 3 + 1));
        self.list.render_with_footer(width, rows, p, &footer)
    }
}

/// A detail in place of the list: its title in the border, its lines below.
fn render_detail(detail: &ShownDetail, width: usize, rows: usize, p: &Palette) -> Vec<String> {
    let inner = width.saturating_sub(4).max(1);
    let mut lines = Vec::new();
    for line in &detail.lines {
        lines.extend(wrap_words(&safe(line), inner));
    }
    let page = rows.saturating_sub(2).max(1);
    detail.page.set(page);
    let start = detail.top.get().min(lines.len().saturating_sub(page));
    detail.top.set(start);
    let body: Vec<String> = lines
        .into_iter()
        .skip(start)
        .take(page)
        .map(|line| fg(&line, &p.text))
        .collect();
    bordered(
        &body,
        width,
        Some(rows),
        &p.border,
        Some(&title_plain(&safe_line(&detail.title), inner, p)),
        Some(&key_hints(t("↑↓ scroll · Esc back"), p)),
    )
}

/// What a reload with no panel reports: the area's size, then what its owner
/// could not use.
pub fn reloaded(area: Area, view: &ManagementView) -> String {
    let entries: Vec<&ManagementEntry> = view
        .groups
        .iter()
        .flat_map(|group| &group.entries)
        .collect();
    // The disabled count says what an on/off state is, so only an area whose
    // entries report one has it: an area of values, or an empty one, does
    // not claim every entry is enabled.
    let count = entries.len().to_string();
    let size = if entries.iter().any(|entry| entry.enabled.is_some()) {
        let disabled = entries
            .iter()
            .filter(|entry| entry.enabled == Some(false))
            .count();
        tf(
            "Reloaded {area}: {entries} entries, {disabled} disabled.",
            &[
                ("area", t(title(area))),
                ("entries", &count),
                ("disabled", &disabled.to_string()),
            ],
        )
    } else {
        tf(
            "Reloaded {area}: {entries} entries.",
            &[("area", t(title(area))), ("entries", &count)],
        )
    };
    let mut lines = vec![size];
    lines.extend(view.issues.iter().map(|issue| format!("  {issue}")));
    lines.join("\n")
}

/// The panel title of `area`, in English: the list translates it as it
/// draws.
fn title(area: Area) -> &'static str {
    match area {
        Area::Skills => "Skills",
        Area::Hooks => "Hooks",
        Area::Mcp => "MCP",
        Area::Scheduled => "Scheduler",
        Area::Permissions => "Command rules",
    }
}

/// One label per kind, in the actions table's order; an area that names its
/// run kind's operation uses that word instead of `run`.
fn label(area: Area, kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Toggle => "Enter toggle",
        ActionKind::Edit => "Ctrl+E edit",
        ActionKind::Reload => "Ctrl+R reload",
        ActionKind::Add => "Ctrl+N add",
        ActionKind::Remove => "Ctrl+X remove",
        ActionKind::Run if area == Area::Mcp => "Ctrl+T test",
        ActionKind::Run => "Ctrl+T run",
        ActionKind::Inspect => "Tab inspect",
    }
}

/// The keys the area and the selected entry offer, as one hint.
fn hint(area: Area, kinds: &[ActionKind]) -> String {
    let mut parts: Vec<&str> = [
        ActionKind::Toggle,
        ActionKind::Edit,
        ActionKind::Reload,
        ActionKind::Add,
        ActionKind::Remove,
        ActionKind::Run,
        ActionKind::Inspect,
    ]
    .into_iter()
    .filter(|kind| kinds.contains(kind))
    .map(|kind| label(area, kind))
    .collect();
    parts.push("Esc close");
    parts.join(" · ")
}

/// The rows: each group's heading, its entries, then what the owner could
/// not use.
fn items(view: &ManagementView) -> Vec<Item> {
    let mut items = Vec::new();
    for group in &view.groups {
        items.push(Item::heading(safe_line(&group.heading)));
        for entry in &group.entries {
            // The owner's text is drawn on one line, so it is made safe and
            // kept on that line before it is stored: an escape sequence, a
            // newline or a tab in a name cannot reach or break the screen.
            let mut item = Item::new(
                entry.id.clone(),
                safe_line(&entry.label),
                safe_line(&states(entry).join("  ")),
            );
            item.search = format!("{} {}", entry.id, entry.detail.join(" "));
            items.push(item);
        }
    }
    if !view.issues.is_empty() {
        items.push(Item::heading(t("Issues")));
        items.extend(view.issues.iter().map(|issue| Item::heading(safe(issue))));
    }
    items
}

/// An entry's states: on or off, when it has one, then the ones its owner
/// adds.
fn states(entry: &ManagementEntry) -> Vec<String> {
    let mut states = Vec::new();
    match entry.enabled {
        Some(true) => states.push(t("enabled").to_owned()),
        Some(false) => states.push(t("disabled").to_owned()),
        None => {}
    }
    for state in &entry.states {
        states.push(match state {
            EntryState::Invalid => t("invalid").to_owned(),
            EntryState::Shadowed { by } => tf("shadowed by {name}", &[("name", by)]),
            EntryState::ReadOnly => t("read-only").to_owned(),
        });
    }
    states
}

/// The label of the entry `action` names, from the views the panel holds:
/// the one the reply brought, then the one it showed before the write. An
/// entry an action removed is gone from the fresh view, so the older one
/// names it.
fn entry_name(
    view: Option<&ManagementView>,
    previous: Option<&ManagementView>,
    action: &ManagementAction,
) -> Option<String> {
    view.into_iter()
        .chain(previous)
        .find_map(|view| management::entry_name(view, action))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::{
        EntryDocument, ManagementGroup, ManagementReply, ManagementResult,
    };
    use crate::ui::render::panel::{strip_escapes, visible_width};
    use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};

    fn palette() -> Palette {
        terminal_palette(ColorMode::TrueColor, Appearance::Dark)
    }

    fn entry(
        id: &str,
        label: &str,
        enabled: Option<bool>,
        states: Vec<EntryState>,
        actions: Vec<ActionKind>,
    ) -> ManagementEntry {
        ManagementEntry {
            id: id.into(),
            label: label.into(),
            detail: vec![
                format!("{label} does its work."),
                format!("/w/.agents/skills/{id}/SKILL.md"),
            ],
            enabled,
            states,
            actions,
            document: None,
        }
    }

    fn view() -> ManagementView {
        ManagementView {
            revision: None,
            document: None,
            create: None,
            actions: vec![ActionKind::Reload],
            groups: vec![
                ManagementGroup {
                    heading: "user".into(),
                    entries: vec![
                        entry(
                            "user:commit",
                            "commit",
                            Some(true),
                            Vec::new(),
                            vec![ActionKind::Toggle],
                        ),
                        entry(
                            "user:review",
                            "review",
                            Some(false),
                            vec![EntryState::Shadowed {
                                by: "commit".into(),
                            }],
                            vec![ActionKind::Toggle],
                        ),
                        entry("user:locked", "locked", Some(true), Vec::new(), vec![]),
                    ],
                },
                ManagementGroup {
                    heading: "project".into(),
                    entries: vec![entry(
                        "workspace:/w:deploy",
                        "deploy",
                        Some(true),
                        Vec::new(),
                        vec![ActionKind::Toggle],
                    )],
                },
            ],
            issues: vec!["/w/.agents/skills/old/SKILL.md: no name".into()],
        }
    }

    fn reply(area: Area, action: ManagementAction, result: ManagementResult) -> ManagementReply {
        ManagementReply {
            request: ManagementRequest { area, action },
            result,
        }
    }

    fn view_reply(area: Area, action: ManagementAction, view: ManagementView) -> ManagementReply {
        reply(
            area,
            action,
            ManagementResult::View {
                view: Box::new(view),
                note: None,
            },
        )
    }

    fn opened() -> ManagementPanel {
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&view_reply(Area::Skills, ManagementAction::List, view()));
        panel
    }

    fn shown(panel: &ManagementPanel, width: usize, rows: usize) -> Vec<String> {
        let lines = panel.render(width, rows, &palette());
        for line in &lines {
            assert_eq!(visible_width(line), width, "{line:?}");
        }
        lines.iter().map(|line| strip_escapes(line)).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), Modifiers::CTRL)
    }

    /// The rows group entries under their source, name each entry's state,
    /// and the selected one says what it is below the list.
    #[test]
    fn rows_group_by_source_and_name_the_selected_entry() {
        let panel = opened();
        let text = shown(&panel, 72, 20).join("\n");
        assert!(text.contains("user"), "{text}");
        assert!(text.contains("project"), "{text}");
        assert!(text.contains("enabled"), "{text}");
        assert!(text.contains("disabled  shadowed by commit"), "{text}");
        assert!(
            text.contains("commit does its work.")
                && text.contains("/w/.agents/skills/user:commit/SKILL.md"),
            "the selected entry says what it is: {text}"
        );
        assert!(
            text.contains("/w/.agents/skills/old/SKILL.md: no name"),
            "the issues follow the entries: {text}"
        );
        assert!(
            text.contains("Enter toggle · Ctrl+R reload · Esc close"),
            "{text}"
        );
        for width in [16, 24, 40] {
            for rows in [6, 10, 20] {
                let lines = shown(&panel, width, rows);
                assert!(lines.len() <= rows, "{width}x{rows}: {lines:?}");
            }
        }
    }

    /// The entry's actions decide its keys: Enter toggles what offers it,
    /// Ctrl+R reloads, and the hint names only what the selection offers.
    #[test]
    fn keys_follow_the_actions_of_the_selected_entry() {
        let mut panel = opened();
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Management(request) => {
                assert_eq!(request.area, Area::Skills);
                assert_eq!(
                    request.action,
                    ManagementAction::Toggle {
                        id: "user:commit".into(),
                        enabled: false
                    }
                );
            }
            _ => panic!("expected a toggle"),
        }
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::Toggle {
                id: "user:commit".into(),
                enabled: false,
            },
            view(),
        ));
        assert!(
            shown(&panel, 72, 20)
                .join("\n")
                .contains("Disabled commit."),
            "{}",
            shown(&panel, 72, 20).join("\n")
        );
        panel.handle_key(key(KeyCode::End));
        let text = shown(&panel, 72, 20).join("\n");
        assert!(
            text.contains("Enter toggle · Ctrl+R reload · Esc close"),
            "{text}"
        );
        match panel.handle_key(ctrl('r')) {
            Effect::Management(request) => assert_eq!(request.action, ManagementAction::Reload),
            _ => panic!("expected a reload"),
        }
        // A reload keeps the entry the user was on.
        panel.set_result(&view_reply(Area::Skills, ManagementAction::Reload, view()));
        assert!(shown(&panel, 72, 20).join("\n").contains("Reloaded."));
        // An entry that offers no toggle ignores Enter and says so by its
        // hint.
        let mut panel = opened();
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Down));
        let text = shown(&panel, 72, 20).join("\n");
        assert!(
            text.contains("Ctrl+R reload · Esc close") && !text.contains("Enter toggle"),
            "{text}"
        );
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        // Space toggles as Enter does.
        panel.handle_key(key(KeyCode::Home));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char(' '))),
            Effect::Management(_)
        ));
    }

    /// A line the panel was opened with stands through the opening read,
    /// however it turns out, and the user's first key takes it away.
    #[test]
    fn the_line_a_panel_was_opened_with_stands_until_the_first_key() {
        let opening = "Use the line prompt or dimcode permissions.";
        let mut panel = ManagementPanel::new(Area::Permissions, Some(opening.into()));
        assert!(
            shown(&panel, 80, 20).join(" ").contains(opening),
            "the line is drawn while the read is out"
        );
        panel.set_result(&view_reply(
            Area::Permissions,
            ManagementAction::List,
            ManagementView {
                actions: vec![ActionKind::Reload],
                ..ManagementView::default()
            },
        ));
        assert!(
            shown(&panel, 80, 20).join(" ").contains(opening),
            "the opening read leaves the line"
        );
        panel.handle_key(key(KeyCode::Down));
        let text = shown(&panel, 80, 20).join(" ");
        assert!(!text.contains(opening), "{text}");
    }

    /// The Command rules area is titled for what it lists, and its hint
    /// names the kinds it has.
    #[test]
    fn the_command_rules_area_names_its_title_and_kinds() {
        let mut panel = ManagementPanel::new(Area::Permissions, None);
        panel.set_result(&view_reply(
            Area::Permissions,
            ManagementAction::List,
            ManagementView {
                revision: Some("r1".into()),
                document: None,
                create: Some(serde_json::json!({
                    "command": "replace-with-a-command",
                    "match": "prefix",
                    "decision": "deny"
                })),
                actions: vec![ActionKind::Reload, ActionKind::Add],
                groups: vec![ManagementGroup {
                    heading: "user".into(),
                    entries: vec![entry(
                        "deny prefix git push",
                        "deny prefix git push",
                        None,
                        Vec::new(),
                        vec![ActionKind::Remove],
                    )],
                }],
                issues: Vec::new(),
            },
        ));
        let text = shown(&panel, 80, 20).join("\n");
        assert!(text.contains("Command rules"), "{text}");
        assert!(text.contains("user"), "{text}");
        assert!(
            text.contains("Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Esc close"),
            "{text}"
        );
        // A rule has a decision, not an on/off state.
        let row = text
            .lines()
            .find(|line| line.contains("deny prefix git push"))
            .expect("the rule has a row");
        assert!(
            !row.contains("enabled") && !row.contains("disabled"),
            "{row}"
        );
    }

    /// A panel with nothing to show says so, and a failed read keeps the
    /// error rather than an empty list.
    #[test]
    fn an_empty_area_and_a_failed_read_say_so() {
        let mut panel = ManagementPanel::new(Area::Skills, None);
        assert!(shown(&panel, 40, 10).join("\n").contains("Loading..."));
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::List,
            ManagementView::default(),
        ));
        assert!(shown(&panel, 40, 10).join("\n").contains("No entries."));
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::List,
            ManagementResult::Refused(ManagementRefusal::Refused(
                "This daemon cannot manage skills.".into(),
            )),
        ));
        assert!(
            shown(&panel, 60, 10)
                .join(" ")
                .contains("This daemon cannot manage skills.")
        );
    }

    /// A hooks view: the user document can be edited, a plugin's source is
    /// read-only, and an unreadable source says it is invalid.
    fn hooks_view() -> ManagementView {
        ManagementView {
            revision: Some("user:1".into()),
            document: Some(serde_json::json!({"hooks": {}})),
            create: None,
            actions: vec![ActionKind::Reload],
            groups: vec![ManagementGroup {
                heading: "sources".into(),
                entries: vec![
                    ManagementEntry {
                        id: "user".into(),
                        label: "user".into(),
                        detail: vec!["The user's own document; always in effect.".into()],
                        enabled: Some(true),
                        states: Vec::new(),
                        actions: vec![ActionKind::Edit],
                        document: None,
                    },
                    ManagementEntry {
                        id: "plugin:kit".into(),
                        label: "plugin:kit".into(),
                        detail: vec!["Owned by plugin kit.".into()],
                        enabled: Some(true),
                        states: vec![EntryState::ReadOnly, EntryState::Invalid],
                        actions: vec![],
                        document: None,
                    },
                ],
            }],
            issues: Vec::new(),
        }
    }

    fn opened_hooks() -> ManagementPanel {
        let mut panel = ManagementPanel::new(Area::Hooks, None);
        panel.set_result(&view_reply(
            Area::Hooks,
            ManagementAction::List,
            hooks_view(),
        ));
        panel
    }

    /// Ctrl+E opens the document it read, with the revision it read it at;
    /// a read-only source offers no write at all.
    #[test]
    fn edit_offers_the_document_and_a_read_only_source_offers_no_write() {
        let mut panel = opened_hooks();
        let text = shown(&panel, 72, 20).join("\n");
        assert!(text.contains("read-only  invalid"), "{text}");
        match panel.handle_key(ctrl('e')) {
            Effect::EditManagement { area, target, text } => {
                assert_eq!(area, Area::Hooks);
                assert_eq!(
                    target,
                    WriteTarget::Area {
                        revision: "user:1".into()
                    }
                );
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                    serde_json::json!({"hooks": {}})
                );
            }
            _ => panic!("expected an edit"),
        }
        // The editor has the terminal: no key reaches the area until it is
        // back.
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        panel.saving();
        assert!(shown(&panel, 72, 20).join("\n").contains("Working..."));
        panel.kept(Area::Hooks, "/home/u/edit/hooks.json");
        let text = shown(&panel, 72, 20).join(" ");
        assert!(
            text.contains("Your text is kept in /home/u/edit/hooks.json."),
            "{text}"
        );
        // A read-only source ignores Enter and Ctrl+E.
        let mut panel = opened_hooks();
        panel.handle_key(key(KeyCode::End));
        let text = shown(&panel, 72, 20).join("\n");
        assert!(!text.contains("Ctrl+E"), "no edit key is offered: {text}");
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(matches!(panel.handle_key(ctrl('e')), Effect::None));
    }

    /// An area whose daemon reports no document offers no edit, and a
    /// reload without a panel names the area's size in its own title.
    #[test]
    fn an_area_without_a_document_offers_no_edit() {
        let mut view = hooks_view();
        view.document = None;
        view.groups[0].entries[0].actions = vec![ActionKind::Reload];
        let mut panel = ManagementPanel::new(Area::Hooks, None);
        panel.set_result(&view_reply(Area::Hooks, ManagementAction::List, view));
        assert!(matches!(panel.handle_key(ctrl('e')), Effect::None));
        let text = reloaded(Area::Hooks, &hooks_view());
        assert!(
            text.starts_with("Reloaded Hooks: 2 entries, 0 disabled."),
            "{text}"
        );
    }

    /// The reload summary counts disabled entries only for an area whose
    /// entries report a state, and names the area as its panel does.
    #[test]
    fn a_reload_summary_fits_the_area() {
        // A rule has a decision, not a state: nothing is off.
        let rules = ManagementView {
            groups: vec![ManagementGroup {
                heading: "user".into(),
                entries: vec![entry(
                    "deny prefix git push",
                    "deny prefix git push",
                    None,
                    Vec::new(),
                    vec![ActionKind::Remove],
                )],
            }],
            ..ManagementView::default()
        };
        assert_eq!(
            reloaded(Area::Permissions, &rules),
            "Reloaded Command rules: 1 entries."
        );
        // An empty area has no entry that could be on or off either.
        assert_eq!(
            reloaded(Area::Skills, &ManagementView::default()),
            "Reloaded Skills: 0 entries."
        );
    }

    /// Reload belongs to the area: it works on an empty area where no entry
    /// is selected to offer it, and after a failed read Ctrl+R reads again.
    #[test]
    fn reload_works_on_an_empty_area_and_after_a_failed_read() {
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::List,
            ManagementView {
                actions: vec![ActionKind::Reload],
                ..ManagementView::default()
            },
        ));
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("No entries."), "{text}");
        assert!(text.contains("Ctrl+R reload · Esc close"), "{text}");
        match panel.handle_key(ctrl('r')) {
            Effect::Management(request) => {
                assert_eq!(request.area, Area::Skills);
                assert_eq!(request.action, ManagementAction::Reload);
            }
            _ => panic!("expected a reload"),
        }
        // A read that failed leaves nothing to select; Ctrl+R reads again.
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::List,
            ManagementResult::Refused(ManagementRefusal::Refused(
                "This daemon cannot manage skills.".into(),
            )),
        ));
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("This daemon cannot manage skills."), "{text}");
        assert!(text.contains("Ctrl+R reload"), "{text}");
        match panel.handle_key(ctrl('r')) {
            Effect::Management(request) => assert_eq!(request.action, ManagementAction::List),
            _ => panic!("expected a read"),
        }
    }

    /// A conflict writes nothing, drops the entry's link to the stale
    /// revision and shows the area as its owner now reports it.
    #[test]
    fn a_conflict_shows_the_newer_document() {
        let mut panel = opened_hooks();
        panel.set_result(&reply(
            Area::Hooks,
            ManagementAction::Save {
                id: None,
                expected_revision: "user:1".into(),
                document: serde_json::json!({"hooks": {}}),
            },
            ManagementResult::Refused(ManagementRefusal::Conflict {
                message: "The user document changed since it was read.".into(),
                view: Ok(Box::new(ManagementView {
                    revision: Some("user:2".into()),
                    document: Some(serde_json::json!({"hooks": {}})),
                    actions: vec![ActionKind::Reload],
                    ..hooks_view()
                })),
            }),
        ));
        let text = shown(&panel, 72, 20).join(" ");
        assert!(
            text.contains("The user document changed since it was read."),
            "{text}"
        );
        panel.kept(Area::Hooks, "/home/u/edit/hooks-1.json");
        let text = shown(&panel, 72, 20).join(" ");
        assert!(text.contains("Your text is kept in"), "{text}");
        assert!(text.contains("hooks-1.json"), "{text}");
        assert_eq!(
            panel.document(&WriteTarget::Area {
                revision: "user:2".into()
            }),
            Some(&serde_json::json!({"hooks": {}}))
        );
    }

    /// A conflict kept the user's text: the panel says what the next attempt
    /// does with it, by kind, and where the text is.
    #[test]
    fn a_conflict_names_what_the_next_attempt_does_with_the_kept_text() {
        for (creating, sentence) in [
            (false, "Your next edit opens the current document."),
            (true, "Your next add reopens your text."),
        ] {
            let mut panel = opened_hooks();
            panel.set_result(&reply(
                Area::Hooks,
                ManagementAction::Save {
                    id: None,
                    expected_revision: "user:1".into(),
                    document: serde_json::json!({"hooks": {}}),
                },
                ManagementResult::Refused(ManagementRefusal::Conflict {
                    message: "The user document changed since it was read; nothing was written."
                        .into(),
                    view: Ok(Box::new(hooks_view())),
                }),
            ));
            panel.conflicted(Area::Hooks, creating, "/home/u/edit/hooks-1.json");
            let text = shown(&panel, 72, 20).join(" ");
            assert!(text.contains(sentence), "{text}");
            assert!(text.contains("Your text is kept in"), "{text}");
            assert!(text.contains("hooks-1.json"), "{text}");
        }
    }

    /// Filtering keeps the entry's keys: the hint follows the new selection.
    #[test]
    fn a_filter_narrows_the_list_and_keeps_the_keys_of_its_selection() {
        let mut panel = opened();
        for c in "lock".chars() {
            panel.handle_key(key(KeyCode::Char(c)));
        }
        let text = shown(&panel, 72, 20).join("\n");
        assert!(text.contains("locked"), "{text}");
        assert!(!text.contains("deploy"), "{text}");
        assert!(text.contains("Ctrl+R reload · Esc close"), "{text}");
        assert!(!text.contains("Enter toggle"), "{text}");
    }

    /// An area whose entries offer every kind, as the Scheduler's jobs do.
    fn catalogue() -> ManagementView {
        ManagementView {
            revision: None,
            document: None,
            create: Some(serde_json::json!({"name": "example"})),
            actions: vec![ActionKind::Reload, ActionKind::Add],
            groups: vec![ManagementGroup {
                heading: "jobs".into(),
                entries: vec![
                    ManagementEntry {
                        id: "job-1".into(),
                        label: "nightly".into(),
                        detail: vec!["every 60 minutes".into(), "no run yet".into()],
                        enabled: Some(true),
                        states: Vec::new(),
                        actions: vec![
                            ActionKind::Toggle,
                            ActionKind::Edit,
                            ActionKind::Remove,
                            ActionKind::Run,
                            ActionKind::Inspect,
                        ],
                        document: Some(EntryDocument {
                            revision: "3".into(),
                            document: serde_json::json!({"name": "nightly"}),
                        }),
                    },
                    ManagementEntry {
                        id: "job-2".into(),
                        label: "release".into(),
                        detail: vec!["at 2030-01-01T00:00:00Z".into()],
                        enabled: Some(false),
                        states: vec![EntryState::Invalid],
                        actions: vec![ActionKind::Toggle],
                        document: Some(EntryDocument {
                            revision: "7".into(),
                            document: serde_json::json!({"name": "release"}),
                        }),
                    },
                ],
            }],
            issues: Vec::new(),
        }
    }

    fn opened_catalogue() -> ManagementPanel {
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::List,
            catalogue(),
        ));
        panel
    }

    /// A remove asks in the hint first: Enter sends the delete, any other
    /// key cancels and changes nothing.
    #[test]
    fn remove_asks_in_the_hint_and_the_next_key_answers_it() {
        let mut panel = opened_catalogue();
        let text = shown(&panel, 120, 20).join("\n");
        assert!(text.contains("Ctrl+X remove"), "{text}");
        assert!(matches!(panel.handle_key(ctrl('x')), Effect::None));
        let text = shown(&panel, 120, 20).join("\n");
        assert!(
            text.contains("Remove nightly? Enter confirm · Esc cancel"),
            "{text}"
        );
        // Any other key cancels: the panel keeps its list and its filter.
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('q'))),
            Effect::None
        ));
        let text = shown(&panel, 120, 20).join("\n");
        assert!(!text.contains("Remove nightly?"), "{text}");
        assert!(
            text.contains("release"),
            "the keys reached no filter: {text}"
        );
        // Enter confirms the one the panel asked about.
        panel.handle_key(ctrl('x'));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Management(request) => assert_eq!(
                request.action,
                ManagementAction::Remove { id: "job-1".into() }
            ),
            _ => panic!("expected a remove"),
        }
    }

    /// Tab and Ctrl+X are the panel's even while the filter holds text: the
    /// filter takes every plain letter, not those keys.
    #[test]
    fn tab_and_ctrl_x_reach_the_panel_past_the_filter() {
        let mut panel = opened_catalogue();
        for c in "night".chars() {
            panel.handle_key(key(KeyCode::Char(c)));
        }
        assert!(matches!(panel.handle_key(ctrl('x')), Effect::None));
        assert!(
            shown(&panel, 120, 20)
                .join("\n")
                .contains("Remove nightly? Enter confirm · Esc cancel"),
            "a filter does not take Ctrl+X"
        );
        panel.handle_key(key(KeyCode::Char('z')));
        match panel.handle_key(key(KeyCode::Tab)) {
            Effect::Management(request) => {
                assert!(matches!(request.action, ManagementAction::Inspect { .. }))
            }
            _ => panic!("a filter does not take Tab"),
        }
    }

    /// Tab asks for the entry's detail and shows it in place of the list;
    /// the first Esc returns to the list, the second closes the panel.
    #[test]
    fn inspect_shows_the_detail_and_the_first_esc_returns_to_the_list() {
        let mut panel = opened_catalogue();
        match panel.handle_key(key(KeyCode::Tab)) {
            Effect::Management(request) => assert_eq!(
                request.action,
                ManagementAction::Inspect { id: "job-1".into() }
            ),
            _ => panic!("expected an inspect"),
        }
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Inspect { id: "job-1".into() },
            ManagementResult::Detail(ManagementDetail {
                title: "Recent runs of nightly".into(),
                lines: vec!["2026-11-01T09:00:12.000Z  completed".into()],
            }),
        ));
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("Recent runs of nightly"), "{text}");
        assert!(
            text.contains("2026-11-01T09:00:12.000Z  completed"),
            "{text}"
        );
        assert!(
            !text.contains("release"),
            "the detail replaces the list: {text}"
        );
        assert!(text.contains("↑↓ scroll · Esc back"), "{text}");
        // Esc returns to the list, with the selection kept. The wrapped hint
        // takes two rows of the list here, so the panel is drawn two taller.
        assert!(matches!(panel.handle_key(key(KeyCode::Esc)), Effect::None));
        let text = shown(&panel, 60, 14).join("\n");
        assert!(
            text.contains("nightly") && text.contains("release"),
            "{text}"
        );
        assert!(matches!(panel.handle_key(key(KeyCode::Esc)), Effect::Close));
    }

    /// A long detail scrolls with the arrows and PageUp/PageDown.
    #[test]
    fn a_long_detail_scrolls() {
        let mut panel = opened_catalogue();
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Inspect { id: "job-1".into() },
            ManagementResult::Detail(ManagementDetail {
                title: "Recent runs of nightly".into(),
                lines: (0..30).map(|index| format!("run {index}")).collect(),
            }),
        ));
        let first = shown(&panel, 40, 8);
        assert!(first.join("\n").contains("run 0"));
        for _ in 0..4 {
            panel.handle_key(key(KeyCode::Down));
        }
        let text = shown(&panel, 40, 8).join("\n");
        assert!(text.contains("run 4"), "{text}");
        assert!(!text.contains("run 0"), "{text}");
        panel.handle_key(key(KeyCode::PageDown));
        let text = shown(&panel, 40, 8).join("\n");
        assert!(text.contains("run 10"), "{text}");
        for _ in 0..40 {
            panel.handle_key(key(KeyCode::Down));
        }
        let last = shown(&panel, 40, 8).join("\n");
        assert!(
            last.contains("run 29"),
            "the window stops at the end: {last}"
        );
    }

    /// Add belongs to the area: it needs no selection, so it works on an
    /// empty area and opens the document the adapter supplied.
    #[test]
    fn add_needs_no_selection_and_opens_the_adapters_document() {
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::List,
            ManagementView {
                actions: vec![ActionKind::Reload, ActionKind::Add],
                create: Some(serde_json::json!({"name": "example"})),
                ..ManagementView::default()
            },
        ));
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("No entries."), "{text}");
        assert!(
            text.contains("Ctrl+R reload · Ctrl+N add · Esc close"),
            "{text}"
        );
        match panel.handle_key(ctrl('n')) {
            Effect::EditManagement { area, target, text } => {
                assert_eq!(area, Area::Skills);
                assert_eq!(target, WriteTarget::Create);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                    serde_json::json!({"name": "example"})
                );
            }
            _ => panic!("expected an editor draft"),
        }
        // An area with no create document offers no add.
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::List,
            ManagementView {
                actions: vec![ActionKind::Add],
                ..ManagementView::default()
            },
        ));
        assert!(matches!(panel.handle_key(ctrl('n')), Effect::None));
    }

    /// An entry's own document is what Ctrl+E edits, with the entry's id and
    /// the revision it was read at.
    #[test]
    fn edit_offers_an_entrys_own_document() {
        let mut panel = opened_catalogue();
        let target = match panel.handle_key(ctrl('e')) {
            Effect::EditManagement { target, text, .. } => {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                    serde_json::json!({"name": "nightly"})
                );
                target
            }
            _ => panic!("expected an edit"),
        };
        assert_eq!(
            target,
            WriteTarget::Entry {
                id: "job-1".into(),
                revision: "3".into()
            }
        );
        assert_eq!(
            panel.document(&target),
            Some(&serde_json::json!({"name": "nightly"}))
        );
    }

    /// Run sends the area's operation for the selected entry, and the
    /// adapter's note is the message the panel shows.
    #[test]
    fn run_sends_the_entrys_operation_and_shows_what_the_owner_answered() {
        let mut panel = opened_catalogue();
        match panel.handle_key(ctrl('t')) {
            Effect::Management(request) => {
                assert_eq!(request.action, ManagementAction::Run { id: "job-1".into() })
            }
            _ => panic!("expected a run"),
        }
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Run { id: "job-1".into() },
            ManagementResult::View {
                view: Box::new(catalogue()),
                note: Some("Run nightly: skipped (overlap).".into()),
            },
        ));
        let text = shown(&panel, 100, 20).join(" ");
        assert!(text.contains("Run nightly: skipped (overlap)."), "{text}");
    }

    /// A remove names the entry the area no longer reports.
    #[test]
    fn a_remove_names_the_entry_it_removed() {
        let mut panel = opened_catalogue();
        panel.handle_key(ctrl('x'));
        panel.handle_key(key(KeyCode::Enter));
        let mut after = catalogue();
        after.groups[0].entries.retain(|entry| entry.id != "job-1");
        panel.set_result(&view_reply(
            Area::Skills,
            ManagementAction::Remove { id: "job-1".into() },
            after,
        ));
        let text = shown(&panel, 100, 20).join(" ");
        assert!(text.contains("Removed nightly."), "{text}");
        assert!(!text.contains("Removed job-1."), "{text}");
    }

    /// An applied action always carries its line, even when the read-back
    /// failed, and the panel draws it with the read error once.
    #[test]
    fn an_applied_remove_shows_its_line_and_draws_the_read_error_once() {
        let mut panel = opened_catalogue();
        panel.handle_key(ctrl('x'));
        panel.handle_key(key(KeyCode::Enter));
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Remove { id: "job-1".into() },
            ManagementResult::Applied {
                note: None,
                read_error: "the scheduler is unavailable".into(),
            },
        ));
        let text = shown(&panel, 100, 20).join("\n");
        assert!(text.contains("Removed nightly."), "{text}");
        assert!(!text.contains("Removed job-1."), "{text}");
        assert_eq!(
            text.matches("the scheduler is unavailable").count(),
            1,
            "the read error is drawn once: {text}"
        );
    }

    /// A toggle whose read-back failed names the entry by the label the
    /// panel holds, not by the id the action was sent with.
    #[test]
    fn an_applied_toggle_names_the_entrys_label() {
        let mut panel = opened_catalogue();
        panel.handle_key(key(KeyCode::Enter));
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Toggle {
                id: "job-1".into(),
                enabled: false,
            },
            ManagementResult::Applied {
                note: None,
                read_error: "the scheduler is unavailable".into(),
            },
        ));
        let text = shown(&panel, 100, 20).join("\n");
        assert!(text.contains("Disabled nightly."), "{text}");
        assert!(!text.contains("Disabled job-1."), "{text}");
    }

    /// A run whose read-back fails keeps the owner's answer, and the read
    /// error is drawn once, in the footer.
    #[test]
    fn an_applied_run_keeps_the_owners_answer_and_draws_the_read_error_once() {
        let mut panel = opened_catalogue();
        panel.handle_key(ctrl('t'));
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Run { id: "job-1".into() },
            ManagementResult::Applied {
                note: Some("Run nightly: skipped (overlap).".into()),
                read_error: "the scheduler is unavailable".into(),
            },
        ));
        let text = shown(&panel, 100, 20).join("\n");
        assert!(text.contains("Run nightly: skipped (overlap)."), "{text}");
        assert_eq!(
            text.matches("the scheduler is unavailable").count(),
            1,
            "the read error is drawn once: {text}"
        );
    }

    /// Owner text is made safe before the panel draws it: a heading, a name,
    /// a detail line, an issue, the note and an inspect's detail cannot carry
    /// an escape sequence to the screen.
    #[test]
    fn owner_text_is_sanitized_before_it_is_drawn() {
        let mut view = catalogue();
        view.groups[0].heading = "jobs\u{1b}[2J".into();
        view.groups[0].entries[0].label = "nightly\u{1b}[2J".into();
        view.groups[0].entries[0].detail = vec!["every 60 minutes\u{1b}[2J".into()];
        view.issues = vec!["an issue\u{1b}[2J".into()];
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Run { id: "job-1".into() },
            ManagementResult::View {
                view: Box::new(view),
                note: Some("Run nightly\u{1b}[2J.".into()),
            },
        ));
        let text = panel.render(100, 20, &palette()).join("\n");
        assert!(!text.contains("[2J"), "{text}");
        assert!(text.contains("nightly"), "{text}");
        // The detail's title and lines are made safe the same way.
        panel.set_result(&reply(
            Area::Skills,
            ManagementAction::Inspect { id: "job-1".into() },
            ManagementResult::Detail(ManagementDetail {
                title: "Recent runs of nightly\u{1b}[2J".into(),
                lines: vec!["2026-11-01T09:00:12.000Z  completed\u{1b}[2J".into()],
            }),
        ));
        let text = panel.render(60, 12, &palette()).join("\n");
        assert!(!text.contains("[2J"), "{text}");
        assert!(text.contains("Recent runs of nightly"), "{text}");
    }

    /// The MCP panel is titled by its area name and names the run kind's
    /// operation, `test`, in the hint; the run key still sends a run.
    #[test]
    fn the_mcp_panel_names_its_run_kind_test() {
        let view = ManagementView {
            revision: None,
            document: None,
            create: Some(serde_json::json!({})),
            actions: vec![ActionKind::Reload],
            groups: vec![ManagementGroup {
                heading: "user".into(),
                entries: vec![entry(
                    "docs",
                    "docs",
                    Some(true),
                    Vec::new(),
                    vec![ActionKind::Run, ActionKind::Inspect],
                )],
            }],
            issues: Vec::new(),
        };
        let mut panel = ManagementPanel::new(Area::Mcp, None);
        panel.set_result(&view_reply(Area::Mcp, ManagementAction::List, view));
        let text = shown(&panel, 100, 20).join("\n");
        assert!(text.contains("MCP"), "the title is the area's: {text}");
        assert!(
            text.contains("Ctrl+T test") && !text.contains("Ctrl+T run"),
            "{text}"
        );
        match panel.handle_key(ctrl('t')) {
            Effect::Management(request) => {
                assert_eq!(request.area, Area::Mcp);
                assert_eq!(request.action, ManagementAction::Run { id: "docs".into() });
            }
            _ => panic!("expected a run"),
        }
    }
}
