//! The `/runtime` panel: the two plugin runtimes RuntimeManager supplies
//! (`runtime.list`) with the source, `auto` and version in effect, what each
//! is doing and why, and below them the plugins that wait for it
//! (`plugin.list`).
//!
//! ```text
//! ╭─ Plugin runtimes ─────────────────────────────────╮
//! │ › node      download  auto on   24.21.0   ready   │
//! │   python    system    auto on   3.12.15   off     │
//! │                                                   │
//! │   4 plugins wait for node: goal, session-title, … │
//! ╰─ Enter install or apply · s source · a auto ──────╯
//! ```

use std::time::{Duration, Instant};

use crate::model::session::{PluginItem, RuntimeAction, RuntimeItem, RuntimesError};
use crate::ui::i18n::{t, tf};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{bordered, key_hints, wrap_words};
use crate::ui::rows::{Columns, Kind, Row, panel_row, title_plain};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::truncate_cells;
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

const HINT: &str = "Enter install or apply · s source · a auto · r retry · Esc close";
/// The same keys for a daemon that does not report `auto`, which leaves `a`
/// nothing to toggle.
const HINT_NO_AUTO: &str = "Enter install or apply · s source · r retry · Esc close";
const ACQUIRING_HINT: &str = "Esc close · the acquisition keeps running if you close this";
const FAILED_HINT: &str = "Enter retry · s switch {name} to {source} · Esc close";
/// How often the rows are read again while one is starting.
const POLL: Duration = Duration::from_secs(1);
/// The row names PluginManager's rows wait for, as `plugin.list` reports
/// them (RuntimeManager §5).
const RUNTIME_ROWS: [(&str, &str); 2] = [("session-node", "node"), ("py", "python")];

/// Every English key this panel draws through [`t`]; each locale translates
/// them all.
#[cfg(test)]
pub const STRINGS: &[&str] = &[
    "Plugin runtimes",
    HINT,
    HINT_NO_AUTO,
    ACQUIRING_HINT,
    FAILED_HINT,
    "Loading plugin runtimes...",
    "acquiring",
    "starting",
    "down",
    "failed",
    "ready",
    "off",
    "auto on",
    "auto off",
    "1 plugin waits for {runtime}: {names}",
    "{count} plugins wait for {runtime}: {names}",
    "show, install or configure the plugin runtimes",
    "This daemon does not serve plugin runtimes ({version})",
    "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]",
];

/// The runtime the plugin row waits for, from the rows `plugin.list` names.
pub fn waited_runtime(item: &PluginItem) -> Option<&'static str> {
    item.waiting_for.iter().find_map(|wait| runtime_row(wait))
}

/// The runtime whose row `wait` names, from the rows `plugin.list` reports.
pub fn runtime_row(wait: &str) -> Option<&'static str> {
    RUNTIME_ROWS
        .iter()
        .find(|(row, _)| *row == wait)
        .map(|(_, runtime)| *runtime)
}

pub struct RuntimePanel {
    items: Vec<RuntimeItem>,
    /// The plugins `plugin.list` reports; those Pending on a runtime are
    /// named below the rows.
    plugins: Vec<PluginItem>,
    selected: usize,
    /// The runtime a panel opened by `/plugins` selects once the rows
    /// arrive.
    choosing: Option<String>,
    loading: bool,
    error: Option<String>,
    /// A change is in flight: keys other than moving and closing wait.
    busy: bool,
    /// The last change's error.
    message: Option<String>,
    /// When this panel saw a row enter `acquiring`, so the elapsed seconds
    /// start from a moment it saw; a row already acquiring when the panel
    /// opened has none.
    since: Vec<(String, Instant)>,
    /// The last re-read while a runtime was starting.
    polled: Instant,
}

