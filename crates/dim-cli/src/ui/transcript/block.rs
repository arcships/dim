//! The scrollback block model: what one committed or retained row of the
//! transcript is, as data.
//!
//! A settled view row is projected into a [`Block`] once ([`project_row`]);
//! the commit path and the reflow path both draw a block through the same
//! renderer, so a row cannot look different when it is redrawn after a
//! resize. Blocks carry semantic [`Tone`]s, never colours: the palette is
//! applied at draw time.

use std::sync::Arc;

use crate::model::shell::ShellRun;
use crate::model::view::{RowKind, RowTone, ViewRow};
use crate::ui::transcript::tool_group::{Detail, ToolGroup, project_group};

/// How a status line is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
pub enum Block {
    /// The startup banner. Drawn at the terminal's full width, not from a row.
    Banner,
    User(String),
    /// A shell-mode run: recorded (`!`) from the transcript, private (`!!`)
    /// from the terminal alone.
    Shell(ShellRun),
    Assistant {
        text: String,
        first: bool,
    },
    /// A settled thought: one summary row, or the whole text when expanded.
    Thinking {
        text: String,
        elapsed_ms: Option<u64>,
        expanded: bool,
    },
    /// The calls of one model request.
    ToolGroup(ToolGroup),
    Error(String),
    Status {
        text: String,
        tone: Tone,
    },
    Failure(String),
    /// A centred rule with a label (session switches).
    Boundary(String),
    /// One muted line (earlier-history page header).
    Note(String),
}

/// The block the settled view row at `index` draws as. A tool call draws
/// its whole group (see [`group_rows`]).
///
/// Assistant text is not projected here: a finished answer may already be
/// laid out by the stream planner, which the caller decides.
pub fn project_row(rows: &[Arc<ViewRow>], index: usize, detail: Detail) -> Block {
    let row = &rows[index];
    match row.kind {
        RowKind::UserMessage => Block::User(row.text.clone()),
        // The projection made it a shell row because it parses.
        RowKind::Shell => {
            ShellRun::parse(&row.text).map_or_else(|| Block::User(row.text.clone()), Block::Shell)
        }
        RowKind::AssistantMessage => Block::Assistant {
            text: row.text.clone(),
            first: true,
        },
        RowKind::Thinking => Block::Thinking {
            text: row.text.clone(),
            elapsed_ms: row.elapsed_ms,
            expanded: detail == Detail::Expanded,
        },
        RowKind::ToolCall => Block::ToolGroup(project_group(&group_rows(rows, index), detail)),
        RowKind::Error => Block::Error(row.text.clone()),
        // A still-streaming answer met outside the live region (history
        // pages) reads as a status line, as it always has.
        RowKind::StreamingAssistant | RowKind::Status | RowKind::Other => Block::Status {
            text: row.text.clone(),
            tone: match row.tone {
                Some(RowTone::Danger) => Tone::Danger,
                Some(RowTone::Warning) => Tone::Warning,
                None => Tone::Muted,
            },
        },
    }
}

/// Whether the call at `index` opens a group: the calls of one model
/// request are consecutive rows, and only the first stands for the group.
pub fn opens_group(rows: &[Arc<ViewRow>], index: usize) -> bool {
    let row = &rows[index];
    row.kind == RowKind::ToolCall
        && (index == 0 || {
            let previous = &rows[index - 1];
            previous.kind != RowKind::ToolCall || previous.batch != row.batch
        })
}

