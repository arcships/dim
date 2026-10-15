//! The `/` command palette: filter, selection model and the bordered list
//! panel it is drawn in (ports of `filterScrollbackCommands`,
//! `createListPanelModel` and `ListPanel`).

use super::commands::{COMMANDS, Command, Context};
use crate::ui::i18n::t;
use crate::ui::render::panel::{bordered, follow_window};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::transcript::blocks::{display_width, truncate_cells};

const IDLE_MARKER: &str = "  ";
const ROW_INDENT: usize = 2;
const DETAIL_GAP: usize = 2;
/// Rows visible in the inline palette (headings included).
pub const PAGE_SIZE: usize = 6;
/// Chrome around the rows: top border, title, footer, bottom border.
pub const CHROME_ROWS: usize = 4;
const HINT: &str = "type filter · enter select · esc clear/close";
const ESC_CLOSE: &str = "esc close";

/// Listed commands matching `query`: name/alias prefixes first, then
/// contains matches on names, aliases or the description.
pub fn filter_commands(query: &str, context: Context) -> Vec<&'static Command> {
    let normalized = query.trim().to_lowercase();
    let normalized = normalized
        .strip_prefix('/')
        .map(str::trim_start)
        .unwrap_or(&normalized)
        .to_owned();
    let listed = COMMANDS.iter().filter(|c| c.listed(context));
    if normalized.is_empty() {
        return listed.collect();
    }
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for command in listed {
        let names = std::iter::once(command.name).chain(command.aliases.iter().copied());
        let mut starts = false;
        let mut has = false;
        for name in names {
            if name.starts_with(&normalized) {
                starts = true;
            } else if name.contains(&normalized) {
                has = true;
            }
        }
        if starts {
            prefix.push(command);
        } else if has
            || command.description.to_lowercase().contains(&normalized)
            || t(command.description).to_lowercase().contains(&normalized)
        {
            contains.push(command);
        }
    }
    prefix.extend(contains);
    prefix
}

/// A skill offered next to the commands (`$skill: name`).
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub display_name: Option<String>,
    pub display_description: Option<String>,
}

impl Skill {
    fn detail(&self) -> String {
        let description = self
            .display_description
            .clone()
            .unwrap_or_else(|| self.description.clone());
        match &self.display_name {
            None => description,
            Some(name) if description.is_empty() => name.clone(),
            Some(name) => format!("{name} · {description}"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    Heading,
    /// A row of a plain list panel (sessions): the id it stands for.
    Item(String),
    Command(&'static Command),
    Skill(String),
}

/// One palette row as Amp lays it out: category, label (bold), detail,
/// and the key that runs it at the right edge.
#[derive(Debug, Clone)]
pub struct Row {
    pub kind: Kind,
    /// Right-aligned in a column of its own; empty in plain lists.
    pub category: String,
    pub label: String,
    pub detail: String,
    pub key: Option<String>,
    /// Shown faint and marked `(unavailable)`; still runs, and the app says
    /// why it cannot.
    pub unavailable: bool,
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, Kind::Heading)
    }
}

fn filter_skills<'a>(skills: &'a [Skill], query: &str) -> Vec<&'a Skill> {
    let normalized = query.trim().to_lowercase();
    let normalized = normalized
        .strip_prefix('$')
        .map(str::trim_start)
        .unwrap_or(&normalized)
        .to_owned();
    if normalized.is_empty() {
        return skills.iter().collect();
    }
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for skill in skills {
        let name = skill.name.to_lowercase();
        let display = skill.display_name.as_deref().unwrap_or("").to_lowercase();
        let description = skill
            .display_description
            .as_deref()
            .unwrap_or(&skill.description)
            .to_lowercase();
        if name.starts_with(&normalized) || display.starts_with(&normalized) {
            prefix.push(skill);
        } else if name.contains(&normalized)
            || display.contains(&normalized)
            || description.contains(&normalized)
        {
            contains.push(skill);
        }
    }
    prefix.extend(contains);
    prefix
}

