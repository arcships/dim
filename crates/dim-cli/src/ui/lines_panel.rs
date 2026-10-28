use crate::model::view::{LineState, LineView, ViewSnapshot};
use crate::sanitize::safe;
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

const HINT: &str = "Enter show the line · x end it · Esc close";

/// The rows in tree order: the session itself, then each line under the line
/// that opened it, children in the order the snapshot lists them (design §14).
fn tree_order(lines: &[LineView]) -> Vec<(usize, usize)> {
    fn walk(
        lines: &[LineView],
        parent: usize,
        depth: usize,
        seen: &mut [bool],
        out: &mut Vec<(usize, usize)>,
    ) {
        for (index, line) in lines.iter().enumerate() {
            if seen[index] || line.parent.as_deref() != Some(lines[parent].id.as_str()) {
                continue;
            }
            seen[index] = true;
            out.push((index, depth));
            walk(lines, index, depth + 1, seen, out);
        }
    }
    let mut seen = vec![false; lines.len()];
    let mut out = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if line.parent.is_some() || seen[index] {
            continue;
        }
        seen[index] = true;
        out.push((index, 0));
        walk(lines, index, 1, &mut seen, &mut out);
    }
    for (index, seen) in seen.iter().enumerate() {
        if !seen {
            out.push((index, 0));
        }
    }
    out
}

pub struct LinesPanel {
    session_id: String,
    projection_id: u64,
    focus: String,
    lines: Vec<LineView>,
    list: ListPanel,
    /// The line the confirmation names, until the user answers.
    confirm: Option<String>,
}

impl LinesPanel {
    pub fn new(snapshot: &ViewSnapshot) -> Self {
        let mut panel = Self {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            focus: snapshot.focus.clone(),
            lines: Vec::new(),
            list: ListPanel::new(
                "Lines",
                HINT,
                "",
                "This session has no lines.",
                "No matching lines.",
                false,
            ),
            confirm: None,
        };
        panel.set_lines(snapshot);
        panel
    }

    /// Whether the panel still describes this attachment.
    pub fn current(&self, snapshot: &ViewSnapshot) -> bool {
        self.session_id == snapshot.session_id && self.projection_id == snapshot.projection_id
    }

