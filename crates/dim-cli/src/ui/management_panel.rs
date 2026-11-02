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
//! and Enter, Space, Ctrl+E and Ctrl+R keep one meaning in every area.

use crate::model::session::{
    ActionKind, Area, EntryState, ManagementAction, ManagementEntry, ManagementRefusal,
    ManagementReply, ManagementRequest, ManagementResult, ManagementView,
};
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::wrap_words;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

/// Every English string the panel draws through [`t`]; each locale
/// translates them all.
#[cfg(test)]
pub const STRINGS: &[&str] = &[
    "Skills",
    "Hooks",
    "Esc close",
    "Enter toggle",
    "Ctrl+E edit",
    "Ctrl+R reload",
    "Loading...",
    "No entries.",
    "No matching entry.",
    "Issues",
    "sources",
    "skills",
    "enabled",
    "disabled",
    "invalid",
    "read-only",
    "shadowed by {name}",
    "Working...",
    "Reloaded.",
    "Saved.",
    "No change.",
    "Your text is kept in {path}.",
    "Enabled {name}.",
    "Disabled {name}.",
    "Reloaded {area}: {entries} entries, {disabled} disabled.",
    "The user's own document; always in effect.",
    "There is no hooks file there.",
    "Owned by plugin {plugin}.",
    "The user document turns every hook off (disableAllHooks).",
    "The user document changed since it was read; nothing was written. Your next edit opens the current document.",
    "This workspace's hook settings changed since they were read; nothing was written.",
];

pub struct ManagementPanel {
    area: Area,
    list: ListPanel,
    view: Option<ManagementView>,
    /// A request is in flight: only Esc answers.
    busy: bool,
    /// What the last request came to.
    message: Option<Result<String, String>>,
    /// Why the read that follows an applied action failed: the action was
    /// done, and the area is not reported.
    read_error: Option<String>,
}

impl ManagementPanel {
    pub fn new(area: Area) -> Self {
        Self {
            area,
            list: ListPanel::new(
                title(area),
                hint(&[]),
                "Loading...",
                "No entries.",
                "No matching entry.",
                false,
            ),
            view: None,
            busy: true,
            message: None,
            read_error: None,
        }
    }

    /// The reply to this panel's request.
    pub fn set_result(&mut self, reply: &ManagementReply) {
        if reply.request.area != self.area {
            return;
        }
        self.busy = false;
        self.read_error = None;
        match &reply.result {
            ManagementResult::View(view) => {
                // The read that opens a panel says nothing; every later
                // answer names what it did.
                self.message = match (&reply.request.action, self.view.is_some()) {
                    (ManagementAction::List, false) => None,
                    (ManagementAction::List, true) => Some(Ok(t("Reloaded.").to_owned())),
                    (action, _) => Some(Ok(outcome(action, view))),
                };
                self.set_view(view.clone());
            }
            ManagementResult::Applied { read_error } => {
                // The action was done, so its own message stands; what failed
                // is the read, which the footer reports once, wrapped. The
                // area has no view until the next read.
                let shown = self.view.take().unwrap_or_default();
                self.message = Some(Ok(outcome(&reply.request.action, &shown)));
                self.read_error = Some(read_error.clone());
                self.list.loading();
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
                    Err(error) => {
                        self.view = None;
                        self.read_error = Some(error.clone());
                        self.list.loading();
                    }
                }
            }
        }
        self.refresh_hint();
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

