//! Tool rows of the transcript (port of `blocks/tool.ts`): the outcome mark,
//! the label, a one-line detail, and a small body for the tools whose result
//! is worth reading in place (edit/write diffs, web search hits).

use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;
use crate::ui::transcript::blocks::{display_width, gutter_mark, gutter_width, truncate_cells};
use serde_json::Value;

pub const TOOL_DONE_MARK: &str = "●";
pub const TOOL_FAILED_MARK: &str = "✕";

const DIFF_BODY_LINES: usize = 8;
const WRITE_BODY_LINES: usize = 8;
const WEB_BODY_LINES: usize = 5;
const AGENT_BODY_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Text,
    Muted,
    Success,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLine {
    pub text: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBlock {
    pub mark: &'static str,
    pub label: String,
    pub detail: String,
    pub state: ToolState,
    pub body: Vec<BodyLine>,
}

/// The fields of a tool row the projection needs.
pub struct ToolRow<'a> {
    pub tool_name: &'a str,
    pub input: Option<&'a Value>,
    pub pending: bool,
    pub result_text: Option<&'a str>,
    pub result_is_error: bool,
    pub diff: Option<&'a str>,
}

fn read_string<'a>(input: Option<&'a Value>, key: &str) -> Option<&'a str> {
    input?.get(key)?.as_str().filter(|s| !s.is_empty())
}

fn read_path(input: Option<&Value>) -> String {
    read_string(input, "path")
        .or_else(|| read_string(input, "file_path"))
        .unwrap_or("")
        .to_owned()
}

fn truncate_summary(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = normalized.chars().count();
    if count <= 80 {
        normalized
    } else {
        format!("{}…", normalized.chars().take(79).collect::<String>())
    }
}

/// First string among the well-known input keys, else the first string value.
pub fn summarize_tool_input(input: Option<&Value>) -> String {
    let Some(object) = input.and_then(Value::as_object) else {
        return String::new();
    };
    for key in [
        "command",
        "path",
        "file_path",
        "pattern",
        "url",
        "query",
        "skill_name",
    ] {
        if let Some(value) = object
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            return truncate_summary(value);
        }
    }
    object
        .values()
        .find_map(Value::as_str)
        .map(truncate_summary)
        .unwrap_or_default()
}

fn is_ask_user(name: &str) -> bool {
    name == "ask_user" || name == "askuserquestion"
}

fn is_agent(name: &str) -> bool {
    name == "agent" || name == "subagent" || name.starts_with("agent_")
}

fn display_label(tool_name: &str) -> String {
    let lower = tool_name.to_lowercase();
    match lower.as_str() {
        "exec" | "bash" => "exec".to_owned(),
        "websearch" | "web_search" => "Web search".to_owned(),
        "webfetch" | "web_fetch" => "Web fetch".to_owned(),
        "todowrite" | "todo_write" => "todo".to_owned(),
        n if is_ask_user(n) => "Q/A".to_owned(),
        _ => {
            let mut chars = tool_name.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

struct Change {
    marker: char,
    line: usize,
    text: String,
}

/// Changed lines of a unified diff, each with the file line it lands on.
fn read_diff_changes(diff: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let (mut old_line, mut new_line) = (0usize, 0usize);
    for line in diff.split('\n') {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let numbers: Vec<usize> = rest
                .split([' ', ',', '+'])
                .filter_map(|p| p.parse().ok())
                .collect();
            // `-a[,b] +c[,d]`: old start is first, new start follows the `+`.
            if let Some(after_plus) = rest.split('+').nth(1) {
                new_line = after_plus
                    .split([',', ' '])
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(new_line);
            }
            old_line = numbers.first().copied().unwrap_or(old_line);
            continue;
        }
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            changes.push(Change {
                marker: '+',
                line: new_line,
                text: text.to_owned(),
            });
            new_line += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            changes.push(Change {
                marker: '-',
                line: old_line,
                text: text.to_owned(),
            });
            old_line += 1;
        } else {
            old_line += 1;
            new_line += 1;
        }
    }
    changes
}

fn prefix_lines(value: Option<&str>, prefix: char) -> Vec<String> {
    match value {
        Some(v) if !v.is_empty() => v
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(|line| format!("{prefix}{line}"))
            .collect(),
        _ => Vec::new(),
    }
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!("\"{value}\"")
    }
}

fn parse_glob_files(result: Option<&str>) -> usize {
    let Some(text) = result else { return 0 };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "No files matched the pattern." {
        return 0;
    }
    trimmed
        .split('\n')
        .map(str::trim)
        .enumerate()
        .filter(|(i, line)| !line.is_empty() && !(*i == 0 && line.starts_with("Matched ")))
        .count()
}