impl RuntimePanel {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            plugins: Vec::new(),
            selected: 0,
            choosing: None,
            loading: true,
            error: None,
            busy: false,
            message: None,
            since: Vec::new(),
            polled: Instant::now(),
        }
    }

    /// A panel opened on the runtime `name`.
    pub fn on(name: &str) -> Self {
        let mut panel = Self::new();
        panel.choosing = Some(name.to_owned());
        panel
    }

    /// The rows `runtime.list` and `plugin.list` returned, and what the
    /// change that asked for them came to.
    pub fn set(
        &mut self,
        result: Option<Result<(), String>>,
        runtimes: Result<Vec<RuntimeItem>, RuntimesError>,
        plugins: Result<Vec<PluginItem>, String>,
    ) {
        self.loading = false;
        if let Some(result) = result {
            self.busy = false;
            self.message = result.err();
        }
        if let Ok(plugins) = plugins {
            self.plugins = plugins;
        }
        match runtimes {
            Ok(items) => {
                self.observe(&items);
                self.items = items;
                self.error = None;
            }
            Err(RuntimesError::Unsupported { version }) => {
                self.error = Some(tf(
                    "This daemon does not serve plugin runtimes ({version})",
                    &[("version", &version)],
                ));
            }
            Err(RuntimesError::Failed(error)) => self.error = Some(error),
        }
        if let Some(name) = self.choosing.take()
            && let Some(at) = self.items.iter().position(|item| item.name == name)
        {
            self.selected = at;
        }
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    /// Records when a row enters `acquiring`; a row already acquiring when
    /// the panel opened claims no start.
    fn observe(&mut self, items: &[RuntimeItem]) {
        let now = Instant::now();
        for item in items.iter().filter(|item| item.status == "acquiring") {
            match self
                .items
                .iter()
                .find(|shown| shown.name == item.name)
                .map(|shown| shown.status.as_str())
            {
                Some("acquiring") => {}
                Some(_) => {
                    self.since.retain(|(name, _)| name != &item.name);
                    self.since.push((item.name.clone(), now));
                }
                None => {}
            }
        }
        self.since.retain(|(name, _)| {
            items
                .iter()
                .any(|item| item.name == *name && item.status == "acquiring")
        });
    }

    /// Whether the rows must be read again: while one is acquiring or
    /// starting, once a second, since the daemon reports no change by
    /// itself.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self
            .items
            .iter()
            .any(|item| item.status == "acquiring" || item.status == "starting")
        {
            return false;
        }
        if now.saturating_duration_since(self.polled) < POLL {
            return false;
        }
        self.polled = now;
        true
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let plain = key.mods.0 & !Modifiers::SHIFT.0 == 0;
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
            KeyCode::Enter | KeyCode::Char('r') if plain => self.change(
                self.items
                    .get(self.selected)
                    .map(|item| RuntimeAction::Install(item.name.clone())),
            ),
            KeyCode::Char('s') if plain => {
                self.change(
                    self.items
                        .get(self.selected)
                        .map(|item| RuntimeAction::Configure {
                            name: item.name.clone(),
                            source: Some(item.source.flipped()),
                            auto: None,
                        }),
                )
            }
            KeyCode::Char('a') if plain => self.change(self.auto()),
            _ => Effect::None,
        }
    }

    /// The toggle `a` asks for: the `auto` the rows report, which a daemon
    /// that does not report it leaves unknown.
    fn auto(&self) -> Option<RuntimeAction> {
        let item = self.items.get(self.selected)?;
        Some(RuntimeAction::Configure {
            name: item.name.clone(),
            source: None,
            auto: Some(!item.auto?),
        })
    }

    fn change(&mut self, action: Option<RuntimeAction>) -> Effect {
        match action {
            Some(action) => {
                self.busy = true;
                self.message = None;
                Effect::Runtime(action)
            }
            None => Effect::None,
        }
    }

    /// `width` columns including borders; `rows` is the panel's row budget.
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let mut content = Vec::new();
        let mut footer = Vec::new();
        if self.items.is_empty() {
            let (text, color) = match (&self.error, self.loading) {
                (Some(error), _) => (error.clone(), &p.danger),
                (_, true) => (t("Loading plugin runtimes...").to_owned(), &p.muted),
                _ => (String::new(), &p.muted),
            };
            for row in wrap_words(&text, inner) {
                content.push(fg(&row, color));
            }
        } else {
            let list: Vec<Row> = self.items.iter().map(|item| self.row(item)).collect();
            let columns = Columns::of(&list, inner);
            let detail = columns.detail();
            for (index, item) in self.items.iter().enumerate() {
                content.push(panel_row(
                    &list[index],
                    index == self.selected,
                    inner,
                    columns,
                    p,
                ));
                for line in failure(item) {
                    let room = inner.saturating_sub(detail).max(1);
                    for row in wrap_words(&line, room) {
                        content.push(format!("{}{}", " ".repeat(detail), fg(&row, &p.danger)));
                    }
                }
            }
            let waits = self.waits();
            if !waits.is_empty() {
                footer.push(String::new());
                for wait in waits {
                    for row in wrap_words(&wait, inner.saturating_sub(2).max(1)) {
                        footer.push(format!("  {}", fg(&row, &p.muted)));
                    }
                }
            }
        }
        if self.busy && self.message.is_none() {
            footer.push(fg(&truncate_cells(t("Working..."), inner), &p.muted));
        }
        if let Some(message) = &self.message {
            for row in wrap_words(message, inner) {
                footer.push(fg(&row, &p.danger));
            }
        }
        let budget = rows.saturating_sub(2);
        footer.truncate(budget);
        content.truncate(budget.saturating_sub(footer.len()));
        content.extend(footer);
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(&title_plain(t("Plugin runtimes"), inner, p)),
            Some(&key_hints(&self.hint(), p)),
        )
    }

    /// The keys the selected row offers: a failed one retries and names the
    /// source `s` switches to; an acquiring one says the work goes on.
    fn hint(&self) -> String {
        match self.items.get(self.selected) {
            Some(item) if item.status == "acquiring" => t(ACQUIRING_HINT).to_owned(),
            Some(item) if item.status == "failed" => tf(
                FAILED_HINT,
                &[
                    ("name", &item.name),
                    ("source", item.source.flipped().as_str()),
                ],
            ),
            Some(item) if item.auto.is_some() => t(HINT).to_owned(),
            _ => t(HINT_NO_AUTO).to_owned(),
        }
    }

    /// The plugins Pending on a runtime, one line per runtime.
    fn waits(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for item in &self.items {
            let names: Vec<&str> = self
                .plugins
                .iter()
                .filter(|plugin| plugin.status.as_deref() == Some("pending"))
                .filter(|plugin| waited_runtime(plugin) == Some(item.name.as_str()))
                .map(|plugin| plugin.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            let names = names.join(", ");
            lines.push(match names.split(", ").count() {
                1 => tf(
                    "1 plugin waits for {runtime}: {names}",
                    &[("runtime", &item.name), ("names", &names)],
                ),
                count => tf(
                    "{count} plugins wait for {runtime}: {names}",
                    &[
                        ("count", &count.to_string()),
                        ("runtime", &item.name),
                        ("names", &names),
                    ],
                ),
            });
        }
        lines
    }

    /// A runtime's row: its source, the version in effect and what it is
    /// doing, with how long an acquisition this panel saw has run.
    fn row(&self, item: &RuntimeItem) -> Row {
        let word = match item.status.as_str() {
            "off" => t("off"),
            "acquiring" => t("acquiring"),
            "starting" => t("starting"),
            "ready" => t("ready"),
            "down" => t("down"),
            "failed" => t("failed"),
            other => other,
        };
        let status = match self.since.iter().find(|(name, _)| name == &item.name) {
            Some((_, since)) => {
                format!(
                    "{word}  {}s",
                    Instant::now().duration_since(*since).as_secs()
                )
            }
            None => word.to_owned(),
        };
        let auto = match item.auto {
            Some(true) => t("auto on"),
            Some(false) => t("auto off"),
            None => "",
        };
        let mut detail = format!(
            "{:<10}{:<10}{:<10}{status}",
            item.source.as_str(),
            auto,
            item.version.as_deref().unwrap_or_default()
        );
        if let Some(reason) = item.reason.as_deref().filter(|_| item.status == "down") {
            detail.push_str("  ");
            detail.push_str(reason);
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
}

/// One line per source: the reason a failed acquisition reports, which lists
/// every source and its error.
fn failure(item: &RuntimeItem) -> Vec<String> {
    if item.status != "failed" {
        return Vec::new();
    }
    item.reason
        .as_deref()
        .map(|reason| reason.split("; ").map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::RuntimeSource;
    use crate::ui::render::panel::{strip_escapes, visible_width};
    use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};

    fn palette() -> Palette {
        terminal_palette(ColorMode::TrueColor, Appearance::Dark)
    }

    fn item(name: &str, source: RuntimeSource, status: &str) -> RuntimeItem {
        RuntimeItem {
            name: name.into(),
            source,
            auto: Some(true),
            version: Some(if source == RuntimeSource::Download {
                "24.21.0".into()
            } else {
                "3.12.15".into()
            }),
            status: status.into(),
            reason: None,
        }
    }

    fn listed(status: &str) -> Vec<RuntimeItem> {
        vec![
            item("node", RuntimeSource::Download, status),
            item("python", RuntimeSource::System, status),
        ]
    }

    fn waiting(name: &str, wait: &str) -> PluginItem {
        PluginItem {
            name: name.into(),
            in_use: None,
            off: false,
            locked: false,
            candidates: Vec::new(),
            status: Some("pending".into()),
            error: None,
            waiting_for: vec![wait.into()],
            refused: Vec::new(),
        }
    }

    fn shown(panel: &RuntimePanel, width: usize, rows: usize) -> Vec<String> {
        let lines = panel.render(width, rows, &palette());
        for line in &lines {
            assert_eq!(visible_width(line), width, "{line:?}");
        }
        assert!(lines.len() <= rows, "{lines:?}");
        lines.iter().map(|line| strip_escapes(line)).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    /// Each row names its source, whether it is turned on at startup, the
    /// version in effect and what it is doing, and `down` says why; the
    /// panel fits any width and height it is given.
    #[test]
    fn rows_show_the_source_auto_version_and_status_at_any_width() {
        let mut panel = RuntimePanel::new();
        let mut python = item("python", RuntimeSource::System, "down");
        python.reason = Some("exited with exit status: 1".into());
        panel.set(
            None,
            Ok(vec![item("node", RuntimeSource::Download, "ready"), python]),
            Ok(Vec::new()),
        );
        let text = shown(&panel, 80, 12).join("\n");
        assert!(
            text.contains("download") && text.contains("24.21.0"),
            "{text}"
        );
        assert!(text.contains("ready"), "{text}");
        assert!(text.contains("auto on"), "{text}");
        assert!(
            text.contains("system") && text.contains("3.12.15"),
            "{text}"
        );
        assert!(text.contains("down  exited with exit status: 1"), "{text}");
        for width in [12, 24, 40] {
            for rows in [6, 10] {
                let text = shown(&panel, width, rows).join("\n");
                if width > 12 {
                    assert!(text.contains("node"), "{width}x{rows}: {text}");
                }
            }
        }
        let mut panel = RuntimePanel::new();
        let mut node = item("node", RuntimeSource::Download, "ready");
        node.auto = None;
        panel.set(None, Ok(vec![node]), Ok(Vec::new()));
        let text = shown(&panel, 80, 10).join("\n");
        assert!(!text.contains("auto on"), "{text}");
        assert!(!text.contains("auto off"), "{text}");
        assert!(
            !text.contains("a auto"),
            "no key for an unknown auto: {text}"
        );
    }

    /// Below the rows, the plugins Pending on a runtime are named, one line
    /// per runtime, singular when there is one.
    #[test]
    fn the_plugins_that_wait_for_a_runtime_are_named_below_the_rows() {
        let mut panel = RuntimePanel::new();
        panel.set(
            None,
            Ok(listed("off")),
            Ok(vec![
                waiting("goal", "session-node"),
                waiting("work", "session-node"),
                waiting("py-tool", "py"),
                waiting("other", "tools"),
            ]),
        );
        let text = shown(&panel, 90, 14).join("\n");
        assert!(
            text.contains("2 plugins wait for node: goal, work"),
            "{text}"
        );
        assert!(
            text.contains("1 plugin waits for python: py-tool"),
            "{text}"
        );
        assert!(!text.contains("other"), "{text}");
    }

    /// Enter applies the configuration as it stands, `s` switches the source
    /// and `a` toggles the `auto` the row reports; a panel opened by
    /// `/plugins` selects the runtime it names.
    #[test]
    fn keys_apply_switch_and_toggle_the_selected_runtime() {
        let mut panel = RuntimePanel::on("python");
        panel.set(None, Ok(listed("off")), Ok(Vec::new()));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::Runtime(action) => assert_eq!(action, RuntimeAction::Install("python".into())),
            other => panic!("{other:?}"),
        }
        panel.set(Some(Ok(())), Ok(listed("acquiring")), Ok(Vec::new()));
        match panel.handle_key(key(KeyCode::Char('s'))) {
            Effect::Runtime(action) => assert_eq!(
                action,
                RuntimeAction::Configure {
                    name: "python".into(),
                    source: Some(RuntimeSource::Download),
                    auto: None,
                }
            ),
            other => panic!("{other:?}"),
        }
        panel.set(
            Some(Err("runtime_configuration: no write".into())),
            Ok(listed("off")),
            Ok(Vec::new()),
        );
        assert!(
            shown(&panel, 80, 12)
                .join("\n")
                .contains("runtime_configuration"),
            "the failed change is shown"
        );
        panel.handle_key(key(KeyCode::Home));
        match panel.handle_key(key(KeyCode::Char('a'))) {
            Effect::Runtime(action) => assert_eq!(
                action,
                RuntimeAction::Configure {
                    name: "node".into(),
                    source: None,
                    auto: Some(false),
                }
            ),
            other => panic!("{other:?}"),
        }
        panel.set(Some(Ok(())), Ok(listed("off")), Ok(Vec::new()));
        let mut unknown = item("node", RuntimeSource::Download, "off");
        unknown.auto = None;
        panel.set(None, Ok(vec![unknown]), Ok(Vec::new()));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('a'))),
            Effect::None
        ));
        assert!(matches!(panel.handle_key(key(KeyCode::Esc)), Effect::Close));
    }

    /// `acquiring` counts the seconds from the moment this panel saw the
    /// row start, and the rows are read again while it runs.
    #[test]
    fn an_acquiring_row_counts_from_the_start_the_panel_saw() {
        let mut panel = RuntimePanel::new();
        panel.set(None, Ok(listed("acquiring")), Ok(Vec::new()));
        let text = shown(&panel, 80, 12).join("\n");
        assert!(text.contains("acquiring"), "{text}");
        assert!(panel.since.is_empty(), "no start was seen: {text}");
        panel.set(None, Ok(listed("off")), Ok(Vec::new()));
        panel.set(None, Ok(listed("acquiring")), Ok(Vec::new()));
        assert!(
            shown(&panel, 80, 12).join("\n").contains("acquiring  0s"),
            "the start this panel saw is counted"
        );
        assert!(panel.tick(Instant::now() + POLL), "the rows are read again");
        panel.set(None, Ok(listed("ready")), Ok(Vec::new()));
        assert!(!panel.tick(Instant::now() + POLL), "nothing is starting");
    }

    /// A failed acquisition prints one line per source and offers the other
    /// source; `r` retries as `Enter` does.
    #[test]
    fn a_failed_row_lists_each_source_and_offers_the_other_one() {
        let mut panel = RuntimePanel::new();
        let mut node = item("node", RuntimeSource::Download, "failed");
        node.reason = Some(
            "node.tar.xz not acquired: https://nodejs.org/x: timed out; https://npmmirror.com/x: 404"
                .into(),
        );
        panel.set(None, Ok(vec![node]), Ok(Vec::new()));
        let text = shown(&panel, 100, 12).join("\n");
        assert!(text.contains("https://nodejs.org/x: timed out"), "{text}");
        assert!(text.contains("https://npmmirror.com/x: 404"), "{text}");
        assert!(text.contains("s switch node to system"), "{text}");
        match panel.handle_key(key(KeyCode::Char('r'))) {
            Effect::Runtime(action) => assert_eq!(action, RuntimeAction::Install("node".into())),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(panel.handle_key(key(KeyCode::Enter)), Effect::None),
            "a change is in flight"
        );
    }

    /// A daemon that does not serve `runtime.list` says so and names its
    /// version instead of showing an empty list.
    #[test]
    fn a_daemon_without_runtimes_says_so() {
        let mut panel = RuntimePanel::new();
        panel.set(
            None,
            Err(RuntimesError::Unsupported {
                version: "0.42.0".into(),
            }),
            Ok(Vec::new()),
        );
        let text = shown(&panel, 80, 10).join(" ");
        assert!(text.contains("does not serve plugin runtimes"), "{text}");
        assert!(text.contains("0.42.0"), "{text}");
        assert!(!text.contains("Loading"), "{text}");
    }
}
