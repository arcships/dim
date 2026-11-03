//! The `/plugins` panel: every plugin name PluginManager lists
//! (`plugin.list`), with the candidate in use, its state and why; the
//! selected one is turned on or off (off after a confirmation) or reloaded.
//! `/plugins install PATH` opens it with the install's outcome.
//!
//! ```text
//! ╭─ Plugins ─────────────────────────────────────╮
//! │                                               │
//! │ › web            running  default             │
//! │   PluginManager  running  builtin  locked     │
//! │   my-tool        failed   my-tool             │
//! │                                               │
//! │   Uses default · candidates: builtin, default │
//! │   Turned off web.                             │
//! ╰─────── Enter on/off · r reload · Esc close ─╯
//! ```

use std::cell::Cell;

use crate::model::session::{PluginAction, PluginItem, PluginReply};
use crate::ui::i18n::{t, tf};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{bordered, confirm_panel, follow_window, key_hints, wrap_words};
use crate::ui::rows::{Columns, Kind, Row, panel_row, title_plain};
use crate::ui::runtime_panel::{runtime_row, waited_runtime};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::truncate_cells;
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

const HINT: &str = "Enter on/off · r reload · Ctrl+R refresh · Esc close";
const WAITING_HINT: &str = "Enter open /runtime · o on/off · r reload · Esc close";
const CONFIRM_HINT: &str = "Tab switch  Enter confirm  Esc cancel";
const LOCKED: &str = "Locked: Dim needs it, so it cannot be turned off.";

pub struct PluginsPanel {
    items: Vec<PluginItem>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    /// A change is in flight: keys other than moving and closing wait.
    busy: bool,
    /// The last change's outcome: a notice, or an error in the danger colour.
    message: Option<Result<String, String>>,
    /// The name whose turning off awaits confirmation, with the focused
    /// button: 0 = Turn off, 1 = Cancel.
    confirming: Option<(String, usize)>,
    /// First row of the visible window, kept between frames.
    window: Cell<usize>,
}