    fn refresh_hint(&mut self) {
        let mut kinds = match &self.view {
            Some(view) => view.actions.clone(),
            // Nothing is shown yet: Ctrl+R repeats the read.
            None => vec![ActionKind::Reload],
        };
        if let Some(entry) = self.selected() {
            kinds.extend(entry.actions.iter().copied());
        }
        self.list.set_hint(hint(&kinds));
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

    /// The document this panel read, which a draft is compared with.
    pub fn document(&self) -> Option<&serde_json::Value> {
        self.view.as_ref()?.document.as_ref()
    }

    /// The editor left the text as it was, so nothing is written.
    pub fn unchanged(&mut self) {
        self.busy = false;
        self.message = Some(Ok(t("No change.").to_owned()));
    }

    /// A draft was written but the owner kept the stored document: the
    /// message names where the user's text still is.
    pub fn kept(&mut self, path: &str) {
        let kept = tf("Your text is kept in {path}.", &[("path", path)]);
        self.message = Some(match self.message.take() {
            Some(Err(error)) => Err(format!("{error} {kept}")),
            _ => Err(kept),
        });
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
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
        let request = ManagementRequest {
            area: self.area,
            action: ManagementAction::Toggle {
                id: entry.id.clone(),
                enabled: !entry.enabled,
            },
        };
        self.busy = true;
        Effect::Management(request)
    }

    /// The selected entry's document for the external editor, with the
    /// revision this panel read it at.
    fn edit(&mut self) -> Effect {
        if !self.offers(ActionKind::Edit) {
            return Effect::None;
        }
        let Some((revision, document)) = self
            .view
            .as_ref()
            .and_then(|view| Some((view.revision.clone()?, view.document.clone()?)))
        else {
            return Effect::None;
        };
        let Ok(json) = serde_json::to_string_pretty(&document) else {
            return Effect::None;
        };
        self.busy = true;
        Effect::EditManagement {
            area: self.area,
            revision,
            text: format!("{json}\n"),
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let mut footer = Vec::new();
        if let Some(entry) = self.selected() {
            for line in &entry.detail {
                for row in wrap_words(line, inner.saturating_sub(2).max(1)) {
                    footer.push(format!("  {}", fg(&row, &p.muted)));
                }
            }
        }
        if let Some(message) = &self.message {
            let (text, color) = match message {
                Ok(text) => (text, &p.success),
                Err(text) => (text, &p.danger),
            };
            for row in wrap_words(text, inner) {
                footer.push(fg(&row, color));
            }
        }
        if let Some(error) = &self.read_error {
            for row in wrap_words(error, inner) {
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

/// What a reload with no panel reports: the area's size, then what its owner
/// could not use.
pub fn reloaded(area: Area, view: &ManagementView) -> String {
    let entries: Vec<&ManagementEntry> = view
        .groups
        .iter()
        .flat_map(|group| &group.entries)
        .collect();
    let disabled = entries.iter().filter(|entry| !entry.enabled).count();
    let mut lines = vec![tf(
        "Reloaded {area}: {entries} entries, {disabled} disabled.",
        &[
            ("area", t(title(area))),
            ("entries", &entries.len().to_string()),
            ("disabled", &disabled.to_string()),
        ],
    )];
    lines.extend(view.issues.iter().map(|issue| format!("  {issue}")));
    lines.join("\n")
}

/// The panel title of `area`, in English: the list translates it as it
/// draws.
fn title(area: Area) -> &'static str {
    match area {
        Area::Skills => "Skills",
        Area::Hooks => "Hooks",
    }
}

/// One label per kind, in the actions table's order.
fn label(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Toggle => "Enter toggle",
        ActionKind::Edit => "Ctrl+E edit",
        ActionKind::Reload => "Ctrl+R reload",
    }
}

/// The keys the area and the selected entry offer, as one hint.
fn hint(kinds: &[ActionKind]) -> String {
    let mut parts: Vec<&str> = [ActionKind::Toggle, ActionKind::Edit, ActionKind::Reload]
        .into_iter()
        .filter(|kind| kinds.contains(kind))
        .map(label)
        .collect();
    parts.push("Esc close");
    parts.join(" · ")
}

/// The rows: each group's heading, its entries, then what the owner could
/// not use.
fn items(view: &ManagementView) -> Vec<Item> {
    let mut items = Vec::new();
    for group in &view.groups {
        items.push(Item::heading(group.heading.clone()));
        for entry in &group.entries {
            let mut item = Item::new(
                entry.id.clone(),
                entry.label.clone(),
                states(entry).join("  "),
            );
            item.search = format!("{} {}", entry.id, entry.detail.join(" "));
            items.push(item);
        }
    }
    if !view.issues.is_empty() {
        items.push(Item::heading(t("Issues")));
        items.extend(view.issues.iter().map(|issue| Item::heading(issue.clone())));
    }
    items
}

/// An entry's states: on or off, then the ones its owner adds.
fn states(entry: &ManagementEntry) -> Vec<String> {
    let mut states = vec![t(if entry.enabled { "enabled" } else { "disabled" }).to_owned()];
    for state in &entry.states {
        states.push(match state {
            EntryState::Invalid => t("invalid").to_owned(),
            EntryState::Shadowed { by } => tf("shadowed by {name}", &[("name", by)]),
            EntryState::ReadOnly => t("read-only").to_owned(),
        });
    }
    states
}

/// The notice a successful change leaves.
fn outcome(action: &ManagementAction, view: &ManagementView) -> String {
    let name = |id: &str| {
        view.groups
            .iter()
            .flat_map(|group| &group.entries)
            .find(|entry| entry.id == id)
            .map(|entry| entry.label.clone())
            .unwrap_or_else(|| id.to_owned())
    };
    match action {
        ManagementAction::Toggle { id, enabled } => tf(
            if *enabled {
                "Enabled {name}."
            } else {
                "Disabled {name}."
            },
            &[("name", &name(id))],
        ),
        ManagementAction::Save { .. } => t("Saved.").to_owned(),
        ManagementAction::List | ManagementAction::Reload => t("Reloaded.").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::{ManagementGroup, ManagementReply};
    use crate::ui::render::panel::{strip_escapes, visible_width};
    use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};

    fn palette() -> Palette {
        terminal_palette(ColorMode::TrueColor, Appearance::Dark)
    }

    fn entry(
        id: &str,
        label: &str,
        enabled: bool,
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
        }
    }

    fn view() -> ManagementView {
        ManagementView {
            revision: None,
            document: None,
            actions: vec![ActionKind::Reload],
            groups: vec![
                ManagementGroup {
                    heading: "user".into(),
                    entries: vec![
                        entry(
                            "user:commit",
                            "commit",
                            true,
                            Vec::new(),
                            vec![ActionKind::Toggle],
                        ),
                        entry(
                            "user:review",
                            "review",
                            false,
                            vec![EntryState::Shadowed {
                                by: "commit".into(),
                            }],
                            vec![ActionKind::Toggle],
                        ),
                        entry("user:locked", "locked", true, Vec::new(), vec![]),
                    ],
                },
                ManagementGroup {
                    heading: "project".into(),
                    entries: vec![entry(
                        "workspace:/w:deploy",
                        "deploy",
                        true,
                        Vec::new(),
                        vec![ActionKind::Toggle],
                    )],
                },
            ],
            issues: vec!["/w/.agents/skills/old/SKILL.md: no name".into()],
        }
    }

    fn opened() -> ManagementPanel {
        let mut panel = ManagementPanel::new(Area::Skills);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(view()),
        });
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
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::Toggle {
                    id: "user:commit".into(),
                    enabled: false,
                },
            },
            result: ManagementResult::View(view()),
        });
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
        match panel.handle_key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CTRL)) {
            Effect::Management(request) => assert_eq!(request.action, ManagementAction::Reload),
            _ => panic!("expected a reload"),
        }
        // A reload keeps the entry the user was on.
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::Reload,
            },
            result: ManagementResult::View(view()),
        });
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

    /// A panel with nothing to show says so, and a failed read keeps the
    /// error rather than an empty list.
    #[test]
    fn an_empty_area_and_a_failed_read_say_so() {
        let mut panel = ManagementPanel::new(Area::Skills);
        assert!(shown(&panel, 40, 10).join("\n").contains("Loading..."));
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(ManagementView::default()),
        });
        assert!(shown(&panel, 40, 10).join("\n").contains("No entries."));
        let mut panel = ManagementPanel::new(Area::Skills);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::Refused(ManagementRefusal::Refused(
                "This daemon cannot manage skills.".into(),
            )),
        });
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
            actions: vec![ActionKind::Reload],
            groups: vec![ManagementGroup {
                heading: "sources".into(),
                entries: vec![
                    ManagementEntry {
                        id: "user".into(),
                        label: "user".into(),
                        detail: vec!["The user's own document; always in effect.".into()],
                        enabled: true,
                        states: Vec::new(),
                        actions: vec![ActionKind::Edit],
                    },
                    ManagementEntry {
                        id: "plugin:kit".into(),
                        label: "plugin:kit".into(),
                        detail: vec!["Owned by plugin kit.".into()],
                        enabled: true,
                        states: vec![EntryState::ReadOnly, EntryState::Invalid],
                        actions: vec![],
                    },
                ],
            }],
            issues: Vec::new(),
        }
    }

    /// Ctrl+E opens the document it read, with the revision it read it at;
    /// a read-only source offers no write at all.
    #[test]
    fn edit_offers_the_document_and_a_read_only_source_offers_no_write() {
        let mut panel = ManagementPanel::new(Area::Hooks);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Hooks,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(hooks_view()),
        });
        let text = shown(&panel, 72, 20).join("\n");
        assert!(text.contains("read-only  invalid"), "{text}");
        match panel.handle_key(KeyEvent::new(KeyCode::Char('e'), Modifiers::CTRL)) {
            Effect::EditManagement {
                area,
                revision,
                text,
            } => {
                assert_eq!(area, Area::Hooks);
                assert_eq!(revision, "user:1");
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
        panel.kept("/home/u/edit/hooks.json");
        let text = shown(&panel, 72, 20).join(" ");
        assert!(
            text.contains("Your text is kept in /home/u/edit/hooks.json."),
            "{text}"
        );
        // A read-only source ignores Enter and Ctrl+E.
        let mut panel = ManagementPanel::new(Area::Hooks);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Hooks,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(hooks_view()),
        });
        panel.handle_key(key(KeyCode::End));
        let text = shown(&panel, 72, 20).join("\n");
        assert!(!text.contains("Ctrl+E"), "no edit key is offered: {text}");
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(matches!(
            panel.handle_key(KeyEvent::new(KeyCode::Char('e'), Modifiers::CTRL)),
            Effect::None
        ));
    }

    /// An area whose daemon reports no document offers no edit, and a
    /// reload without a panel names the area's size in its own title.
    #[test]
    fn an_area_without_a_document_offers_no_edit() {
        let mut view = hooks_view();
        view.document = None;
        view.groups[0].entries[0].actions = vec![ActionKind::Reload];
        let mut panel = ManagementPanel::new(Area::Hooks);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Hooks,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(view),
        });
        assert!(matches!(
            panel.handle_key(KeyEvent::new(KeyCode::Char('e'), Modifiers::CTRL)),
            Effect::None
        ));
        let text = reloaded(Area::Hooks, &hooks_view());
        assert!(
            text.starts_with("Reloaded Hooks: 2 entries, 0 disabled."),
            "{text}"
        );
    }

    /// Reload belongs to the area: it works on an empty area where no entry
    /// is selected to offer it, and after a failed read Ctrl+R reads again.
    #[test]
    fn reload_works_on_an_empty_area_and_after_a_failed_read() {
        let mut panel = ManagementPanel::new(Area::Skills);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(ManagementView {
                actions: vec![ActionKind::Reload],
                ..ManagementView::default()
            }),
        });
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("No entries."), "{text}");
        assert!(text.contains("Ctrl+R reload · Esc close"), "{text}");
        match panel.handle_key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CTRL)) {
            Effect::Management(request) => {
                assert_eq!(request.area, Area::Skills);
                assert_eq!(request.action, ManagementAction::Reload);
            }
            _ => panic!("expected a reload"),
        }
        // A read that failed leaves nothing to select; Ctrl+R reads again.
        let mut panel = ManagementPanel::new(Area::Skills);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::Refused(ManagementRefusal::Refused(
                "This daemon cannot manage skills.".into(),
            )),
        });
        let text = shown(&panel, 60, 12).join("\n");
        assert!(text.contains("This daemon cannot manage skills."), "{text}");
        assert!(text.contains("Ctrl+R reload"), "{text}");
        match panel.handle_key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CTRL)) {
            Effect::Management(request) => assert_eq!(request.action, ManagementAction::List),
            _ => panic!("expected a read"),
        }
    }

    /// A conflict writes nothing, drops the entry's link to the stale
    /// revision and shows the area as its owner now reports it.
    #[test]
    fn a_conflict_shows_the_newer_document() {
        let mut panel = ManagementPanel::new(Area::Hooks);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Hooks,
                action: ManagementAction::List,
            },
            result: ManagementResult::View(hooks_view()),
        });
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Hooks,
                action: ManagementAction::Save {
                    expected_revision: "user:1".into(),
                    document: serde_json::json!({"hooks": {}}),
                },
            },
            result: ManagementResult::Refused(ManagementRefusal::Conflict {
                message: "The user document changed since it was read.".into(),
                view: Ok(Box::new(ManagementView {
                    revision: Some("user:2".into()),
                    document: Some(serde_json::json!({"hooks": {}})),
                    actions: vec![ActionKind::Reload],
                    ..hooks_view()
                })),
            }),
        });
        let text = shown(&panel, 72, 20).join(" ");
        assert!(
            text.contains("The user document changed since it was read."),
            "{text}"
        );
        panel.kept("/home/u/edit/hooks-1.json");
        let text = shown(&panel, 72, 20).join(" ");
        assert!(text.contains("Your text is kept in"), "{text}");
        assert!(text.contains("hooks-1.json"), "{text}");
        assert_eq!(panel.document(), Some(&serde_json::json!({"hooks": {}})));
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
}