/// Palette rows for `query`: commands and skills share the list, told apart
/// by their category; skills lead when a skill name matches and no command
/// name does. A slash command reads `/name` and its description; an action
/// reached by a key reads as its description.
pub fn rows_for(query: &str, context: Context, skills: &[Skill]) -> Vec<Row> {
    let commands = filter_commands(query, context);
    let matched_skills = filter_skills(skills, query);
    let command_rows: Vec<Row> = commands
        .iter()
        .map(|command| {
            let description = t(command.description);
            let (label, detail) = if command.slash {
                (format!("/{}", command.name), description.to_owned())
            } else {
                (description.to_owned(), String::new())
            };
            Row {
                kind: Kind::Command(command),
                category: t(command.category).to_owned(),
                label,
                detail,
                key: command.key.map(str::to_owned),
                unavailable: !command.available(context),
            }
        })
        .collect();
    let skill_rows: Vec<Row> = matched_skills
        .iter()
        .map(|skill| Row {
            kind: Kind::Skill(skill.name.clone()),
            category: t("skill").to_owned(),
            label: format!("${}", skill.name),
            detail: skill.detail(),
            key: None,
            unavailable: false,
        })
        .collect();
    if skill_rows.is_empty() {
        return command_rows;
    }
    if command_rows.is_empty() {
        return skill_rows;
    }
    let normalized = query
        .trim()
        .to_lowercase()
        .trim_start_matches('$')
        .trim_start_matches('/')
        .to_owned();
    let skills_first = !normalized.is_empty()
        && matched_skills.iter().any(|s| {
            s.name.to_lowercase().starts_with(&normalized)
                || s.display_name
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().starts_with(&normalized))
        })
        && !commands.iter().any(|c| {
            std::iter::once(c.name)
                .chain(c.aliases.iter().copied())
                .any(|n| n.to_lowercase().starts_with(&normalized))
        });
    let (mut first, second) = if skills_first {
        (skill_rows, command_rows)
    } else {
        (command_rows, skill_rows)
    };
    first.extend(second);
    first
}

/// Selection and window over the rows of one query.
#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub query: String,
    /// Index into the rows (never a heading).
    pub selected: usize,
    /// First row of the visible window, kept between frames.
    window: std::cell::Cell<usize>,
}

impl PaletteState {
    pub fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.query = query.to_owned();
            self.selected = 0;
            self.window.set(0);
        }
    }

    /// The nearest selectable row from `index`, searching `direction` first.
    fn selectable(rows: &[Row], index: usize, direction: isize) -> usize {
        if rows.is_empty() {
            return 0;
        }
        let clamped = index.min(rows.len() - 1) as isize;
        for dir in [direction, -direction] {
            let mut at = clamped;
            while at >= 0 && (at as usize) < rows.len() {
                if rows[at as usize].selectable() {
                    return at as usize;
                }
                at += dir;
            }
        }
        clamped as usize
    }

    pub fn current(&self, rows: &[Row]) -> usize {
        Self::selectable(rows, self.selected, 1)
    }

    pub fn step(&mut self, delta: isize, rows: &[Row]) {
        if rows.is_empty() {
            self.selected = 0;
            return;
        }
        let from = Self::selectable(rows, self.selected, 1) as isize;
        let target = (from + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = Self::selectable(rows, target, if delta < 0 { -1 } else { 1 });
    }

    /// The first visible row of a `page`-row window; it moves only as far
    /// as the selection needs.
    pub fn window_start(&self, rows: &[Row], page: usize) -> usize {
        let start = follow_window(self.window.get(), self.current(rows), page, rows.len());
        self.window.set(start);
        start
    }
}

/// The panel title: "Suggestions" only when both kinds are listed.
fn panel_title(rows: &[Row]) -> &'static str {
    let skills = rows.iter().any(|r| matches!(r.kind, Kind::Skill(_)));
    let commands = rows.iter().any(|r| matches!(r.kind, Kind::Command(_)));
    match (skills, commands) {
        (true, true) => "Suggestions",
        (true, false) => "Skills",
        _ => "Commands",
    }
}

/// The bordered panel for the current query, `width` columns wide including
/// borders, fixed at `PAGE_SIZE + CHROME_ROWS` rows.
pub fn render(state: &PaletteState, rows: &[Row], width: usize, p: &Palette) -> Vec<String> {
    let inner = width.saturating_sub(4).max(1);
    let mut content: Vec<String> = Vec::new();
    content.push(title(t(panel_title(rows)), inner, p));
    content.extend(list(state, rows, inner, PAGE_SIZE, p));
    content.push(footer(&position(state, rows), t(HINT), inner, p));
    bordered(
        &content,
        width,
        Some(PAGE_SIZE + CHROME_ROWS),
        &p.border,
        None,
    )
}

/// Rows above the list in the fullscreen palette: a gap, the query row and
/// a gap, as Amp spaces it.
pub const OVERLAY_QUERY_ROWS: usize = 3;