impl PluginsPanel {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            busy: false,
            message: None,
            confirming: None,
            window: Cell::new(0),
        }
    }

    /// A panel waiting for the outcome of `/plugins install PATH`.
    pub fn installing(path: &std::path::Path) -> Self {
        let mut panel = Self::new();
        panel.busy = true;
        panel.message = Some(Ok(tf(
            "Installing {path}...",
            &[("path", &path.display().to_string())],
        )));
        panel
    }

    pub fn set_items(&mut self, items: Result<Vec<PluginItem>, String>) {
        self.loading = false;
        match items {
            Ok(items) => {
                self.items = items;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    /// The outcome of a change, then the plugins listed after it; an
    /// install selects the installed plugin.
    pub fn set_result(
        &mut self,
        action: &PluginAction,
        result: Result<PluginReply, String>,
        items: Result<Vec<PluginItem>, String>,
    ) {
        self.busy = false;
        let name = match action {
            PluginAction::Install(_) => result.as_ref().ok().and_then(|reply| reply.name.clone()),
            PluginAction::Enable(name)
            | PluginAction::Disable(name)
            | PluginAction::Reload(name) => Some(name.clone()),
        };
        self.message = Some(result.map(|reply| outcome(action, &reply)));
        self.set_items(items);
        if let Some(at) = name.and_then(|name| self.items.iter().position(|item| item.name == name))
        {
            self.selected = at;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some((_, focus)) = &mut self.confirming {
            return match key.code {
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                    *focus = 1 - *focus;
                    Effect::None
                }
                KeyCode::Char('y') => self.turn_off(),
                KeyCode::Enter if *focus == 0 => self.turn_off(),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Enter => {
                    self.confirming = None;
                    Effect::None
                }
                _ => Effect::None,
            };
        }
        let ctrl = key.mods.contains(Modifiers::CTRL);
        match key.code {
            KeyCode::Esc => Effect::Close,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Effect::None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
                Effect::None
            }
            KeyCode::Home => {
                self.selected = 0;
                Effect::None
            }
            KeyCode::End => {
                self.selected = self.items.len().saturating_sub(1);
                Effect::None
            }
            _ if self.busy => Effect::None,
            KeyCode::Char('r') if ctrl => {
                self.loading = true;
                Effect::Refresh
            }
            KeyCode::Enter => match self.items.get(self.selected).and_then(waited_runtime) {
                Some(runtime) => Effect::OpenRuntime(runtime.to_owned()),
                None => self.toggle(),
            },
            KeyCode::Char('o') => self.toggle(),
            KeyCode::Char('r') => match self.items.get(self.selected) {
                Some(item) => self.change(PluginAction::Reload(item.name.clone())),
                None => Effect::None,
            },
            _ => Effect::None,
        }
    }

    /// Turns the selected name on, or off after a confirmation; a locked one
    /// only says why it stays.
    fn toggle(&mut self) -> Effect {
        match self.items.get(self.selected) {
            Some(item) if item.off => self.change(PluginAction::Enable(item.name.clone())),
            Some(item) if item.locked => {
                self.message = Some(Err(t(LOCKED).to_owned()));
                Effect::None
            }
            Some(item) => {
                self.confirming = Some((item.name.clone(), 1));
                Effect::None
            }
            None => Effect::None,
        }
    }

    fn turn_off(&mut self) -> Effect {
        match self.confirming.take() {
            Some((name, _)) => self.change(PluginAction::Disable(name)),
            None => Effect::None,
        }
    }

    fn change(&mut self, action: PluginAction) -> Effect {
        self.busy = true;
        self.message = None;
        Effect::Plugin(action)
    }

    /// The keys for the selected row: one that waits for a runtime opens it.
    fn hint(&self) -> &'static str {
        match self.items.get(self.selected) {
            Some(item) if waited_runtime(item).is_some() => t(WAITING_HINT),
            _ => t(HINT),
        }
    }

    /// `width` columns including borders; `rows` is the panel's row budget.
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        if let Some((name, focus)) = &self.confirming {
            return confirm_panel(
                &tf("Turn off {name}?", &[("name", name)]),
                &[
                    t("Its tools and content stop in new turns until you turn it on again.")
                        .to_owned(),
                ],
                &[t("Turn off").to_owned(), t("Cancel").to_owned()],
                *focus,
                t(CONFIRM_HINT),
                width,
                p,
            );
        }
        let inner = width.saturating_sub(4).max(1);
        let mut footer = Vec::new();
        if let Some(item) = self.items.get(self.selected) {
            for line in about(item) {
                let color = if line.1 { &p.danger } else { &p.muted };
                for row in wrap_words(&line.0, inner.saturating_sub(2).max(1)) {
                    footer.push(format!("  {}", fg(&row, color)));
                }
            }
        }
        if self.busy && self.message.is_none() {
            footer.push(fg(&truncate_cells(t("Working..."), inner), &p.muted));
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
        let mut content = vec![String::new()];
        let list: Vec<Row> = self.items.iter().map(row).collect();
        if list.is_empty() {
            let text = if self.loading {
                t("Loading plugins...").to_owned()
            } else if let Some(error) = &self.error {
                error.clone()
            } else {
                t("No plugins.").to_owned()
            };
            let color = if self.error.is_some() {
                &p.danger
            } else {
                &p.muted
            };
            for row in wrap_words(&text, inner) {
                content.push(fg(&row, color));
            }
        } else {
            if let Some(error) = &self.error {
                for row in wrap_words(error, inner) {
                    footer.insert(0, fg(&row, &p.danger));
                }
            }
            let budget = rows.saturating_sub(2 + 2 + footer.len()).max(1);
            footer.truncate(rows.saturating_sub(2 + 2 + budget.min(list.len())));
            let page = budget.min(list.len());
            let columns = Columns::of(&list, inner);
            let start = follow_window(self.window.get(), self.selected, page, list.len());
            self.window.set(start);
            for (offset, row) in list.iter().skip(start).take(page).enumerate() {
                content.push(panel_row(
                    row,
                    start + offset == self.selected,
                    inner,
                    columns,
                    p,
                ));
            }
        }
        if !footer.is_empty() {
            content.push(String::new());
            content.extend(footer);
        }
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(&title_plain(t("Plugins"), inner, p)),
            Some(&key_hints(self.hint(), p)),
        )
    }
}

