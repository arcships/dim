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
use crate::model::view::{CompactionPartSize, CompactionState, RowKind, RowTone, ViewRow};
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
    /// A user message; `cut` is where a line or fork opened from it cuts, and
    /// `pending` is that the model has not consumed it yet (timeline UI §4.8).
    User {
        text: String,
        display: Option<String>,
        cut: Option<u64>,
        pending: bool,
    },
    /// A shell-mode run: recorded (`!`) from the transcript, private (`!!`)
    /// from the terminal alone.
    Shell(ShellRun),
    Assistant(String),
    /// A settled thought: one summary row, or the whole text when expanded.
    Thinking {
        text: String,
        elapsed_ms: Option<u64>,
        expanded: bool,
    },
    /// The calls of one model request.
    ToolGroup(ToolGroup),
    /// A context compaction: running, then its outcome; a completed one's
    /// summary sits behind the disclosure.
    Compaction {
        state: CompactionState,
        text: String,
        insufficient: Vec<CompactionPartSize>,
        expanded: bool,
    },
    Error(String),
    Status {
        text: String,
        tone: Tone,
    },
    AccountUsage(crate::model::session::Usage),
    Failure(String),
    /// A centred rule with a label (session switches).
    Boundary(String),
    /// One muted line (earlier-history page header).
    Note(String),
    /// Fullscreen's first block while this many messages before it are not
    /// drawn; a pick or a click draws the previous page.
    LoadMore(usize),
    /// Where a line opened on the shown line; a pick enters it.
    Line {
        id: String,
        text: String,
    },
}