fn parse_grep_matches(result: Option<&str>) -> usize {
    let Some(text) = result else { return 0 };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "No matches found." {
        return 0;
    }
    text.split('\n')
        .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
        .count()
}

fn parse_web_results(result: Option<&str>) -> Vec<String> {
    let Some(text) = result else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if let Some(rest) = line.strip_prefix("## ") {
            let rest = rest.trim_start();
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && rest[digits..].starts_with(". ") {
                let rank = &rest[..digits];
                let title = rest[digits + 2..].trim();
                let url = lines.get(index + 1).map(|l| l.trim()).unwrap_or("");
                let place = url
                    .split("://")
                    .nth(1)
                    .unwrap_or(url)
                    .trim_end_matches('/')
                    .to_owned();
                out.push(if place.is_empty() {
                    format!("{rank}. {title}")
                } else {
                    format!("{rank}. {title} — {place}")
                });
            }
        }
        index += 1;
    }
    out
}

/// `header: answer` lines of an AskUser result.
fn parse_answer_map(result: Option<&str>) -> std::collections::HashMap<String, String> {
    let mut answers = std::collections::HashMap::new();
    let Some(text) = result else { return answers };
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if let Some(at) = line.find(": ")
            && at > 0
        {
            answers.insert(
                line[..at].trim().to_owned(),
                line[at + 2..].trim().to_owned(),
            );
        }
    }
    answers
}

fn tail_lines(text: &str, max: usize) -> Vec<String> {
    let lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_owned)
        .collect();
    let start = lines.len().saturating_sub(max);
    lines[start..].to_vec()
}

fn first_lines(text: &str, max: usize) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .take(max)
        .map(str::to_owned)
        .collect()
}

pub fn project_tool(row: &ToolRow<'_>) -> ToolBlock {
    let state = if row.pending {
        ToolState::Running
    } else if row.result_is_error {
        ToolState::Failed
    } else {
        ToolState::Done
    };
    let name = row.tool_name.to_lowercase();
    let mark = if name == "todowrite" || name == "todo_write" || is_ask_user(&name) {
        "◦"
    } else if is_agent(&name) || name == "wait" {
        "⚙"
    } else {
        match state {
            ToolState::Running => "·",
            ToolState::Failed => TOOL_FAILED_MARK,
            ToolState::Done => TOOL_DONE_MARK,
        }
    };
    let (detail, body) = project_content(row, &name);
    ToolBlock {
        mark,
        label: display_label(row.tool_name),
        detail,
        state,
        body,
    }
}