/// A name's row: its state, the candidate in use and whether it is locked.
fn row(item: &PluginItem) -> Row {
    let state = match item.status.as_deref() {
        _ if item.off => t("off"),
        Some("running") => t("running"),
        Some("pending") => t("pending"),
        Some("failed") => t("failed"),
        Some("stopped") => t("stopped"),
        Some("off") => t("off"),
        _ => t("on"),
    };
    let mut detail = state.to_owned();
    if let Some(runtime) = waited_runtime(item) {
        detail.push_str(" · ");
        detail.push_str(&tf(
            "waits for the {runtime} runtime",
            &[("runtime", runtime)],
        ));
    }
    if let Some(in_use) = &item.in_use {
        detail.push_str("  ");
        detail.push_str(in_use);
    }
    if item.locked {
        detail.push_str("  ");
        detail.push_str(t("locked"));
    }
    Row {
        kind: Kind::Item,
        category: String::new(),
        label: item.name.clone(),
        detail,
        key: None,
        unavailable: false,
    }
}

/// What the selected name says about itself: `(line, is a problem)`.
fn about(item: &PluginItem) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    let candidates: Vec<&str> = item.candidates.iter().map(|c| c.id.as_str()).collect();
    let mut summary = tf(
        "Candidates: {candidates}",
        &[("candidates", &candidates.join(", "))],
    );
    if let Some(in_use) = &item.in_use {
        summary = format!("{} · {summary}", tf("Uses {use}", &[("use", in_use)]));
    }
    lines.push((summary, false));
    if item.locked {
        lines.push((t(LOCKED).to_owned(), false));
    }
    if let Some(error) = &item.error {
        lines.push((tf("Error: {error}", &[("error", error)]), true));
    }
    let others: Vec<&str> = item
        .waiting_for
        .iter()
        .map(String::as_str)
        .filter(|wait| runtime_row(wait).is_none())
        .collect();
    if !others.is_empty() {
        let names = others.join(", ");
        lines.push((tf("Waiting for: {names}", &[("names", &names)]), true));
    }
    for refused in &item.refused {
        lines.push((
            tf(
                "Refused {kind}: {error}",
                &[("kind", &refused.kind), ("error", &refused.error)],
            ),
            true,
        ));
    }
    lines
}