    pub fn set_lines(&mut self, snapshot: &ViewSnapshot) {
        let selected = self.list.selected_id();
        self.focus = snapshot.focus.clone();
        self.lines = snapshot.lines.clone();
        let items = tree_order(&snapshot.lines)
            .into_iter()
            .map(|(index, depth)| {
                let line = &snapshot.lines[index];
                let state = match &line.state {
                    LineState::Main => t("the session itself").to_owned(),
                    LineState::Working => t("working").to_owned(),
                    LineState::Waiting => t("waiting").to_owned(),
                    LineState::EndedItself => t("ended itself").to_owned(),
                    LineState::EndedByOperation => t("ended by an operation").to_owned(),
                    LineState::EndedWith(ancestor) => tf("ended with {id}", &[("id", ancestor)]),
                    LineState::CreationFailed => t("creation failed").to_owned(),
                };
                let viewed = if line.id == self.focus {
                    format!(" · {}", t("shown"))
                } else {
                    String::new()
                };
                let detail = if line.purpose.is_empty() {
                    format!("{state}{viewed}")
                } else {
                    format!("{} · {state}{viewed}", safe(&line.purpose))
                };
                Item::new(
                    line.id.clone(),
                    format!("{}{}", "  ".repeat(depth), safe(&line.id)),
                    detail,
                )
            })
            .collect();
        let preselect = selected
            .filter(|id| snapshot.lines.iter().any(|line| &line.id == id))
            .or_else(|| Some(self.focus.clone()));
        self.list.set_items(Ok(items), preselect.as_deref());
    }

    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some(line) = self.confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') if key.mods == Modifiers::NONE => {
                    self.confirm = None;
                    return Effect::EndLine {
                        session_id: self.session_id.clone(),
                        projection_id: self.projection_id,
                        branch_id: line,
                    };
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                    if key.mods == Modifiers::NONE =>
                {
                    self.confirm = None;
                }
                _ => {}
            }
            return Effect::None;
        }
        if key.code == KeyCode::Char('x')
            && key.mods == Modifiers::NONE
            && self.list.filter_is_empty()
            && let Some(id) = self.list.selected_id()
        {
            if id == "main" {
                return Effect::Notice(
                    t("main is the session itself; a line ends, main does not").into(),
                );
            }
            if let Some(line) = self.lines.iter().find(|line| line.id == id) {
                if matches!(
                    line.state,
                    LineState::EndedItself
                        | LineState::EndedByOperation
                        | LineState::EndedWith(_)
                        | LineState::CreationFailed
                ) {
                    return Effect::Notice(format!("{} {id}", t("Line has ended:")));
                }
                self.confirm = Some(id);
            }
            return Effect::None;
        }
        match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(id) => Effect::FocusLine(id),
        }
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.list.cursor(width)
    }

    pub fn render(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        _markdown: &MarkdownRenderer,
    ) -> Vec<String> {
        let mut lines = self.list.render(width, rows, p);
        if let Some(line) = &self.confirm {
            let prompt = format!("{} {}? ({})", t("End line"), safe(line), t("y/N"));
            if lines.len() >= rows {
                lines.truncate(rows.saturating_sub(1));
            }
            lines.push(fg(&prompt, &p.warning));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::view::{LineState, LineView, ViewSnapshot};
    use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn snapshot(focus: &str) -> ViewSnapshot {
        ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 3,
            focus: focus.into(),
            lines: vec![
                LineView {
                    id: "main".into(),
                    parent: None,
                    purpose: String::new(),
                    state: LineState::Main,
                },
                LineView {
                    id: "b-7c".into(),
                    parent: Some("main".into()),
                    purpose: "parser race".into(),
                    state: LineState::Working,
                },
                LineView {
                    id: "b-02".into(),
                    parent: Some("main".into()),
                    purpose: "docs pass".into(),
                    state: LineState::EndedItself,
                },
                LineView {
                    id: "b-91".into(),
                    parent: Some("b-7c".into()),
                    purpose: "review".into(),
                    state: LineState::Waiting,
                },
            ],
            ..ViewSnapshot::default()
        }
    }

    fn palette() -> Palette {
        use crate::ui::theme::{Appearance, ColorMode, Theme};
        Theme::system(ColorMode::TrueColor, Appearance::Dark, None).palette
    }

    fn markdown() -> MarkdownRenderer {
        use crate::ui::theme::{Appearance, ColorMode, Theme};
        MarkdownRenderer::new(&Theme::system(ColorMode::TrueColor, Appearance::Dark, None))
    }

    #[test]
    fn main_is_the_session_and_only_lines_end_after_a_confirmation() {
        let mut panel = LinesPanel::new(&snapshot("main"));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
        assert!(matches!(panel.handle_key(key(KeyCode::Down)), Effect::None));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::None
        ));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('n'))),
            Effect::None
        ));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::None
        ));
        match panel.handle_key(key(KeyCode::Char('y'))) {
            Effect::EndLine {
                session_id,
                projection_id,
                branch_id,
            } => {
                assert_eq!((session_id.as_str(), projection_id), ("s1", 3));
                assert_eq!(branch_id, "b-7c");
            }
            _ => panic!("a confirmed end ends the line"),
        }
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::None
        ));
        panel.handle_key(key(KeyCode::Char('n')));
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
    }

    #[test]
    fn opening_a_line_asks_the_screen_to_follow_it() {
        let mut panel = LinesPanel::new(&snapshot("main"));
        panel.handle_key(key(KeyCode::Down));
        match panel.handle_key(key(KeyCode::Enter)) {
            Effect::FocusLine(line) => assert_eq!(line, "b-7c"),
            _ => panic!("Enter follows the selected line"),
        }
        assert!(panel.current(&snapshot("main")));
        let mut other = snapshot("main");
        other.session_id = "s2".into();
        assert!(!panel.current(&other));
    }

    #[test]
    fn a_line_below_a_line_is_listed_under_its_parent_and_can_be_ended() {
        let snapshot = snapshot("main");
        let mut panel = LinesPanel::new(&snapshot);
        let rows = panel
            .render(100, 20, &palette(), &markdown())
            .into_iter()
            .map(|row| crate::ui::render::panel::strip_escapes(&row))
            .collect::<Vec<_>>();
        let position = |id: &str| {
            rows.iter()
                .position(|row| row.contains(id))
                .unwrap_or_else(|| panic!("{id} is listed: {rows:#?}"))
        };
        assert!(rows.iter().any(|row| row.contains("    b-91")), "{rows:#?}");
        assert!(position("b-7c") < position("b-91"), "{rows:#?}");
        assert!(
            position("b-91") < position("b-02"),
            "a line follows the line that opened it, not arrival order: {rows:#?}"
        );
        for _ in 0..2 {
            panel.handle_key(key(KeyCode::Down));
        }
        panel.handle_key(key(KeyCode::Char('x')));
        match panel.handle_key(key(KeyCode::Char('y'))) {
            Effect::EndLine { branch_id, .. } => assert_eq!(branch_id, "b-91"),
            _ => panic!("a line below a line can be ended"),
        }
    }

    #[test]
    fn a_line_an_ancestor_ended_names_that_ancestor() {
        let mut snapshot = snapshot("main");
        snapshot.lines[1].state = LineState::EndedWith("main".into());
        let mut panel = LinesPanel::new(&snapshot);
        let rows = panel
            .render(100, 20, &palette(), &markdown())
            .into_iter()
            .map(|row| crate::ui::render::panel::strip_escapes(&row))
            .collect::<Vec<_>>();
        let row = rows
            .iter()
            .find(|row| row.contains("b-7c"))
            .expect("the line is listed");
        assert!(row.contains("ended with main"), "{row}");
        assert!(!row.contains("ended itself"), "{row}");
        panel.handle_key(key(KeyCode::Down));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
    }
}