/// The calls of the group that opens at `index`.
pub fn group_rows(rows: &[Arc<ViewRow>], index: usize) -> Vec<&ViewRow> {
    let batch = &rows[index].batch;
    std::iter::once(&*rows[index])
        .chain(
            rows[index + 1..]
                .iter()
                .map(|row| &**row)
                .take_while(|row| row.kind == RowKind::ToolCall && &row.batch == batch),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};
    use crate::ui::transcript::tool_group::{State, render_group};
    use serde_json::json;

    fn row(kind: RowKind, text: &str) -> ViewRow {
        ViewRow {
            row_id: "r".into(),
            kind,
            text: text.into(),
            streaming: false,
            elapsed_ms: None,
            tone: None,
            tool_name: None,
            tool_input: None,
            batch: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
        }
    }

    #[test]
    fn unknown_and_streaming_rows_draw_as_muted_status() {
        for kind in [RowKind::Other, RowKind::StreamingAssistant, RowKind::Status] {
            let block = project_row(&shared(&[row(kind, "x")]), 0, Detail::Collapsed);
            assert!(matches!(
                block,
                Block::Status {
                    tone: Tone::Muted,
                    ..
                }
            ));
        }
    }

    #[test]
    fn status_tone_follows_the_row() {
        let mut warn = row(RowKind::Status, "x");
        warn.tone = Some(RowTone::Warning);
        assert!(matches!(
            project_row(&shared(&[warn]), 0, Detail::Collapsed),
            Block::Status {
                tone: Tone::Warning,
                ..
            }
        ));
    }

    /// Rows as a snapshot publishes them.
    fn shared(rows: &[ViewRow]) -> Vec<Arc<ViewRow>> {
        rows.iter().cloned().map(Arc::new).collect()
    }

    fn call(name: &str, input: serde_json::Value, result: Option<&str>) -> ViewRow {
        let mut call = row(RowKind::ToolCall, "");
        call.row_id = format!("c-{name}-{}", input);
        call.tool_name = Some(name.into());
        call.tool_input = Some(input);
        call.batch = Some("r1".into());
        call.result_text = result.map(str::to_owned);
        call
    }

    /// The committed text of a transcript's first group, as the terminal shows it.
    fn drawn(rows: &[ViewRow], detail: Detail, width: usize) -> Vec<String> {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark);
        let Block::ToolGroup(group) = project_row(&shared(rows), 0, detail) else {
            panic!("not a group");
        };
        render_group(&group, width, &theme.palette, "·")
            .iter()
            .map(|r| {
                let mut out = String::new();
                let mut skip = false;
                for c in r.chars() {
                    match (skip, c) {
                        (false, '\x1b') => skip = true,
                        (true, 'm') => skip = false,
                        (false, c) => out.push(c),
                        _ => {}
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn the_calls_of_one_request_are_one_group() {
        let mut rows = vec![
            call("exec", json!({ "command": "npm test" }), Some("1 failing")),
            call("grep", json!({ "pattern": "TODO" }), Some("ok")),
            call("read", json!({ "path": "/repo/a.ts" }), Some("ok")),
        ];
        rows[0].result_is_error = true;
        let mut other = call("read", json!({ "path": "b.ts" }), Some("ok"));
        other.batch = Some("r2".into());
        rows.push(other);
        let published = shared(&rows);
        assert!(opens_group(&published, 0));
        assert!(!opens_group(&published, 1) && !opens_group(&published, 2));
        assert!(
            opens_group(&published, 3),
            "another request opens its own group"
        );
        assert_eq!(group_rows(&published, 0).len(), 3);
        assert_eq!(
            drawn(&rows, Detail::Collapsed, 80),
            ["✓ Ran 1 command, ran 1 search, read 1 file · 1 failed"]
        );
    }

    #[test]
    fn a_call_waiting_on_approval_keeps_its_group_running() {
        let mut parked = call("bash", json!({ "command": "rm x" }), None);
        parked.pending = true;
        parked.parked = true;
        let Block::ToolGroup(group) = project_row(&shared(&[parked]), 0, Detail::Collapsed) else {
            panic!("not a group");
        };
        assert_eq!(group.state, State::Running);
    }

    #[test]
    fn a_multi_line_script_counts_as_a_command() {
        let rows = [call(
            "exec",
            json!({ "command": "ls\nnpm test" }),
            Some("PASS"),
        )];
        assert_eq!(
            drawn(&rows, Detail::Expanded, 80),
            ["✓ Ran 1 command", "  $ ls", "  npm test", "    PASS"]
        );
    }

    #[test]
    fn a_long_command_keeps_four_rows_and_counts_the_rest() {
        let body: Vec<String> = (1..=40).map(|i| format!("row {i}")).collect();
        let command = format!("cat > gen.txt <<'EOF'\n{}\nEOF", body.join("\n"));
        let rows = [call("exec", json!({ "command": command }), Some(""))];
        assert_eq!(
            drawn(&rows, Detail::Expanded, 80),
            [
                "✓ Ran 1 command",
                "  $ cat > gen.txt <<'EOF'",
                "  row 1",
                "  row 2",
                "  … +39 lines"
            ]
        );
    }

    #[test]
    fn a_large_output_shows_only_its_tail() {
        let output: Vec<String> = (1..=100_000).map(|i| format!("line {i}")).collect();
        let rows = [call(
            "exec",
            json!({ "command": "seq" }),
            Some(&output.join("\n")),
        )];
        let lines = drawn(&rows, Detail::Expanded, 80);
        let expected: Vec<String> = (99_986..=100_000)
            .map(|i| format!("    line {i}"))
            .collect();
        assert_eq!(lines[2..], expected[..]);
    }

    #[test]
    fn hunk_lines_carry_file_numbers_and_dashes_inside_a_hunk_are_content() {
        let mut edit = call("edit", json!({ "path": "a.sql" }), Some("Edited a.sql"));
        edit.diff = Some("@@ -3,2 +3,2 @@\n--- old comment\n+++new value\n same".into());
        let lines = drawn(&[edit], Detail::Expanded, 80);
        assert_eq!(lines[0], "✓ Edited 1 file (+1 -1)");
        assert_eq!(
            lines[1..],
            [
                "  ✓ Edited a.sql +1 -1",
                "    ...",
                "      3 -  -- old comment",
                "      3 +  ++new value",
                "      4    same"
            ]
        );
    }

    #[test]
    fn a_capped_row_ends_on_a_whole_grapheme_within_the_width() {
        let rows = [call(
            "exec",
            json!({ "command": "printf" }),
            Some(&"😀".repeat(200)),
        )];
        for line in drawn(&rows, Detail::Expanded, 20) {
            assert!(
                crate::ui::transcript::blocks::display_width(&line) <= 20,
                "{line}"
            );
        }
    }

    #[test]
    fn a_call_the_run_ended_on_draws_no_empty_error() {
        let rows = [call(
            "edit",
            json!({ "path": "a.ts", "oldText": "x", "newText": "y" }),
            None,
        )];
        assert_eq!(
            drawn(&rows, Detail::Expanded, 80),
            ["✗ Edited 1 file", "  ✗ Edit a.ts"]
        );
    }
}