/// The fullscreen palette (Amp's command palette): a box titled
/// `Command Palette` with its own `> query` row over `page` rows of
/// commands. The query row is the box's third row.
pub fn render_overlay(
    state: &PaletteState,
    rows: &[Row],
    width: usize,
    page: usize,
    p: &Palette,
) -> Vec<String> {
    let inner = width.saturating_sub(4).max(1);
    let mut content = vec![
        String::new(),
        format!(
            "> {}",
            truncate_cells(&state.query, inner.saturating_sub(2))
        ),
        String::new(),
    ];
    content.extend(list(state, rows, inner, page, p));
    // The footer stays at the bottom however few commands match.
    content.resize(OVERLAY_QUERY_ROWS + page, String::new());
    content.push(footer(&position(state, rows), t(HINT), inner, p));
    let title = paint(
        t("Command Palette"),
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    );
    bordered(&content, width, None, &p.border, Some(&title))
}

/// The visible window of `rows`, `page` rows at most.
fn list(state: &PaletteState, rows: &[Row], inner: usize, page: usize, p: &Palette) -> Vec<String> {
    if rows.is_empty() {
        return vec![fg(&truncate_cells(t("No matches"), inner), &p.muted)];
    }
    let columns = Columns::of(rows, inner);
    let start = state.window_start(rows, page);
    let current = state.current(rows);
    rows.iter()
        .skip(start)
        .take(page)
        .enumerate()
        .map(|(offset, row)| panel_row(row, start + offset == current, inner, columns, p))
        .collect()
}

/// `place / total` over the selectable rows.
fn position(state: &PaletteState, rows: &[Row]) -> String {
    let selectable_total = rows.iter().filter(|r| r.selectable()).count();
    if selectable_total == 0 {
        return "0 / 0".to_owned();
    }
    let at = state.current(rows);
    let place = rows[..=at.min(rows.len() - 1)]
        .iter()
        .filter(|r| r.selectable())
        .count();
    format!("{place} / {selectable_total}")
}

/// A panel title without the `esc close` hint (forms say how to go back in
/// their own hint row).
pub(crate) fn title_plain(text: &str, inner: usize, p: &Palette) -> String {
    paint(
        &truncate_cells(text, inner),
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    )
}

pub(crate) fn title(text: &str, inner: usize, p: &Palette) -> String {
    let close = t(ESC_CLOSE);
    let head = truncate_cells(text, inner.saturating_sub(display_width(close) + 2).max(1));
    let styled = paint(
        &head,
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    );
    let gap = inner.saturating_sub(display_width(&head) + display_width(close));
    if gap < 2 {
        return styled;
    }
    format!("{styled}{}{}", " ".repeat(gap), fg(close, &p.faint))
}

/// Column widths shared by every row, not just the visible window, so
/// nothing shifts while scrolling.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Columns {
    /// The right-aligned category column; 0 in plain lists.
    category: usize,
    /// Where details start, counted from the label.
    label: usize,
}

impl Columns {
    pub(crate) fn of(rows: &[Row], inner: usize) -> Self {
        let widest = |f: &dyn Fn(&Row) -> Option<usize>| {
            rows.iter()
                .filter(|r| r.selectable())
                .filter_map(f)
                .max()
                .unwrap_or(0)
        };
        let category = widest(&|r| Some(display_width(&r.category)));
        // Only labels with a detail after them set the detail column: an
        // action named by its description runs on into the free space.
        let label = widest(&|r| (!r.detail.is_empty()).then(|| display_width(&r.label)));
        let room = inner.saturating_sub(ROW_INDENT + category + DETAIL_GAP);
        Self {
            category,
            label: (label + DETAIL_GAP).min((room / 2).max(1)),
        }
    }
}

const UNAVAILABLE: &str = " (unavailable)";

