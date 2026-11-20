use crate::model::view::{LineState, LineView, ViewSnapshot};
use crate::sanitize::safe;
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use dim_agent_types::ROOT_BRANCH_ID;

const HINT: &str = "Enter show · p continue from here · x end it · Esc close";

/// The rows in tree order: the session itself, then each line under the line
/// that opened it, children by where they were cut from it and then in the
/// order the snapshot lists them (design §14, timeline UI §6).
/// A line's id as the screen names it: its first eight characters.
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// How a line stands, as the panels say it.
pub fn state_label(state: &LineState) -> String {
    match state {
        LineState::Main => t("the session itself").to_owned(),
        LineState::Working => t("working").to_owned(),
        LineState::Waiting => t("waiting").to_owned(),
        LineState::Ended => t("ended").to_owned(),
        LineState::EndedItself => t("ended itself").to_owned(),
        LineState::EndedByOperation => t("ended by an operation").to_owned(),
        LineState::EndedWith(ancestor) => tf("ended with {id}", &[("id", ancestor)]),
        LineState::CreationFailed => t("creation failed").to_owned(),
    }
}

fn tree_order(lines: &[LineView]) -> Vec<(usize, usize)> {
    fn walk(
        lines: &[LineView],
        parent: usize,
        depth: usize,
        seen: &mut [bool],
        out: &mut Vec<(usize, usize)>,
    ) {
        let mut children: Vec<usize> = (0..lines.len())
            .filter(|index| {
                !seen[*index] && lines[*index].parent.as_deref() == Some(lines[parent].id.as_str())
            })
            .collect();
        children.sort_by_key(|index| lines[*index].cut.map(|cut| cut.sequence));
        for index in children {
            if seen[index] {
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
    default_branch_id: String,
    lines: Vec<LineView>,
    list: ListPanel,
    /// The line the confirmation names, until the user answers.
    confirm: Option<String>,
    /// Why an action was refused and the line it was raised on, until the
    /// choice changes (design §14).
    refusal: Option<(String, String)>,
}

impl LinesPanel {
    pub fn new(snapshot: &ViewSnapshot) -> Self {
        let mut panel = Self {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            focus: snapshot.focus.clone(),
            default_branch_id: snapshot.default_branch_id.clone(),
            lines: Vec::new(),
            list: ListPanel::new(
                "Lines",
                t(HINT),
                "",
                "This session has no lines.",
                "No matching lines.",
                false,
            ),
            confirm: None,
            refusal: None,
        };
        panel.set_lines(snapshot);
        panel
    }

    /// Whether the panel still describes this attachment.
    pub fn current(&self, snapshot: &ViewSnapshot) -> bool {
        self.session_id == snapshot.session_id && self.projection_id == snapshot.projection_id
    }

    /// Shows why an action was refused, in the panel that raised it, on the
    /// line that raised it, until the choice changes (design §14).
    pub fn refuse(&mut self, projection_id: u64, branch_id: &str, text: String) -> bool {
        if projection_id != self.projection_id {
            return false;
        }
        self.refusal = Some((branch_id.to_owned(), text));
        self.refresh_note();
        true
    }

    /// Refuses the action on the selected line, without a round trip.
    fn refuse_selected(&mut self, text: String) {
        let line = self.list.selected_id().unwrap_or_default();
        self.refuse(self.projection_id, &line, text);
    }

    pub fn set_lines(&mut self, snapshot: &ViewSnapshot) {
        let selected = self.list.selected_id();
        self.focus = snapshot.focus.clone();
        if snapshot.default_branch_id != self.default_branch_id {
            self.default_branch_id = snapshot.default_branch_id.clone();
            self.refusal = None;
        }
        self.lines = snapshot.lines.clone();
        let order = tree_order(&snapshot.lines);
        let items = order
            .iter()
            .copied()
            .map(|(index, depth)| {
                let line = &snapshot.lines[index];
                let state = state_label(&line.state);
                let viewed = if line.id == self.focus {
                    format!(" · {}", t("shown"))
                } else {
                    String::new()
                };
                let mut mark = String::new();
                if line.id == self.default_branch_id {
                    mark.push_str(" · ");
                    mark.push_str(t("Main line"));
                }
                if line.id == ROOT_BRANCH_ID {
                    mark.push_str(" · ");
                    mark.push_str(t("Start"));
                }
                let mut detail = String::new();
                let unread = snapshot.unread_lines.contains(&line.id);
                if unread {
                    detail.push_str(t("Unread"));
                    detail.push_str(" · ");
                }
                if let Some(cut) = line.cut {
                    detail.push_str(&tf(
                        "after message {count}",
                        &[("count", &cut.messages.to_string())],
                    ));
                    detail.push_str(" · ");
                }
                let title = if line.purpose.is_empty() {
                    safe(short_id(&line.id))
                } else {
                    detail.push_str(&safe(short_id(&line.id)));
                    detail.push_str(" · ");
                    safe(&line.purpose)
                };
                detail.push_str(&state);
                detail.push_str(&mark);
                detail.push_str(&viewed);
                Item::new(
                    line.id.clone(),
                    format!(
                        "{}{}{title}",
                        "  ".repeat(depth),
                        if unread { "* " } else { "" }
                    ),
                    detail,
                )
            })
            .collect();
        let preselect = selected
            .filter(|id| snapshot.lines.iter().any(|line| &line.id == id))
            .or_else(|| {
                order
                    .iter()
                    .map(|(index, _)| &snapshot.lines[*index])
                    .find(|line| snapshot.unread_lines.contains(&line.id))
                    .map(|line| line.id.clone())
            })
            .or_else(|| Some(self.focus.clone()));
        self.list.set_items(Ok(items), preselect.as_deref());
        self.refresh_note();
    }

    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some(line) = self.confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') if key.mods == Modifiers::NONE => {
                    self.answer();
                    return Effect::EndLine {
                        session_id: self.session_id.clone(),
                        projection_id: self.projection_id,
                        branch_id: line,
                    };
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                    if key.mods == Modifiers::NONE =>
                {
                    self.answer();
                }
                _ => {}
            }
            return Effect::None;
        }
        if key.mods == Modifiers::NONE
            && self.list.filter_is_empty()
            && let Some(id) = self.list.selected_id()
        {
            if key.code == KeyCode::Char('x') {
                let refusal = if id == ROOT_BRANCH_ID {
                    Some(t("main is the session itself; a line ends, main does not").to_owned())
                } else if self.is_default_or_above(&id) {
                    Some(
                        t("the default branch and the lines it continues from stay open; continue on another line first")
                            .to_owned(),
                    )
                } else {
                    self.lines
                        .iter()
                        .find(|line| line.id == id)
                        .filter(|line| {
                            matches!(
                                line.state,
                                LineState::Ended
                                    | LineState::EndedItself
                                    | LineState::EndedByOperation
                                    | LineState::EndedWith(_)
                                    | LineState::CreationFailed
                            )
                        })
                        .map(|_| format!("{} {id}", t("Line has ended:")))
                };
                if let Some(refusal) = refusal {
                    self.refuse_selected(refusal.clone());
                    return Effect::Notice(refusal);
                }
                if self.lines.iter().any(|line| line.id == id) {
                    self.ask_to_end(id);
                }
                return Effect::None;
            }
            if key.code == KeyCode::Char('p') {
                self.confirm = None;
                self.refusal = None;
                self.list.set_hint(t(HINT).to_owned());
                return Effect::SetDefaultLine {
                    session_id: self.session_id.clone(),
                    projection_id: self.projection_id,
                    branch_id: id,
                };
            }
        }
        let effect = match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick { id, .. } => Effect::FocusLine(id),
        };
        self.refresh_note();
        effect
    }

    /// Whether `id` is the default branch or an ancestor of it: no end of it
    /// is admitted, because an end takes every line below it (design §10).
    fn is_default_or_above(&self, id: &str) -> bool {
        let mut current = Some(self.default_branch_id.as_str());
        while let Some(line) = current {
            if line == id {
                return true;
            }
            current = self
                .lines
                .iter()
                .find(|candidate| candidate.id == line)
                .and_then(|candidate| candidate.parent.as_deref());
        }
        false
    }

    /// The panel's own paragraphs: what a move leaves in place, and the line
    /// the last refusal named. The root keeps its right to open a line in
    /// either direction, so it never trades it with the role (design §14).
    fn refresh_note(&mut self) {
        let selected = self.list.selected_id().unwrap_or_default();
        let keeps_its_right =
            selected == ROOT_BRANCH_ID || self.default_branch_id == ROOT_BRANCH_ID;
        let mut notes = vec![t(if keeps_its_right {
            "The old default branch stays open and keeps the lines it opened. The root keeps its own right to open a line in either direction."
        } else {
            "The old default branch stays open and keeps the lines it opened. The two lines swap the right to open a line."
        })
        .to_owned()];
        if let Some((line, refusal)) = &self.refusal
            && *line == selected
        {
            notes.push(refusal.clone());
        }
        self.list.set_notes(notes);
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.list.cursor(width)
    }

    /// Asks for the end of `id` in the panel's hint, naming the line as the
    /// rows name it, until the user answers.
    fn ask_to_end(&mut self, id: String) {
        let question = tf("End line {id}?", &[("id", &safe(short_id(&id)))]);
        self.list
            .set_confirmation(question, t("y end it").to_owned(), t("n cancel").to_owned());
        self.confirm = Some(id);
    }

    /// Drops the question and gives the panel its key hint back.
    fn answer(&mut self) {
        self.confirm = None;
        self.list.set_hint(t(HINT).to_owned());
    }

    pub fn render(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        _markdown: &MarkdownRenderer,
    ) -> Vec<String> {
        self.list.render(width, rows, p)
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
            default_branch_id: "main".into(),
            lines: vec![
                LineView {
                    id: "main".into(),
                    parent: None,
                    purpose: String::new(),
                    state: LineState::Main,
                    cut: None,
                },
                LineView {
                    id: "b-7c".into(),
                    parent: Some("main".into()),
                    purpose: "parser race".into(),
                    state: LineState::Working,
                    cut: Some(crate::model::view::LineCut {
                        sequence: 1,
                        messages: 0,
                    }),
                },
                LineView {
                    id: "b-02".into(),
                    parent: Some("main".into()),
                    purpose: "docs pass".into(),
                    state: LineState::EndedItself,
                    cut: Some(crate::model::view::LineCut {
                        sequence: 1,
                        messages: 0,
                    }),
                },
                LineView {
                    id: "b-91".into(),
                    parent: Some("b-7c".into()),
                    purpose: "review".into(),
                    state: LineState::Waiting,
                    cut: Some(crate::model::view::LineCut {
                        sequence: 1,
                        messages: 0,
                    }),
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

    fn rows(panel: &LinesPanel) -> Vec<String> {
        panel
            .render(100, 20, &palette(), &markdown())
            .into_iter()
            .map(|row| crate::ui::render::panel::strip_escapes(&row))
            .collect()
    }

    /// The drawn rows as one string, borders and padding dropped, so a phrase
    /// a wrap split still reads.
    fn text(panel: &LinesPanel) -> String {
        rows(panel)
            .join(" ")
            .replace(['│', '╭', '╮', '╰', '╯', '─'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn main_is_the_session_and_only_lines_end_after_a_confirmation() {
        let mut panel = LinesPanel::new(&snapshot("main"));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
        assert!(
            text(&panel).contains(t("main is the session itself; a line ends, main does not")),
            "the refusal is drawn in the panel"
        );
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
        assert!(
            text(&panel).contains(t("Line has ended:")),
            "the refusal is drawn in the panel"
        );
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
        let rows = rows(&panel);
        let position = |id: &str| {
            rows.iter()
                .position(|row| row.contains(id))
                .unwrap_or_else(|| panic!("{id} is listed: {rows:#?}"))
        };
        let review = &rows[position("b-91")];
        assert!(review.contains("    review"), "{rows:#?}");
        assert!(
            review.find("review") < review.find("b-91"),
            "the title leads, the id follows: {review}"
        );
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
    fn lines_are_listed_where_they_were_cut_from_their_parent() {
        let mut snapshot = snapshot("main");
        snapshot.lines[2].cut = Some(crate::model::view::LineCut {
            sequence: 0,
            messages: 0,
        });
        snapshot.lines[1].cut = Some(crate::model::view::LineCut {
            sequence: 9,
            messages: 3,
        });
        let rows = rows(&LinesPanel::new(&snapshot));
        let position = |id: &str| rows.iter().position(|row| row.contains(id)).unwrap();
        assert!(
            position("b-02") < position("b-7c"),
            "the earlier cut comes first, whatever opened first: {rows:#?}"
        );
        assert!(
            rows[position("b-7c")].contains("after message 3"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_line_an_ancestor_ended_names_that_ancestor() {
        let mut snapshot = snapshot("main");
        snapshot.lines[1].state = LineState::EndedWith("main".into());
        let mut panel = LinesPanel::new(&snapshot);
        let drawn = rows(&panel);
        let row = drawn
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
        assert!(
            text(&panel).contains(t("Line has ended:")),
            "an ended line is refused in the panel"
        );
    }

    #[test]
    fn the_root_and_the_default_branch_are_marked_and_p_chooses_a_line() {
        let mut snapshot = snapshot("main");
        snapshot.default_branch_id = "b-7c".into();
        let mut panel = LinesPanel::new(&snapshot);
        let drawn = rows(&panel);
        let row = |id: &str| {
            drawn
                .iter()
                .find(|row| row.contains(id))
                .unwrap_or_else(|| panic!("{id} is listed: {drawn:#?}"))
                .clone()
        };
        assert!(row("main").contains(t("Start")), "{drawn:#?}");
        assert!(row("b-7c").contains(t("Main line")), "{drawn:#?}");
        assert!(!row("b-7c").contains(t("Start")), "{drawn:#?}");
        assert!(!row("b-91").contains(t("Main line")), "{drawn:#?}");
        panel.handle_key(key(KeyCode::Down));
        match panel.handle_key(key(KeyCode::Char('p'))) {
            Effect::SetDefaultLine { branch_id, .. } => assert_eq!(branch_id, "b-7c"),
            _ => panic!("p continues on the selected line"),
        }
    }

    #[test]
    fn x_is_refused_on_the_default_branch_and_on_a_line_above_it() {
        let mut snapshot = snapshot("main");
        snapshot.default_branch_id = "b-91".into();
        let mut panel = LinesPanel::new(&snapshot);
        for _ in 0..2 {
            panel.handle_key(key(KeyCode::Down));
        }
        let refusal = t(
            "the default branch and the lines it continues from stay open; continue on another line first",
        );
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
        assert!(
            text(&panel).contains(refusal),
            "x on the default branch is refused in the panel"
        );
        panel.handle_key(key(KeyCode::Up));
        assert!(matches!(
            panel.handle_key(key(KeyCode::Char('x'))),
            Effect::Notice(_)
        ));
        assert!(
            text(&panel).contains(refusal),
            "x on an ancestor of the default branch is refused in the panel"
        );
    }

    #[test]
    fn a_refused_move_is_shown_on_the_line_that_raised_it() {
        let mut panel = LinesPanel::new(&snapshot("main"));
        assert!(panel.refuse(
            3,
            "b-7c",
            "branch_already_default: the line is already the default branch".into()
        ));
        assert!(
            !text(&panel).contains("branch_already_default"),
            "the refusal belongs to the line that raised it: {:#?}",
            rows(&panel)
        );
        assert!(matches!(panel.handle_key(key(KeyCode::Down)), Effect::None));
        let drawn = rows(&panel);
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("branch_already_default")),
            "{drawn:#?}"
        );
        assert!(
            text(&panel).contains("old default branch stays open"),
            "the panel keeps its own paragraph: {drawn:#?}"
        );
        assert!(!panel.refuse(4, "b-7c", "another attachment".into()));
    }
}