/// The block the settled view row at `index` draws as. A tool call draws
/// its whole group (see [`group_rows`]).
///
/// Assistant text is not projected here: a finished answer may already be
/// laid out by the stream planner, which the caller decides.
pub fn project_row(rows: &[Arc<ViewRow>], index: usize, detail: Detail) -> Block {
    let row = &rows[index];
    if let Some(id) = &row.line {
        return Block::Line {
            id: id.clone(),
            text: row.text.clone(),
        };
    }
    match row.kind {
        RowKind::UserMessage => Block::User {
            text: row.text.clone(),
            display: row.display.clone(),
            cut: row.cut,
            pending: row.pending,
        },
        RowKind::Shell => ShellRun::parse(&row.text).map_or_else(
            || Block::User {
                text: row.text.clone(),
                display: row.display.clone(),
                cut: row.cut,
                pending: row.pending,
            },
            Block::Shell,
        ),
        RowKind::AssistantMessage => Block::Assistant(row.text.clone()),
        RowKind::Thinking => Block::Thinking {
            text: row.text.clone(),
            elapsed_ms: row.elapsed_ms,
            expanded: detail == Detail::Expanded,
        },
        RowKind::ToolCall => Block::ToolGroup(project_group(&group_rows(rows, index), detail)),
        RowKind::Compaction => match &row.compaction {
            Some(compaction) => Block::Compaction {
                state: compaction.state,
                text: compaction.text.clone(),
                insufficient: compaction.insufficient.clone(),
                expanded: detail == Detail::Expanded,
            },
            None => Block::Status {
                text: row.text.clone(),
                tone: Tone::Muted,
            },
        },
        RowKind::Error => Block::Error(row.text.clone()),
        RowKind::StreamingAssistant | RowKind::Status | RowKind::Other => Block::Status {
            text: if row.text == crate::model::view::INTERRUPT_REASON {
                crate::ui::i18n::t(crate::model::view::INTERRUPT_REASON).to_owned()
            } else {
                row.text.clone()
            },
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
    use crate::model::view::CompactionView;
    use crate::ui::render::markdown::MarkdownRenderer;
    use crate::ui::theme::{Appearance, ColorMode, Theme};
    use crate::ui::transcript::blocks::render_block;
    use crate::ui::transcript::tool_group::{State, render_group};
    use serde_json::json;

    fn row(kind: RowKind, text: &str) -> ViewRow {
        ViewRow {
            row_id: "r".into(),
            kind,
            text: text.into(),
            display: None,
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
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            compaction: None,
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
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let Block::ToolGroup(group) = project_row(&shared(rows), 0, detail) else {
            panic!("not a group");
        };
        render_group(&group, width, &theme.palette, crate::ui::text::RUNNING_MARK)
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
            ["✓ Ran 1 command, ran 1 search, read 1 file · 1 failed ▸"]
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
            ["✓ Ran 1 command ▾", "  $ ls", "  npm test ▾", "    PASS"]
        );
    }

    #[test]
    fn a_long_command_is_shown_whole() {
        let body: Vec<String> = (1..=40).map(|i| format!("row {i}")).collect();
        let command = format!("cat > gen.txt <<'EOF'\n{}\nEOF", body.join("\n"));
        let rows = [call("exec", json!({ "command": command }), Some(""))];
        let lines = drawn(&rows, Detail::Expanded, 80);
        assert_eq!(lines.len(), 1 + 42);
        assert_eq!(lines[1], "  $ cat > gen.txt <<'EOF'");
        assert_eq!(lines[42], "  EOF");
    }

    #[test]
    fn tool_output_cannot_emit_terminal_commands_or_links() {
        let attack = "visible\x1b]52;c;clipboard-payload\x07\x1b]8;;file:///tmp/unsafe-link\x07label\x1b]8;;\x07\x1b[2J";
        let name = "exec";
        let rows = shared(&[call(
            name,
            json!({"command":"echo output", "path":"file.txt"}),
            Some(attack),
        )]);
        let Block::ToolGroup(group) = project_row(&rows, 0, Detail::Expanded) else {
            panic!("tool group")
        };
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let rendered =
            render_group(&group, 80, &theme.palette, crate::ui::text::RUNNING_MARK).join("\n");
        for control in [
            "\x1b]",
            "\x1b[2J",
            "clipboard-payload",
            "file:///tmp/unsafe-link",
        ] {
            assert!(!rendered.contains(control), "{name}: {rendered:?}");
        }
        assert!(
            crate::sanitize::safe(&rendered).contains("visible"),
            "{name}: {rendered:?}"
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
        assert_eq!(lines[2], "    … +99985 lines");
        let expected: Vec<String> = (99_986..=100_000)
            .map(|i| format!("    line {i}"))
            .collect();
        assert_eq!(lines[3..], expected);
    }

    #[test]
    fn an_output_that_fits_has_no_more_lines_note() {
        let output: Vec<String> = (1..=15).map(|i| format!("line {i}")).collect();
        let rows = [call(
            "exec",
            json!({ "command": "seq" }),
            Some(&output.join("\n")),
        )];
        let lines = drawn(&rows, Detail::Expanded, 80);
        assert_eq!(lines[2], "    line 1");
        assert_eq!(lines.len(), 17);
    }

    #[test]
    fn hunk_lines_carry_file_numbers_and_dashes_inside_a_hunk_are_content() {
        let mut edit = call("edit", json!({ "path": "a.sql" }), Some("Edited a.sql"));
        edit.diff = Some("@@ -3,2 +3,2 @@\n--- old comment\n+++new value\n same".into());
        let lines = drawn(&[edit], Detail::Expanded, 80);
        assert_eq!(lines[0], "✓ Edited a.sql (+1 -1) ▾");
        assert_eq!(
            lines[1..],
            [
                "  ✓ Edited a.sql +1 -1 ▾",
                "    ...",
                "      3 -  -- old comment",
                "      3 +  ++new value",
                "      4    same"
            ]
        );
    }

    #[test]
    fn collapsed_edits_name_unique_paths_and_keep_totals_in_mixed_groups() {
        let mut edit = call("edit", json!({ "path": "src/main.rs" }), Some("ok"));
        edit.diff = Some("@@ -1,1 +1,2 @@\n-old\n+new\n+extra".into());
        assert_eq!(
            drawn(&[edit.clone()], Detail::Collapsed, 100),
            ["✓ Edited src/main.rs (+2 -1) ▸"]
        );
        let rows = [
            edit.clone(),
            edit,
            call(
                "write",
                json!({ "path": "tests/main.rs", "content": "test\n" }),
                Some("ok"),
            ),
        ];
        assert_eq!(
            drawn(&rows, Detail::Collapsed, 100),
            ["✓ Changed 2 files (+5 -2) · src/main.rs, tests/main.rs ▸"]
        );
        let mut mixed = vec![call(
            "exec",
            json!({ "command": "cargo check" }),
            Some("ok"),
        )];
        mixed.extend(rows);
        let expected = "✓ Ran 1 command, changed 2 files (+5 -2) · src/main.rs, tests/main.rs";
        assert_eq!(
            drawn(&mixed, Detail::Collapsed, 100),
            [format!("{expected} ▸")]
        );
        assert_eq!(
            drawn(&mixed, Detail::Expanded, 100)[0],
            format!("{expected} ▾")
        );
        mixed[1].result_is_error = true;
        assert_eq!(
            drawn(&mixed, Detail::Collapsed, 100),
            ["✓ Ran 1 command, changed 2 files (+3 -1) · src/main.rs, tests/main.rs · 1 failed ▸"]
        );
    }

    #[test]
    fn collapsed_edit_paths_fit_the_terminal_without_losing_totals() {
        use crate::ui::i18n::{self, Locale};

        let mut edit = call("edit", json!({ "path": "src/main.rs" }), Some("ok"));
        edit.diff = Some("@@ -1,1 +1,2 @@\n-old\n+new\n+extra".into());
        let rows = [
            edit,
            call("edit", json!({ "path": "src/config.rs" }), Some("ok")),
            call("edit", json!({ "path": "tests/config.rs" }), Some("ok")),
        ];
        assert_eq!(
            drawn(&rows, Detail::Collapsed, 50),
            ["✓ Edited 3 files (+2 -1) · src/main.rs, … 2 more ▸"]
        );
        assert_eq!(
            drawn(&rows[..1], Detail::Collapsed, 28),
            ["✓ Edited src/main… (+2 -1) ▸"]
        );
        let mut unicode = rows[0].clone();
        unicode.tool_input = Some(json!({"path": "src/配置/👩‍💻设置.rs"}));
        for (locale, _, _) in i18n::LOCALES {
            i18n::set(locale);
            for width in 12..100 {
                for scenario in [&rows[..], std::slice::from_ref(&unicode)] {
                    let lines = drawn(scenario, Detail::Collapsed, width);
                    assert_eq!(lines.len(), 1);
                    assert!(lines[0].ends_with(" ▸"), "{locale:?}: {lines:?}");
                    assert!(
                        crate::ui::text::display_width(&lines[0]) <= width,
                        "{locale:?}: {lines:?}"
                    );
                    if width >= 60 {
                        assert!(lines[0].contains("(+2 -1)"), "{locale:?}: {lines:?}");
                    }
                }
            }
        }
        i18n::set(Locale::ZhCn);
        assert_eq!(
            drawn(&rows, Detail::Collapsed, 58),
            ["✓ 编辑了 3 个文件 (+2 -1) · src/main.rs，…另 2 个 ▸"]
        );
        i18n::set(Locale::En);
    }

    #[test]
    fn a_capped_row_ends_on_a_whole_grapheme_within_the_width() {
        let rows = [call(
            "exec",
            json!({ "command": "printf" }),
            Some(&"😀".repeat(200)),
        )];
        for line in drawn(&rows, Detail::Expanded, 20) {
            assert!(crate::ui::text::display_width(&line) <= 20, "{line}");
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
            ["✗ Edited a.ts ▾", "  ✗ Edit a.ts"]
        );
    }

    #[test]
    fn an_exec_row_draws_its_command_and_no_boundary() {
        let rows = [call(
            "exec",
            json!({ "command": "cargo test", "sandbox": "host" }),
            Some("ok"),
        )];
        for detail in [Detail::Expanded, Detail::Collapsed] {
            let text = drawn(&rows, detail, 120).join("\n");
            assert!(!text.contains("boundary"), "{text}");
        }
        let expanded = drawn(&rows, Detail::Expanded, 120).join("\n");
        assert!(expanded.contains("cargo test"), "{expanded}");
    }

    #[test]
    fn a_completed_compaction_draws_only_its_summary_body() {
        let dim_agent_types::MessageContent::Text(production) =
            dim_agent_capabilities::history::summary_message("The cache lock moved into a RwLock.")
                .content
        else {
            panic!("summary_message must produce text");
        };
        for text in [
            production.as_str(),
            "The cache lock moved into a RwLock.",
            "<dim-summary>\nThe cache lock moved into a RwLock.\n</dim-summary>",
            "<dim-summary>\nThe cache lock moved into a RwLock.",
            "<dim-summary>\nThe cache lock moved into a RwLock.\n</dim-summary>\n<dim-digest>\nwrote src/cache.rs\n</dim-digest>",
        ] {
            let mut completed = row(RowKind::Compaction, "");
            completed.compaction = Some(CompactionView {
                state: CompactionState::Completed,
                text: text.into(),
                insufficient: Vec::new(),
            });
            let rows = shared(&[completed]);
            assert_eq!(
                compaction_rows(&rows, Detail::Expanded),
                [
                    "✓ Context compacted ▾",
                    "▎ The cache lock moved into a RwLock.",
                ]
            );
            assert_eq!(
                compaction_rows(&rows, Detail::Collapsed),
                ["✓ Context compacted ▸"]
            );
        }
        for text in [
            "Keep <dim-summary> literal.",
            "End of recorded background. The live conversation governs.",
            "Background from earlier parts of this session, recorded by a summarizer.",
        ] {
            for (content, expected) in [
                (
                    dim_agent_types::MessageContent::Text(text.into()),
                    text.to_owned(),
                ),
                (
                    dim_agent_types::MessageContent::Text(format!(
                        "<dim-summary>\n{text}\n</dim-summary>"
                    )),
                    text.to_owned(),
                ),
                (
                    dim_agent_capabilities::history::summary_message(text).content,
                    text.replace('<', "&lt;").replace('>', "&gt;"),
                ),
            ] {
                let dim_agent_types::MessageContent::Text(content) = content else {
                    panic!("summary_message must produce text");
                };
                let mut completed = row(RowKind::Compaction, "");
                completed.compaction = Some(CompactionView {
                    state: CompactionState::Completed,
                    text: content,
                    insufficient: Vec::new(),
                });
                let rendered = compaction_rows(&shared(&[completed]), Detail::Expanded);
                let body = rendered[1..]
                    .iter()
                    .map(|line| line.trim_start_matches("▎ "))
                    .collect::<Vec<_>>()
                    .join(" ");
                assert_eq!(body, expected);
            }
        }
    }

    /// The rows a compaction row draws at `detail`, without escape sequences.
    fn compaction_rows(rows: &[Arc<ViewRow>], detail: Detail) -> Vec<String> {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let markdown = MarkdownRenderer::new(&theme);
        let block = project_row(rows, 0, detail);
        render_block(&block, 60, &theme.palette, &markdown)
            .iter()
            .map(|row| {
                let mut out = String::new();
                let mut skip = false;
                for c in row.chars() {
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
}