/// One row in Amp's columns: category (faint, right-aligned), label (bold),
/// detail (faint), key (keybind colour, at the right edge). An unavailable
/// row is faint throughout; the selected row sits on an accent bar.
pub(crate) fn panel_row(
    row: &Row,
    selected: bool,
    inner: usize,
    columns: Columns,
    p: &Palette,
) -> String {
    if !row.selectable() {
        // A group header sits left of the rows it introduces.
        return paint(
            &truncate_cells(&row.label, inner),
            Paint {
                fg: Some(&p.muted),
                bold: true,
                ..Paint::default()
            },
        );
    }
    let category = if columns.category == 0 {
        String::new()
    } else {
        let text = truncate_cells(&row.category, columns.category);
        format!(
            "{}{text}{}",
            " ".repeat(columns.category - display_width(&text)),
            " ".repeat(DETAIL_GAP)
        )
    };
    let key = row
        .key
        .as_deref()
        .map(|key| format!("{}{key}", " ".repeat(DETAIL_GAP)))
        .unwrap_or_default();
    let room = inner
        .saturating_sub(ROW_INDENT + display_width(&category) + display_width(&key))
        .max(1);
    let mut label = row.label.clone();
    let mut detail = row.detail.clone();
    if row.unavailable {
        if detail.is_empty() {
            label.push_str(t(UNAVAILABLE));
        } else {
            detail.push_str(t(UNAVAILABLE));
        }
    }
    let label = if detail.is_empty() {
        truncate_cells(&label, room)
    } else {
        truncate_cells(
            &label,
            room.min(columns.label.saturating_sub(DETAIL_GAP).max(1)),
        )
    };
    let detail = if detail.is_empty() {
        String::new()
    } else {
        let gap = columns
            .label
            .saturating_sub(display_width(&label))
            .max(DETAIL_GAP);
        truncate_cells(
            &format!("{}{detail}", " ".repeat(gap)),
            room.saturating_sub(display_width(&label)),
        )
    };
    let used = ROW_INDENT
        + display_width(&category)
        + display_width(&label)
        + display_width(&detail)
        + display_width(&key);
    let fill = " ".repeat(inner.saturating_sub(used));
    let part = |text: &str, fg: &str, bold: bool| {
        paint(
            text,
            Paint {
                fg: Some(fg),
                bg: selected.then_some(p.accent.as_str()),
                bold,
                ..Paint::default()
            },
        )
    };
    let (text, faint, keyed) = if selected {
        (&p.on_accent, &p.on_accent, &p.keybind)
    } else if row.unavailable {
        (&p.muted, &p.muted, &p.muted)
    } else {
        (&p.text, &p.muted, &p.keybind)
    };
    format!(
        "{}{}{}{}{}",
        part(IDLE_MARKER, text, false),
        part(&category, faint, false),
        part(&label, text, true),
        part(&format!("{detail}{fill}"), faint, false),
        part(&key, keyed, true),
    )
}