fn project_content(row: &ToolRow<'_>, name: &str) -> (String, Vec<BodyLine>) {
    let input = row.input;
    match name {
        "exec" | "bash" => {
            let command = read_string(input, "command")
                .map(str::to_owned)
                .unwrap_or_else(|| summarize_tool_input(input));
            let background = if input
                .and_then(|i| i.get("runInBackground"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                " (bg)"
            } else {
                ""
            };
            (
                format!("{command}{background}").trim().to_owned(),
                Vec::new(),
            )
        }
        "edit" => {
            let path = read_path(input);
            let real = row.diff.filter(|d| !d.is_empty());
            let numbered = real.is_some();
            let fallback;
            let diff_text = match real {
                Some(diff) => diff,
                None => {
                    let rows: Vec<String> = prefix_lines(read_string(input, "oldText"), '-')
                        .into_iter()
                        .chain(prefix_lines(read_string(input, "newText"), '+'))
                        .collect();
                    let removed = rows.iter().filter(|r| r.starts_with('-')).count();
                    let added = rows.iter().filter(|r| r.starts_with('+')).count();
                    fallback = if rows.is_empty() {
                        String::new()
                    } else {
                        format!("@@ -1,{removed} +1,{added} @@\n{}", rows.join("\n"))
                    };
                    &fallback
                }
            };
            let changes = read_diff_changes(diff_text);
            let shown = &changes[..changes.len().min(DIFF_BODY_LINES)];
            let number_width = if numbered {
                shown
                    .iter()
                    .map(|c| c.line.to_string().len())
                    .max()
                    .unwrap_or(0)
            } else {
                0
            };
            let mut body: Vec<BodyLine> = shown
                .iter()
                .map(|change| BodyLine {
                    text: format!(
                        "{}{} {}",
                        if number_width == 0 {
                            String::new()
                        } else {
                            format!("{:>number_width$} ", change.line)
                        },
                        change.marker,
                        change.text
                    ),
                    tone: if change.marker == '+' {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                })
                .collect();
            let hidden = changes.len() - shown.len();
            if hidden > 0 {
                body.push(BodyLine {
                    text: format!("… +{hidden} lines"),
                    tone: Tone::Muted,
                });
            }
            let added = changes.iter().filter(|c| c.marker == '+').count();
            let removed = changes.len() - added;
            let summary = [
                (added > 0).then(|| format!("+{added}")),
                (removed > 0).then(|| format!("-{removed}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            let detail = [path, summary]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("  ");
            (detail, body)
        }
        "write" => {
            let path = read_path(input);
            let content = read_string(input, "content").unwrap_or("");
            let line_count = content
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .count();
            let window = if row.pending {
                tail_lines(content, WRITE_BODY_LINES)
            } else {
                first_lines(content, WRITE_BODY_LINES)
            };
            let elided = line_count.saturating_sub(WRITE_BODY_LINES);
            let note: Vec<BodyLine> = if elided > 0 {
                vec![BodyLine {
                    text: format!("… +{elided} lines"),
                    tone: Tone::Muted,
                }]
            } else {
                Vec::new()
            };
            let lines: Vec<BodyLine> = window
                .into_iter()
                .map(|line| BodyLine {
                    text: format!("+{line}"),
                    tone: Tone::Success,
                })
                .collect();
            let detail = [
                Some(path).filter(|p| !p.is_empty()),
                (!content.is_empty()).then(|| format!("({line_count} lines)")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            let body = if row.pending {
                note.into_iter().chain(lines).collect()
            } else {
                lines.into_iter().chain(note).collect()
            };
            (detail, body)
        }
        "grep" => {
            let matches = parse_grep_matches(row.result_text);
            let query = read_string(input, "pattern")
                .or_else(|| read_string(input, "query"))
                .unwrap_or("");
            let tail = if matches > 0 {
                format!("  {matches} matches")
            } else {
                String::new()
            };
            (
                format!("{}{tail}", quote(query)).trim().to_owned(),
                Vec::new(),
            )
        }
        "glob" => {
            let files = parse_glob_files(row.result_text);
            let pattern = read_string(input, "pattern").unwrap_or("");
            let tail = if files > 0 {
                format!("  {files} files")
            } else {
                String::new()
            };
            (
                format!("{}{tail}", quote(pattern)).trim().to_owned(),
                Vec::new(),
            )
        }
        "websearch" | "web_search" => (
            summarize_tool_input(input),
            parse_web_results(row.result_text)
                .into_iter()
                .take(WEB_BODY_LINES)
                .map(|text| BodyLine {
                    text,
                    tone: Tone::Muted,
                })
                .collect(),
        ),
        "webfetch" | "web_fetch" => (summarize_tool_input(input), Vec::new()),
        n if is_ask_user(n) => {
            // The record of an AskUser round: the prompt itself commits
            // nothing, so this block carries the question and the decision.
            let answers = parse_answer_map(row.result_text);
            let cancelled = !row.pending && row.result_is_error;
            let questions = input
                .and_then(|i| i.get("questions"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut body = Vec::new();
            for question in &questions {
                let prompt = question
                    .get("question")
                    .and_then(Value::as_str)
                    .or_else(|| question.get("header").and_then(Value::as_str));
                let Some(prompt) = prompt else { continue };
                let header = question
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or(prompt);
                let answer = answers
                    .get(header)
                    .cloned()
                    .or_else(|| cancelled.then(|| "Cancelled by user".to_owned()));
                let text = match answer {
                    Some(answer) => format!("Q: {prompt}  A: {answer}"),
                    None => format!("Q: {prompt}"),
                };
                body.push(BodyLine {
                    text,
                    tone: Tone::Muted,
                });
            }
            (String::new(), body)
        }
        n if is_agent(n) => {
            let agent = read_string(input, "agent")
                .or_else(|| read_string(input, "agentName"))
                .or_else(|| read_string(input, "subagent_type"))
                .unwrap_or(row.tool_name);
            let status = if row.pending {
                " · running"
            } else if row.result_is_error {
                " · failed"
            } else {
                " · done"
            };
            let body = tail_lines(row.result_text.unwrap_or(""), AGENT_BODY_LINES)
                .into_iter()
                .map(|l| l.trim_end().to_owned())
                .filter(|l| !l.is_empty())
                .map(|text| BodyLine {
                    text,
                    tone: Tone::Muted,
                })
                .collect();
            (format!("{agent}{status}"), body)
        }
        _ => (summarize_tool_input(input), Vec::new()),
    }
}

/// The rows of one tool block (gutter mark, label, detail, body), at `width`.
pub fn render_tool(block: &ToolBlock, width: usize, p: &Palette) -> Vec<String> {
    let gutter = gutter_width(width);
    let detail_width = width.saturating_sub(gutter + display_width(&block.label) + 2);
    let failed = block.state == ToolState::Failed;
    let mark_color = if block.mark == TOOL_FAILED_MARK {
        &p.danger
    } else if block.mark == TOOL_DONE_MARK {
        &p.success
    } else {
        &p.muted
    };
    let detail = if block.detail.is_empty() {
        String::new()
    } else {
        fg(
            &format!("  {}", truncate_cells(&block.detail, detail_width)),
            &p.muted,
        )
    };
    let mut rows = vec![format!(
        "{}{}{}",
        fg(&gutter_mark(block.mark, gutter), mark_color),
        fg(&block.label, if failed { &p.danger } else { &p.success }),
        detail
    )];
    for line in &block.body {
        let color = match line.tone {
            Tone::Text => &p.text,
            Tone::Muted => &p.muted,
            Tone::Success => &p.success,
            Tone::Danger => &p.danger,
        };
        let text = truncate_cells(&line.text, width.saturating_sub(gutter).max(1));
        let text = if text.is_empty() {
            " ".to_owned()
        } else {
            text
        };
        rows.push(format!("{}{}", " ".repeat(gutter), fg(&text, color)));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};
    use serde_json::json;

    fn row<'a>(name: &'a str, input: &'a Value) -> ToolRow<'a> {
        ToolRow {
            tool_name: name,
            input: Some(input),
            pending: false,
            result_text: None,
            result_is_error: false,
            diff: None,
        }
    }

    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
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
    fn exec_shows_only_the_command() {
        let input = json!({"command": "cargo test", "runInBackground": true});
        let block = project_tool(&row("exec", &input));
        assert_eq!((block.mark, block.label.as_str()), ("●", "exec"));
        assert_eq!(block.detail, "cargo test (bg)");
        assert!(block.body.is_empty());
    }

    #[test]
    fn running_and_failed_marks() {
        let input = json!({"path": "a.txt"});
        let mut r = row("read", &input);
        r.pending = true;
        assert_eq!(project_tool(&r).mark, "·");
        r.pending = false;
        r.result_is_error = true;
        let block = project_tool(&r);
        assert_eq!((block.mark, block.state), ("✕", ToolState::Failed));
        assert_eq!(block.label, "Read");
    }

    #[test]
    fn edit_without_a_real_diff_is_unnumbered_and_summarized() {
        let input = json!({"path": "src/index.ts", "oldText": "a", "newText": "b\nc"});
        let block = project_tool(&row("edit", &input));
        assert_eq!(block.detail, "src/index.ts  +2 -1");
        let texts: Vec<&str> = block.body.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["- a", "+ b", "+ c"]);
    }

    #[test]
    fn edit_with_a_real_diff_numbers_the_changed_lines() {
        let input = json!({"path": "f"});
        let mut r = row("edit", &input);
        r.diff = Some("@@ -10,2 +10,2 @@\n ctx\n-old\n+new");
        let block = project_tool(&r);
        let texts: Vec<&str> = block.body.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["11 - old", "11 + new"]);
    }

    #[test]
    fn long_diffs_are_capped_with_an_elision_row() {
        let new_text: String = (0..12)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let input = json!({"path": "f", "newText": new_text});
        let block = project_tool(&row("edit", &input));
        assert_eq!(block.body.len(), 9);
        assert_eq!(block.body.last().unwrap().text, "… +4 lines");
    }

    #[test]
    fn grep_and_glob_count_their_results() {
        let input = json!({"pattern": "TODO"});
        let mut r = row("grep", &input);
        r.result_text = Some("src/a.ts\n  3| TODO one\n  9| TODO two\n");
        assert_eq!(project_tool(&r).detail, "\"TODO\"  2 matches");
        let input = json!({"pattern": "*.ts"});
        let mut g = row("glob", &input);
        g.result_text = Some("Matched 2 files\na.ts\nb.ts");
        assert_eq!(project_tool(&g).detail, "\"*.ts\"  2 files");
    }

    #[test]
    fn long_details_are_truncated_to_the_row() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let long = "x".repeat(200);
        let input = json!({"command": long});
        let block = project_tool(&row("exec", &input));
        let rows = plain(&render_tool(&block, 40, &theme.palette));
        assert_eq!(rows.len(), 1);
        assert!(display_width(&rows[0]) <= 40, "{:?}", rows[0]);
        assert!(rows[0].ends_with('…'));
    }

    #[test]
    fn web_fetch_uses_the_scrollback_label() {
        let input = json!({"url": "https://example.com"});
        let block = project_tool(&row("WebFetch", &input));
        assert_eq!(block.label, "Web fetch");
        assert_eq!(block.detail, "https://example.com");
    }
}