/// The notice a successful change leaves. A failed row or refused content
/// shows on the selected plugin, which the change selects.
fn outcome(action: &PluginAction, reply: &PluginReply) -> String {
    let mut parts = vec![match action {
        PluginAction::Enable(name) => tf("Turned on {name}.", &[("name", name)]),
        PluginAction::Disable(name) => tf("Turned off {name}.", &[("name", name)]),
        PluginAction::Reload(name) => tf("Reloaded {name}.", &[("name", name)]),
        PluginAction::Install(_) => {
            let name = reply.name.as_deref().unwrap_or_default();
            let id = reply.id.as_deref().unwrap_or_default();
            let text = if reply.enabled == Some(false) {
                "Installed {name} ({id}); it is not turned on."
            } else {
                "Installed {name} ({id})."
            };
            tf(text, &[("name", name), ("id", id)])
        }
    }];
    if reply.restart_required {
        parts.push(
            t("Restart required: it loads after the App Server restarts (dimcode daemon stop).")
                .to_owned(),
        );
    }
    if !reply.skipped.is_empty() {
        let kinds = reply.skipped.join(", ");
        parts.push(tf(
            "Skipped {kinds}: their owner is not running.",
            &[("kinds", &kinds)],
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::{PluginCandidate, PluginRefusal};
    use crate::ui::render::panel::{strip_escapes, visible_width};
    use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};

    fn palette() -> Palette {
        terminal_palette(ColorMode::TrueColor, Appearance::Dark)
    }

    fn item(name: &str, in_use: &str, status: &str) -> PluginItem {
        PluginItem {
            name: name.into(),
            in_use: Some(in_use.into()),
            off: status == "off",
            locked: false,
            candidates: vec![PluginCandidate { id: in_use.into() }],
            status: Some(status.into()),
            error: None,
            waiting_for: Vec::new(),
            refused: Vec::new(),
        }
    }

    fn listed() -> Vec<PluginItem> {
        let mut manager = item("PluginManager", "builtin", "running");
        manager.locked = true;
        let mut tool = item("my-tool", "my-tool", "failed");
        tool.error = Some("SyntaxError: unexpected token".into());
        tool.refused = vec![PluginRefusal {
            kind: "mcp".into(),
            error: "bad document".into(),
        }];
        let mut goal = item("goal", "default", "off");
        goal.in_use = None;
        vec![item("web", "default", "running"), manager, tool, goal]
    }

    fn shown(panel: &PluginsPanel, width: usize, rows: usize) -> Vec<String> {
        let lines = panel.render(width, rows, &palette());
        for line in &lines {
            assert_eq!(visible_width(line), width, "{line:?}");
        }
        lines.iter().map(|line| strip_escapes(line)).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    /// Each row names the plugin, its state and the candidate in use; the
    /// selected one says why below, at any width the panel is given.
    #[test]
    fn rows_show_state_and_the_selected_reason_at_any_width() {
        let mut panel = PluginsPanel::new();
        panel.set_items(Ok(listed()));
        let text = shown(&panel, 72, 18).join("\n");
        assert!(
            text.contains("web") && text.contains("running  default"),
            "{text}"
        );
        assert!(text.contains("running  builtin  locked"), "{text}");
        assert!(text.contains("goal") && text.contains("off"), "{text}");
        assert!(
            text.contains("Uses default · Candidates: default"),
            "{text}"
        );
        panel.handle_key(key(KeyCode::Down));
        assert!(shown(&panel, 72, 18).join("\n").contains(LOCKED));
        panel.handle_key(key(KeyCode::Down));
        let text = shown(&panel, 72, 18).join("\n");
        assert!(
            text.contains("Error: SyntaxError: unexpected token"),
            "{text}"
        );
        assert!(text.contains("Refused mcp: bad document"), "{text}");
        for width in [12, 24, 40] {
            for rows in [6, 10] {
                let lines = shown(&panel, width, rows);
                assert!(lines.len() <= rows, "{width}x{rows}: {lines:?}");
                if width > 12 {
                    let text = lines.join("\n");
                    assert!(text.contains("my-tool"), "{width}x{rows}: {text}");
                }
            }
        }
    }

    #[test]
    fn loading_empty_and_failed_lists_say_so() {
        let panel = PluginsPanel::new();
        assert!(
            shown(&panel, 40, 10)
                .join("\n")
                .contains("Loading plugins...")
        );
        let mut panel = PluginsPanel::new();
        panel.set_items(Ok(Vec::new()));
        assert!(shown(&panel, 40, 10).join("\n").contains("No plugins."));
        let mut panel = PluginsPanel::new();
        panel.set_items(Err("This daemon cannot manage plugins".into()));
        let text = shown(&panel, 20, 10).join(" ");
        assert!(
            text.contains("cannot manage") && text.contains("plugins"),
            "{text}"
        );
    }

    /// Enter turns an off plugin on, asks before turning one off, and only
    /// explains a locked one; `r` reloads; nothing else is sent while a
    /// change runs.
    #[test]
    fn keys_ask_for_the_change_of_the_selected_plugin() {
        let mut panel = PluginsPanel::new();
        panel.set_items(Ok(listed()));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(shown(&panel, 60, 12).join("\n").contains("Turn off web?"));
        assert!(matches!(panel.handle_key(key(KeyCode::Esc)), Effect::None));
        assert!(panel.confirming.is_none());
        panel.handle_key(key(KeyCode::Enter));
        panel.handle_key(key(KeyCode::Left));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Plugin(action) => assert_eq!(action, PluginAction::Disable("web".into())),
            _ => panic!("expected disable"),
        }
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('r'))),
            Effect::None
        ));
        panel.set_result(
            &PluginAction::Disable("web".into()),
            Ok(PluginReply::default()),
            Ok(listed()),
        );
        assert!(shown(&panel, 60, 12).join("\n").contains("Turned off web."));
        match panel.handle_key(key(KeyCode::Char('r'))) {
            Effect::Plugin(action) => assert_eq!(action, PluginAction::Reload("web".into())),
            _ => panic!("expected reload"),
        }
        panel.set_result(
            &PluginAction::Reload("web".into()),
            Err("boom".into()),
            Ok(listed()),
        );
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::None
        ));
        assert!(panel.confirming.is_none());
        assert!(shown(&panel, 60, 12).join("\n").contains(LOCKED));
        panel.handle_key(key(KeyCode::End));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Plugin(action) => assert_eq!(action, PluginAction::Enable("goal".into())),
            _ => panic!("expected enable"),
        }
    }

    /// A Pending row that waits for a runtime names it and opens `/runtime`
    /// on it; `o` still turns the name on or off there.
    #[test]
    fn a_waiting_row_names_its_runtime_and_opens_it() {
        let mut pending = item("goal", "default", "pending");
        pending.waiting_for = vec!["session-node".into()];
        let mut py = item("py-tool", "default", "pending");
        py.waiting_for = vec!["py".into()];
        let mut panel = PluginsPanel::new();
        panel.set_items(Ok(vec![pending, py]));
        let text = shown(&panel, 80, 12).join("\n");
        assert!(
            text.contains("pending · waits for the node runtime"),
            "{text}"
        );
        assert!(text.contains("Enter open /runtime"), "{text}");
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::OpenRuntime(runtime) => assert_eq!(runtime, "node"),
            other => panic!("{other:?}"),
        }
        match panel.handle_key(key(KeyCode::Char('o'))) {
            Effect::None => {}
            other => panic!("{other:?}"),
        }
        assert!(
            shown(&panel, 80, 12).join("\n").contains("Turn off goal?"),
            "{text}"
        );
        panel.handle_key(key(KeyCode::Esc));
        panel.handle_key(key(KeyCode::Down));
        let text = shown(&panel, 80, 12).join("\n");
        assert!(text.contains("waits for the python runtime"), "{text}");
    }

    /// A failed change shows the method's message; an install names what
    /// it installed, says when a restart is needed and selects it.
    #[test]
    fn outcomes_show_errors_installs_and_restarts() {
        let mut panel = PluginsPanel::new();
        panel.set_items(Ok(listed()));
        panel.set_result(
            &PluginAction::Enable("goal".into()),
            Err("no plugin \"goal\" (RPC -32000, plugin_not_found)".into()),
            Ok(listed()),
        );
        let text = shown(&panel, 80, 14).join("\n");
        assert!(text.contains("plugin_not_found"), "{text}");
        let mut panel = PluginsPanel::installing(std::path::Path::new("/p/my-tool"));
        assert!(
            shown(&panel, 80, 14)
                .join("\n")
                .contains("Installing /p/my-tool...")
        );
        let reply = PluginReply {
            id: Some("my-tool".into()),
            name: Some("my-tool".into()),
            enabled: Some(true),
            restart_required: true,
            ..PluginReply::default()
        };
        panel.set_result(
            &PluginAction::Install("/p/my-tool".into()),
            Ok(reply),
            Ok(listed()),
        );
        let text = shown(&panel, 80, 16).join(" ");
        assert!(text.contains("Installed my-tool (my-tool)."), "{text}");
        assert!(text.contains("Restart required"), "{text}");
        assert!(
            text.contains("SyntaxError"),
            "the installed plugin is selected: {text}"
        );
    }
}