pub(crate) fn footer(count: &str, hint: &str, inner: usize, p: &Palette) -> String {
    let count = truncate_cells(count, inner);
    let rest = truncate_cells(
        &format!(" · {hint}"),
        inner.saturating_sub(display_width(&count)),
    );
    let mut out = fg(&count, &p.faint);
    if !rest.is_empty() {
        out.push_str(&fg(&rest, &p.muted));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};

    const SCROLLBACK: Context = Context {
        session: true,
        running: false,
        queued: false,
        fullscreen: false,
    };
    const FULLSCREEN: Context = Context {
        fullscreen: true,
        ..SCROLLBACK
    };

    fn names(query: &str, context: Context) -> Vec<&'static str> {
        filter_commands(query, context)
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn the_registry_lists_by_category_and_hides_fullscreen_actions_elsewhere() {
        let all = names("/", FULLSCREEN);
        assert_eq!(&all[..2], ["new", "resume"]);
        assert_eq!(all.len(), COMMANDS.len());
        let scrollback = names("/", SCROLLBACK);
        assert!(!scrollback.contains(&"scroll-top"));
        assert!(scrollback.contains(&"interrupt"));
    }

    #[test]
    fn prefixes_come_before_contains_matches() {
        assert_eq!(&names("/re", SCROLLBACK)[..2], ["resume", "reconnect"]);
        // "sessions" (an alias of resume) only contains "ss".
        assert!(names("/ss", SCROLLBACK).contains(&"resume"));
        assert!(names("/quit", SCROLLBACK).contains(&"exit"));
        assert_eq!(names("/hist", SCROLLBACK), ["history"]);
    }

    #[test]
    fn rows_that_cannot_run_now_are_marked_unavailable() {
        let none = Context::default();
        assert!(rows_for("/hist", none, &[])[0].unavailable);
        assert!(!rows_for("/hist", SCROLLBACK, &[])[0].unavailable);
        let interrupt = &rows_for("/interrupt", SCROLLBACK, &[])[0];
        assert!(interrupt.unavailable);
        assert_eq!(interrupt.key.as_deref(), Some("Esc"));
    }

    #[test]
    fn rows_read_as_amps_category_label_and_key() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark);
        let rows = rows_for("/", FULLSCREEN, &[]);
        let mut state = PaletteState::default();
        state.set_query("");
        let panel = render_overlay(&state, &rows, 72, rows.len(), &theme.palette);
        let plain: Vec<String> = panel.iter().map(|r| strip(r)).collect();
        // Selected first row: the accent bar across the row.
        assert!(plain[4].starts_with("│   session  /new "), "{:?}", plain[4]);
        assert!(panel[4].contains("\x1b[30;43m"), "{:?}", panel[4]);
        let top = plain
            .iter()
            .position(|r| r.contains("scroll to the top"))
            .unwrap();
        assert!(
            plain[top].contains("   view  scroll to the top"),
            "{:?}",
            plain[top]
        );
        assert!(plain[top].trim_end_matches(['│', ' ']).ends_with("Home"));
        // Keys take the keybind colour, bold.
        assert!(panel[top].contains("\x1b[1;34m  Home"), "{:?}", panel[top]);
        // An action that cannot run now says so, faint.
        let interrupt = plain
            .iter()
            .position(|r| r.contains("interrupt the run"))
            .unwrap();
        assert!(plain[interrupt].contains("interrupt the run (unavailable)"));
        assert!(
            panel[interrupt].contains("\x1b[1;2m"),
            "{:?}",
            panel[interrupt]
        );
        assert!(plain.iter().all(|r| display_width(r) == 72), "{plain:#?}");
    }

    #[test]
    fn window_follows_the_selection() {
        let rows = rows_for("/", SCROLLBACK, &[]);
        let len = rows.len();
        let mut state = PaletteState::default();
        assert_eq!(state.window_start(&rows, PAGE_SIZE), 0);
        state.selected = 5;
        assert_eq!(state.window_start(&rows, PAGE_SIZE), 0);
        state.selected = 6;
        assert_eq!(state.window_start(&rows, PAGE_SIZE), 1);
        state.selected = len - 1;
        assert_eq!(state.window_start(&rows, PAGE_SIZE), len - PAGE_SIZE);
    }

    #[test]
    fn panel_has_a_fixed_height_and_a_position_footer() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark);
        let rows = rows_for("/", SCROLLBACK, &[]);
        let panel = render(&PaletteState::default(), &rows, 80, &theme.palette);
        assert_eq!(panel.len(), PAGE_SIZE + CHROME_ROWS);
        let plain: Vec<String> = panel.iter().map(|r| strip(r)).collect();
        assert!(plain[1].contains("Commands") && plain[1].contains("esc close"));
        assert!(plain[2].starts_with("│   session  /new"), "{:?}", plain[2]);
        assert!(
            plain[8].contains(&format!("1 / {}", rows.len())),
            "{:?}",
            plain[8]
        );
        assert!(plain.iter().all(|r| display_width(r) == 80), "{plain:#?}");
    }

    fn sample_skills() -> Vec<Skill> {
        vec![
            Skill {
                name: "claude-code-import".into(),
                description: "Import Claude Code session history".into(),
                display_name: None,
                display_description: None,
            },
            Skill {
                name: "codex-import".into(),
                description: "Import Codex history".into(),
                display_name: None,
                display_description: None,
            },
        ]
    }

    #[test]
    fn skills_share_the_list_under_their_own_category() {
        let skills = sample_skills();
        // Both kinds: commands first, told apart by category.
        let rows = rows_for("hist", SCROLLBACK, &skills);
        let categories: Vec<&str> = rows.iter().map(|r| r.category.as_str()).collect();
        assert_eq!(categories, ["session", "skill", "skill"]);
        assert_eq!(panel_title(&rows), "Suggestions");
        let only = rows_for("claude", SCROLLBACK, &skills);
        assert!(only.iter().all(|r| matches!(r.kind, Kind::Skill(_))));
        assert_eq!(panel_title(&only), "Skills");
        // A skill name prefix with no command name prefix leads.
        let lead = rows_for("codex", SCROLLBACK, &skills);
        assert_eq!(lead.len(), 1);
        let mut state = PaletteState::default();
        state.step(1, &rows);
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark);
        let panel = render(&state, &rows, 100, &theme.palette);
        let text: Vec<String> = panel.iter().map(|r| strip(r)).collect();
        assert!(text.iter().any(|r| r.contains("2 / 3")), "{text:#?}");
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut skip = false;
        for c in s.chars() {
            match (skip, c) {
                (false, '\x1b') => skip = true,
                (true, 'm') => skip = false,
                (false, c) => out.push(c),
                _ => {}
            }
        }
        out
    }
}
