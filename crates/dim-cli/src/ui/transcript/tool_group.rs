//! The tool calls of one model response drawn as one group (port of
//! `blocks/tool-group.ts` and `tool-group-render.ts`).
//!
//! Collapsed, the default, is the summary row alone
//! (`✓ Ran 1 command, ran 1 search, read 1 file · 1 failed`). Expanded (the
//! `/tools` toggle) adds a row per call with the bodies that matter: command
//! output, edit diffs. The live region and the committed block both draw
//! through [`render_group`], so a group that settles lands on exactly the rows
//! it occupied while it ran.

use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::model::view::{MemberCard, MemberConversationState, MemberTarget, TaskKind, ViewRow};
use crate::ui::i18n::{t, tf};
use crate::ui::text::{DONE_MARK, FAILED_MARK, RUNNING_MARK, SHELL_MARK, display_width};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};

/// Source lines of one edit's diff before the rest is counted, not shown.
const DIFF_MAX_LINES: usize = 40;
/// Line numbers take at least this many columns.
const LINE_NUMBER_MIN_WIDTH: usize = 3;
/// Rows one wrapped line may take before the rest is cut.
const WRAP_MAX_ROWS: usize = 4;
/// Screen rows of a command's output: its end, unmarked.
const TAIL_MAX_ROWS: usize = 15;

/// How much of a group is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Collapsed,
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed,
}

/// Roles a span is painted in. Command roles follow a shell highlighter: the
/// program bold, flags and quoted strings coloured, operators dimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Text,
    Muted,
    Success,
    Danger,
    Path,
    Program,
    Flag,
    Quoted,
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    text: String,
    tone: Tone,
}

fn span(text: impl Into<String>, tone: Tone) -> Span {
    let text = text.into();
    Span {
        text: match crate::ui::text::expand_tabs(&text) {
            std::borrow::Cow::Borrowed(_) => text,
            std::borrow::Cow::Owned(expanded) => expanded,
        },
        tone,
    }
}

/// What sits in front of a row; the `*Muted` outcomes are a child call's,
/// recessive under its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Running,
    Done,
    Failed,
    DoneMuted,
    FailedMuted,
    Command,
    CommandFailed,
}

/// `Truncate` keeps one row. `Wrap` continues under the text column and keeps
/// the text's own line breaks, up to [`WRAP_MAX_ROWS`] (`Rows` up to its
/// own count), ending the last in `…`; `Whole` keeps every row, however long the command.
/// `Tail` keeps the last [`TAIL_MAX_ROWS`] rows, as a command's output is
/// shown, under a `… +N lines` row when there are more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overflow {
    Truncate,
    Wrap,
    Rows(usize),
    Whole,
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    /// Columns before the mark (or before the text when there is none).
    indent: usize,
    mark: Option<Mark>,
    spans: Vec<Span>,
    overflow: Overflow,
    /// Wrapped rows start this many columns further in than `indent`.
    hang: usize,
    /// A member card's line: the delegation (request event id) it opens.
    action: Option<String>,
}

fn header_line(indent: usize, mark: Mark, spans: Vec<Span>) -> Line {
    Line {
        indent,
        mark: Some(mark),
        spans,
        overflow: Overflow::Truncate,
        hang: 0,
        action: None,
    }
}

fn body_line(indent: usize, text: impl Into<String>, tone: Tone, overflow: Overflow) -> Line {
    Line {
        indent,
        mark: None,
        spans: vec![span(text, tone)],
        overflow,
        hang: 0,
        action: None,
    }
}

/// One batch of tool calls, projected for drawing. Both projections are kept,
/// so a click flips a committed group without its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolGroup {
    pub state: State,
    pub detail: Detail,
    /// The delegations its member cards show, by request event id.
    pub cards: Vec<String>,
    collapsed: Vec<Line>,
    expanded: Vec<Line>,
}

impl ToolGroup {
    fn lines(&self) -> &[Line] {
        match self.detail {
            Detail::Collapsed => &self.collapsed,
            Detail::Expanded => &self.expanded,
        }
    }
}

impl Detail {
    pub fn flipped(self) -> Self {
        match self {
            Detail::Collapsed => Detail::Expanded,
            Detail::Expanded => Detail::Collapsed,
        }
    }
}

/// The fields of a call row the group reads.
struct Call<'a> {
    name: String,
    input: Option<&'a Value>,
    state: State,
    result: Option<&'a str>,
    diff: Option<&'a str>,
    exit_code: Option<i64>,
    member: Option<&'a MemberCard>,
}

impl<'a> Call<'a> {
    fn of(row: &'a ViewRow) -> Self {
        let state = if row.pending {
            State::Running
        } else if row.result_is_error || row.result_text.is_none() {
            State::Failed
        } else {
            State::Done
        };
        Self {
            name: row.tool_name.as_deref().unwrap_or("tool").to_lowercase(),
            input: row.tool_input.as_ref(),
            state,
            result: row.result_text.as_deref(),
            diff: row.diff.as_deref(),
            exit_code: row.exit_code,
            member: row.member.as_deref(),
        }
    }

    fn string(&self, key: &str) -> Option<&'a str> {
        self.input?.get(key)?.as_str().filter(|s| !s.is_empty())
    }

    fn path(&self) -> &'a str {
        self.string("path")
            .or_else(|| self.string("file_path"))
            .unwrap_or("")
    }

    fn background(&self) -> bool {
        self.input
            .and_then(|i| i.get("runInBackground"))
            .and_then(Value::as_bool)
            == Some(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExploreKind {
    File,
    Page,
    Skill,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Explore,
    Command,
    Edit,
    Check,
    Agent,
    Tool,
}

enum Item<'a> {
    Explore {
        call: Call<'a>,
        kind: ExploreKind,
        label: &'static str,
        detail: String,
    },
    Command {
        call: Call<'a>,
        command: String,
    },
    Edit {
        call: Call<'a>,
        write: bool,
    },
    Check {
        call: Call<'a>,
    },
    /// The `agent` tool: delegating, messaging or looking at a member.
    Agent {
        call: Call<'a>,
        detail: String,
    },
    Tool {
        call: Call<'a>,
    },
}

impl<'a> Item<'a> {
    fn call(&self) -> &Call<'a> {
        match self {
            Item::Explore { call, .. }
            | Item::Command { call, .. }
            | Item::Edit { call, .. }
            | Item::Check { call }
            | Item::Agent { call, .. }
            | Item::Tool { call } => call,
        }
    }

    fn section(&self) -> Section {
        match self {
            Item::Explore { .. } => Section::Explore,
            Item::Command { .. } => Section::Command,
            Item::Edit { .. } => Section::Edit,
            Item::Check { .. } => Section::Check,
            Item::Agent { .. } => Section::Agent,
            Item::Tool { .. } => Section::Tool,
        }
    }

    fn explore_kind(&self) -> Option<ExploreKind> {
        match self {
            Item::Explore { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// A `request` the server refused before a member Session existed.
    fn rejected(&self) -> bool {
        self.call().member.is_some_and(rejected)
    }
}

/// A delegation the server refused before a member Session existed: its
/// delivery carries an error and its target never became a member.
fn rejected(card: &MemberCard) -> bool {
    card.task.finished.is_some() && card.task.kind == TaskKind::Member(MemberTarget::Starting)
}

/// One batch of calls as a group.
pub fn project_group(rows: &[&ViewRow], detail: Detail) -> ToolGroup {
    let items: Vec<Item<'_>> = rows.iter().map(|row| classify(Call::of(row))).collect();
    let states: Vec<State> = items.iter().map(|item| item.call().state).collect();
    let state = group_state(&states);
    let mark = state_mark(state);
    let sections = ordered_sections(&items);
    let header = header_spans(&items, &sections);
    let mut expanded = vec![header_line(0, mark, header.clone())];
    if sections.len() == 1 {
        let members: Vec<&Item<'_>> = items.iter().collect();
        append_section(&mut expanded, sections[0], &members, false);
    } else {
        for run in section_runs(&items) {
            append_section(&mut expanded, run[0].section(), &run, true);
        }
    }
    let mut header = header;
    let started = items.iter().filter(|item| !item.rejected()).count();
    let failed = items
        .iter()
        .filter(|item| !item.rejected() && item.call().state == State::Failed)
        .count();
    if failed > 0 && failed < started {
        header.push(span(
            tf(" · {count} failed", &[("count", &failed.to_string())]),
            Tone::Danger,
        ));
    }
    if let Some(text) = boundary(&items) {
        header.push(span(text, Tone::Muted));
    }
    let mut collapsed = vec![header_line(0, mark, header)];
    let now = now_ms();
    for item in &items {
        if let Item::Agent { call, .. } = item
            && call.member.is_some()
        {
            append_card(&mut collapsed, call, now, Detail::Collapsed);
        }
    }
    ToolGroup {
        state,
        detail,
        cards: items
            .iter()
            .filter_map(|item| Some(item.call().member?.task.id.clone()))
            .collect(),
        collapsed,
        expanded,
    }
}

fn classify(call: Call<'_>) -> Item<'_> {
    let explore = |call, kind, label, detail| Item::Explore {
        call,
        kind,
        label,
        detail,
    };
    match call.name.as_str() {
        "read" => {
            let detail = base_name(call.path()).to_owned();
            explore(call, ExploreKind::File, "Read", detail)
        }
        "glob" => {
            let mut parts = vec![call.string("pattern").unwrap_or("").to_owned()];
            if call.state == State::Done {
                parts.extend(glob_files(call.result).map(str::to_owned));
            }
            explore(call, ExploreKind::Search, "List", parts.join(" "))
        }
        "grep" => {
            let pattern = call
                .string("pattern")
                .or_else(|| call.string("query"))
                .unwrap_or("");
            let first_match = if call.state == State::Done {
                grep_matches(call.result).next().unwrap_or("")
            } else {
                ""
            };
            let detail = [call.path(), &quote(pattern), first_match]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            explore(call, ExploreKind::Search, "Grep", detail)
        }
        "websearch" | "web_search" => {
            let detail = format!("· {}", call.string("query").unwrap_or(""));
            explore(call, ExploreKind::Search, "Search the web", detail)
        }
        "webfetch" | "web_fetch" => {
            let detail = call.string("url").unwrap_or("").to_owned();
            explore(call, ExploreKind::Page, "Read", detail)
        }
        "skill" => {
            let detail = call.string("skill_name").unwrap_or("").to_owned();
            explore(call, ExploreKind::Skill, "Read skill", detail)
        }
        "exec" | "bash" => {
            let command = call.string("command").unwrap_or("").to_owned();
            let output = call.exit_code.filter(|code| *code != 0).and_then(|code| {
                let note = format!("Command exited with code {code}.");
                call.result
                    .map(|text| text.strip_suffix(note.as_str()).unwrap_or(text).trim_end())
            });
            let read_only = if call.background() {
                None
            } else {
                explore_command(&command, output.or(call.result))
            };
            match read_only {
                Some((kind, label, detail)) => {
                    let mut call = call;
                    if output.is_some() {
                        call.state = State::Done;
                        call.result = output;
                    }
                    explore(call, kind, label, detail)
                }
                None => Item::Command { call, command },
            }
        }
        "wait" => Item::Check { call },
        "agent" => {
            let action = call.string("action").unwrap_or("");
            let detail = match action {
                "request" => call.string("prompt"),
                "message" => call.string("content"),
                _ => call.string("agentId"),
            }
            .map_or_else(
                || action.to_owned(),
                |text| {
                    let first = text.lines().next().unwrap_or("");
                    if action.is_empty() {
                        first.to_owned()
                    } else {
                        format!("{action} · {first}")
                    }
                },
            );
            Item::Agent { call, detail }
        }
        "edit" => Item::Edit { call, write: false },
        "write" => Item::Edit { call, write: true },
        _ => Item::Tool { call },
    }
}

/// A plain read-only command reads as the exploring it is, counting
/// `ls`, `wc` and `git log` among the files and searches. Anything with shell
/// operators, redirection, substitution or a second line stays a command:
/// what it does is no longer one program's.
fn explore_command(
    command: &str,
    result: Option<&str>,
) -> Option<(ExploreKind, &'static str, String)> {
    if command.contains(['|', '&', ';', '<', '>', '`', '$', '(', ')', '{', '}', '\n']) {
        return None;
    }
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or("");
    let args: Vec<&str> = words.filter(|word| !word.starts_with('-')).collect();
    match program {
        "ls" | "tree" | "find" | "fd" => {
            let listed = output_lines(result);
            let detail = args
                .iter()
                .copied()
                .chain(listed.iter().map(String::as_str).filter(|l| !l.is_empty()))
                .collect::<Vec<_>>()
                .join(" ");
            Some((ExploreKind::Search, "List", detail))
        }
        "cat" | "head" | "tail" | "wc" | "stat" | "file" => {
            let names = args.iter().map(|a| base_name(a)).collect::<Vec<_>>();
            let detail = if names.is_empty() {
                ".".to_owned()
            } else {
                names.join(" ")
            };
            Some((ExploreKind::File, "Read", detail))
        }
        "rg" | "grep" => Some((ExploreKind::Search, "Grep", args.join(" "))),
        "git" if matches!(args.first(), Some(&("log" | "show" | "blame"))) => Some((
            ExploreKind::Search,
            "Search git history",
            first_line(result),
        )),
        _ => None,
    }
}

fn header_spans(items: &[Item<'_>], sections: &[Section]) -> Vec<Span> {
    if let [section] = sections {
        let members: Vec<&Item<'_>> = items.iter().collect();
        return match section {
            Section::Explore => explore_header(&members),
            Section::Edit => edit_header(&members),
            _ => vec![span(
                capitalize(&phrase(phrase_key(&items[0]), &members)),
                Tone::Text,
            )],
        };
    }
    let mut keys: Vec<PhraseKey> = Vec::new();
    for item in items {
        let key = phrase_key(item);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let phrases: Vec<String> = keys
        .iter()
        .map(|key| {
            let members: Vec<&Item<'_>> = items.iter().filter(|i| phrase_key(i) == *key).collect();
            phrase(*key, &members)
        })
        .collect();
    vec![span(capitalize(&phrases.join(t(", "))), Tone::Text)]
}

/// The tiers the group's calls asked for, in widening order, when any did. A
/// collapsed group draws no command rows, so its header carries the boundary
/// instead.
fn boundary(items: &[Item<'_>]) -> Option<String> {
    let tiers: Vec<&str> = ["workspace", "network", "host"]
        .into_iter()
        .filter(|tier| {
            items
                .iter()
                .any(|item| item.call().string("sandbox") == Some(*tier))
        })
        .collect();
    if tiers.is_empty() {
        return None;
    }
    Some(tf(
        " (boundary: {sandbox})",
        &[("sandbox", &tiers.join(", "))],
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhraseKey {
    Search,
    Explore,
    Skill,
    Command,
    Edit,
    Check,
    Agent,
    Tool,
}

fn phrase_key(item: &Item<'_>) -> PhraseKey {
    match item {
        Item::Explore { kind, .. } => match kind {
            ExploreKind::Search => PhraseKey::Search,
            ExploreKind::Skill => PhraseKey::Skill,
            ExploreKind::File | ExploreKind::Page => PhraseKey::Explore,
        },
        Item::Command { .. } => PhraseKey::Command,
        Item::Edit { .. } => PhraseKey::Edit,
        Item::Check { .. } => PhraseKey::Check,
        Item::Agent { .. } => PhraseKey::Agent,
        Item::Tool { .. } => PhraseKey::Tool,
    }
}

fn running(members: &[&Item<'_>]) -> bool {
    members
        .iter()
        .any(|item| item.call().state == State::Running)
}

/// `ran 2 commands`: the verb in the progressive while any member runs.
fn phrase(key: PhraseKey, members: &[&Item<'_>]) -> String {
    let verb = |past: &'static str, progressive: &'static str| {
        t(if running(members) { progressive } else { past })
    };
    let n = members.len();
    match key {
        PhraseKey::Search => format!("{} {}", verb("ran", "running"), counted(n, SEARCH)),
        PhraseKey::Skill => format!("{} {}", verb("read", "reading"), counted(n, SKILL)),
        PhraseKey::Explore => {
            let files = members
                .iter()
                .filter(|item| item.explore_kind() == Some(ExploreKind::File))
                .count();
            let pages = n - files;
            let items = match (files, pages) {
                (0, pages) => counted(pages, WEB_PAGE),
                (files, 0) => counted(files, FILE),
                (files, pages) => tf(
                    "{files} and {pages}",
                    &[
                        ("files", &counted(files, FILE)),
                        ("pages", &counted(pages, WEB_PAGE)),
                    ],
                ),
            };
            let verb = if pages == 0 {
                verb("read", "reading")
            } else {
                verb("explored", "exploring")
            };
            format!("{verb} {items}")
        }
        PhraseKey::Command => format!("{} {}", verb("ran", "running"), counted(n, COMMAND)),
        PhraseKey::Edit => {
            let verb = if all_edits(members) {
                verb("edited", "editing")
            } else {
                verb("changed", "changing")
            };
            format!("{verb} {}", counted(edited_paths(members), FILE))
        }
        PhraseKey::Check => format!("{} {}", verb("checked on", "checking on"), counted(n, TASK)),
        PhraseKey::Agent => {
            let rejected = members.iter().filter(|item| item.rejected()).count();
            let started = n - rejected;
            if started == 0 {
                return counted(rejected, DELEGATION);
            }
            let mut phrase = format!(
                "{} {}",
                verb("worked with", "working with"),
                counted(started, AGENT)
            );
            if rejected > 0 {
                phrase.push_str(&tf(
                    " · {count} rejected",
                    &[("count", &rejected.to_string())],
                ));
            }
            phrase
        }
        PhraseKey::Tool => format!("{} {}", verb("ran", "running"), counted(n, TOOL)),
    }
}

/// `Explored 2 files, 2 searches`: the verb, then the counts muted.
fn explore_header(members: &[&Item<'_>]) -> Vec<Span> {
    let count = |kind| {
        members
            .iter()
            .filter(|item| item.explore_kind() == Some(kind))
            .count()
    };
    let parts: Vec<String> = [
        (ExploreKind::File, FILE),
        (ExploreKind::Page, WEB_PAGE),
        (ExploreKind::Skill, SKILL),
        (ExploreKind::Search, SEARCH),
    ]
    .into_iter()
    .filter(|(kind, _)| count(*kind) > 0)
    .map(|(kind, noun)| counted(count(kind), noun))
    .collect();
    let verb = if running(members) {
        "Exploring"
    } else {
        "Explored"
    };
    vec![
        span(t(verb), Tone::Text),
        span(format!(" {}", parts.join(t(", "))), Tone::Muted),
    ]
}

/// `Edited 1 file (+1 -1)`, or `Changed` once a write is among them.
fn edit_header(members: &[&Item<'_>]) -> Vec<Span> {
    let verb = match (all_edits(members), running(members)) {
        (true, false) => "Edited",
        (true, true) => "Editing",
        (false, false) => "Changed",
        (false, true) => "Changing",
    };
    let (mut added, mut removed) = (0, 0);
    for item in members {
        if let Item::Edit { call, write } = item
            && call.state != State::Failed
        {
            let (a, r) = diff_counts(&edit_lines(call, *write));
            added += a;
            removed += r;
        }
    }
    let mut spans = vec![span(
        format!("{} {}", t(verb), counted(edited_paths(members), FILE)),
        Tone::Text,
    )];
    let summary = format_counts(added, removed);
    if !summary.is_empty() {
        spans.push(span(" (", Tone::Muted));
        spans.push(span(summary, Tone::Text));
        spans.push(span(")", Tone::Muted));
    }
    spans
}

fn append_section(lines: &mut Vec<Line>, section: Section, items: &[&Item<'_>], nested: bool) {
    let members: Vec<&Item<'_>> = items
        .iter()
        .copied()
        .filter(|item| item.section() == section)
        .collect();
    match section {
        Section::Explore => {
            if nested {
                let states: Vec<State> = members.iter().map(|i| i.call().state).collect();
                lines.push(header_line(
                    2,
                    state_mark(group_state(&states)),
                    explore_header(&members),
                ));
            }
            for item in &members {
                let Item::Explore {
                    call,
                    label,
                    detail,
                    ..
                } = item
                else {
                    continue;
                };
                let text = [t(label), detail.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut spans = vec![span(text, Tone::Muted)];
                if let Some(sandbox) = call.string("sandbox") {
                    spans.push(span(
                        tf(" (boundary: {sandbox})", &[("sandbox", sandbox)]),
                        Tone::Muted,
                    ));
                }
                lines.push(Line {
                    indent: if nested { 3 } else { 1 },
                    mark: Some(match call.state {
                        State::Running => Mark::Running,
                        State::Failed => Mark::FailedMuted,
                        State::Done => Mark::DoneMuted,
                    }),
                    spans,
                    overflow: Overflow::Truncate,
                    hang: 0,
                    action: None,
                });
            }
        }
        Section::Command => {
            for item in &members {
                if let Item::Command { call, command } = item {
                    append_command(lines, call, command);
                }
            }
        }
        Section::Edit => {
            let single = members.len() == 1;
            for item in &members {
                if let Item::Edit { call, write } = item {
                    append_edit(lines, call, *write, single);
                }
            }
        }
        Section::Check => {
            for item in &members {
                append_check(lines, item.call());
            }
        }
        Section::Agent => {
            for item in &members {
                if let Item::Agent { call, detail } = item {
                    append_agent(lines, call, detail);
                }
            }
        }
        Section::Tool => {
            for item in &members {
                append_tool(lines, item.call());
            }
        }
    }
}

/// A call to the team: the action and its words; a failure's first line.
/// A request that started a delegation is drawn as its card.
fn append_agent(lines: &mut Vec<Line>, call: &Call<'_>, detail: &str) {
    if call.member.is_some() {
        append_card(lines, call, now_ms(), Detail::Expanded);
        return;
    }
    let mut spans = vec![span(t("Agent"), Tone::Text)];
    if !detail.is_empty() {
        spans.push(span(format!(" · {detail}"), Tone::Muted));
    }
    lines.push(header_line(2, state_mark(call.state), spans));
    if call.state == State::Failed {
        append_error(lines, call, 4);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// `<1s`, `42s`, `3m07s`.
fn elapsed_text(ms: u64) -> String {
    match ms {
        0..1000 => "<1s".to_owned(),
        1000..60_000 => format!("{}s", ms / 1000),
        _ => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// A delegation card: who took the task, the task, the member's observed
/// model and activity, the delegation's own state and time, and its result —
/// or, for a request the server refused before a member existed, that
/// refusal's own text as the reason. Externally supplied text is sanitized
/// before it is styled.
fn append_card(lines: &mut Vec<Line>, call: &Call<'_>, now: u64, detail: Detail) {
    use crate::sanitize::safe;
    let Some(card) = call.member else {
        return;
    };
    let refused = rejected(card);
    let task = &card.task;
    let (status, state) = match &task.finished {
        Some(_) if refused => (t("Not started"), State::Failed),
        Some(finish) if finish.failed => (t(finish.status), State::Failed),
        Some(finish) => (t(finish.status), State::Done),
        None if task.kind == TaskKind::Member(MemberTarget::Starting) => {
            (t("Starting"), State::Running)
        }
        None => (t("Running"), State::Running),
    };
    let elapsed = elapsed_text(
        task.finished
            .as_ref()
            .map_or(now, |finish| finish.at)
            .saturating_sub(task.started_at),
    );
    let provider = task.kind == TaskKind::Member(MemberTarget::Provider);
    let action =
        matches!(task.kind, TaskKind::Member(MemberTarget::Session(_))).then(|| task.id.clone());
    let who = if provider {
        tf("Provider member: {name}", &[("name", &safe(&card.agent))])
    } else {
        tf("Agent: {name}", &[("name", &safe(&card.agent))])
    };
    let model = if provider || refused {
        None
    } else {
        let mut model = card
            .model
            .as_deref()
            .map_or_else(|| t("No model observed").to_owned(), safe);
        match &card.observation {
            Some(MemberConversationState::Loading) if card.model.is_none() => {
                model = t("Observing member...").to_owned();
            }
            Some(MemberConversationState::ReadError(_)) => {
                model = tf("{model} (observation unavailable)", &[("model", &model)]);
            }
            Some(MemberConversationState::Closed) => {
                model = tf("{model} (Session closed)", &[("model", &model)]);
            }
            _ if task.finished.is_some() && card.model.is_some() => {
                model = tf("{model} (last observed)", &[("model", &model)]);
            }
            _ => {}
        }
        Some(model)
    };
    let meta = match &model {
        Some(model) => format!("{model} · {status} · {elapsed}"),
        None => format!("{status} · {elapsed}"),
    };
    let first = |text: &str| {
        safe(
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or(""),
        )
    };
    let mark = state_mark(state);
    let line = |text: String, tone: Tone, overflow: Overflow| Line {
        action: action.clone(),
        ..body_line(4, text, tone, overflow)
    };
    if detail == Detail::Collapsed {
        let mut spans = vec![span(who, Tone::Text)];
        let label = first(&card.prompt);
        if !label.is_empty() {
            spans.push(span(format!(" · {label}"), Tone::Muted));
        }
        lines.push(Line {
            action: action.clone(),
            ..header_line(2, mark, spans)
        });
        lines.push(line(meta, Tone::Muted, Overflow::Truncate));
        return;
    }
    lines.push(Line {
        action: action.clone(),
        ..header_line(2, mark, vec![span(who, Tone::Text)])
    });
    lines.push(Line {
        hang: 6,
        ..line(
            tf("Task: {task}", &[("task", safe(&card.prompt).trim())]),
            Tone::Text,
            Overflow::Rows(3),
        )
    });
    lines.push(line(meta, Tone::Muted, Overflow::Truncate));
    let activity = if provider {
        Some(t("Activity: not observed on this attachment").to_owned())
    } else {
        card.activity
            .as_deref()
            .map(|activity| tf("Activity: {activity}", &[("activity", &first(activity))]))
    };
    if let Some(activity) = activity {
        lines.push(line(activity, Tone::Muted, Overflow::Truncate));
    }
    if let Some(finish) = &task.finished {
        let tone = if finish.failed {
            Tone::Danger
        } else {
            Tone::Text
        };
        let summary = if refused {
            let reason = call.result.unwrap_or(&finish.text);
            tf("Reason: {reason}", &[("reason", safe(reason).trim())])
        } else {
            tf("Result: {result}", &[("result", safe(&finish.text).trim())])
        };
        lines.push(Line {
            hang: 8,
            ..line(summary, tone, Overflow::Rows(6))
        });
    }
    lines.push(if provider {
        line(
            t("Controlled by the leader").to_owned(),
            Tone::Muted,
            Overflow::Truncate,
        )
    } else if action.is_some() {
        line(
            t("› Open member conversation").to_owned(),
            Tone::Path,
            Overflow::Truncate,
        )
    } else {
        return;
    });
}

fn append_command(lines: &mut Vec<Line>, call: &Call<'_>, command: &str) {
    let mut spans = highlight_command(command);
    if let Some(sandbox) = call.string("sandbox") {
        spans.push(span(
            tf(" (boundary: {sandbox})", &[("sandbox", sandbox)]),
            Tone::Muted,
        ));
    }
    if call.background() {
        spans.push(span(t(" (detached)"), Tone::Muted));
    }
    lines.push(Line {
        indent: 2,
        mark: Some(match call.state {
            State::Running => Mark::Running,
            State::Failed => Mark::CommandFailed,
            State::Done => Mark::Command,
        }),
        spans,
        overflow: Overflow::Whole,
        hang: 0,
        action: None,
    });
    if call.state == State::Running || (call.background() && call.state == State::Done) {
        return;
    }
    let output = output_lines(call.result);
    if !output.is_empty() {
        let tone = if call.state == State::Failed {
            Tone::Danger
        } else {
            Tone::Muted
        };
        lines.push(body_line(4, output.join("\n"), tone, Overflow::Tail));
    }
}

fn append_edit(lines: &mut Vec<Line>, call: &Call<'_>, write: bool, single: bool) {
    let diff = if call.state == State::Failed {
        Vec::new()
    } else {
        edit_lines(call, write)
    };
    let (added, removed) = diff_counts(&diff);
    let label = match (write, removed) {
        (true, 0) => "Create",
        (true, _) => "Write",
        _ if single && call.state == State::Done => "Edited",
        _ => "Edit",
    };
    let mut spans = vec![
        span(format!("{} ", t(label)), Tone::Text),
        span(call.path(), Tone::Path),
    ];
    if added > 0 {
        spans.push(span(format!(" +{added}"), Tone::Success));
    }
    if removed > 0 {
        spans.push(span(format!(" -{removed}"), Tone::Danger));
    }
    lines.push(header_line(2, state_mark(call.state), spans));
    match call.state {
        State::Failed => append_error(lines, call, 4),
        State::Running => {}
        State::Done => append_diff(lines, &diff),
    }
}

/// `Checked on background task` and how the wait ended.
fn append_check(lines: &mut Vec<Line>, call: &Call<'_>) {
    lines.push(header_line(
        2,
        state_mark(call.state),
        vec![span(t("Checked on background task"), Tone::Text)],
    ));
    let body = if call.state == State::Running {
        t("Waiting…").to_owned()
    } else {
        first_line(call.result)
    };
    if !body.is_empty() {
        lines.push(body_line(4, body, Tone::Muted, Overflow::Truncate));
    }
}

/// A tool without a phrase of its own: its label and first argument.
fn append_tool(lines: &mut Vec<Line>, call: &Call<'_>) {
    let detail = summarize_input(call.input);
    let mut spans = vec![span(capitalize(&call.name), Tone::Text)];
    if !detail.is_empty() {
        spans.push(span(format!(" · {detail}"), Tone::Muted));
    }
    lines.push(header_line(2, state_mark(call.state), spans));
    if call.state == State::Failed {
        append_error(lines, call, 4);
    }
}

/// The failure's first line under the call. A call the run ended on top of
/// has no result to quote; its `✗` says enough.
fn append_error(lines: &mut Vec<Line>, call: &Call<'_>, indent: usize) {
    let text = first_line(call.result);
    if !text.is_empty() {
        lines.push(body_line(indent, text, Tone::Danger, Overflow::Truncate));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffLine {
    /// The unchanged stretch a hunk skips.
    Gap,
    Line {
        marker: char,
        /// Old file line for `-`, new file line for `+` and context; 0 when
        /// the change was rebuilt from the input and has no real numbers.
        line: usize,
        text: String,
    },
}

/// The diff as printed: `...` for each skipped stretch, then every hunk
/// line behind its file line number, changes coloured whole, context plain.
fn append_diff(lines: &mut Vec<Line>, diff: &[DiffLine]) {
    let shown = if diff.len() <= DIFF_MAX_LINES + 1 {
        diff
    } else {
        &diff[..DIFF_MAX_LINES]
    };
    let width = shown
        .iter()
        .filter_map(|line| match line {
            DiffLine::Line { line, .. } if *line > 0 => Some(line.to_string().len()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .max(LINE_NUMBER_MIN_WIDTH);
    for line in shown {
        let DiffLine::Line { marker, line, text } = line else {
            lines.push(body_line(4, "...", Tone::Muted, Overflow::Truncate));
            continue;
        };
        let number = if *line > 0 {
            format!("{line:>width$}")
        } else {
            " ".repeat(width)
        };
        let tone = match marker {
            '+' => Tone::Success,
            '-' => Tone::Danger,
            _ => Tone::Text,
        };
        lines.push(Line {
            indent: 4,
            mark: None,
            spans: vec![
                span(format!("{number} "), Tone::Muted),
                span(format!("{marker}  {text}"), tone),
            ],
            overflow: Overflow::Wrap,
            hang: width + 4,
            action: None,
        });
    }
    let hidden = diff.len() - shown.len();
    if hidden > 0 {
        lines.push(body_line(
            4,
            tf("… +{count} lines", &[("count", &hidden.to_string())]),
            Tone::Muted,
            Overflow::Truncate,
        ));
    }
}

/// The real diff carries file line numbers and context. Without it — a call
/// that has not finished — the change is rebuilt from the input with no
/// numbers rather than numbers the file does not have.
fn edit_lines(call: &Call<'_>, write: bool) -> Vec<DiffLine> {
    if write {
        let Some(content) = call.string("content") else {
            return Vec::new();
        };
        let content = normalize(content);
        let body = content.strip_suffix('\n').unwrap_or(&content);
        return std::iter::once(DiffLine::Gap)
            .chain(
                body.split('\n')
                    .enumerate()
                    .map(|(index, text)| DiffLine::Line {
                        marker: '+',
                        line: index + 1,
                        text: text.to_owned(),
                    }),
            )
            .collect();
    }
    if let Some(diff) = call.diff.filter(|d| !d.is_empty()) {
        return read_diff_lines(diff);
    }
    let changed: Vec<String> = prefix_lines(call.string("oldText"), '-')
        .chain(prefix_lines(call.string("newText"), '+'))
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    let removed = changed.iter().filter(|l| l.starts_with('-')).count();
    let added = changed.len() - removed;
    let fallback = format!("@@ -1,{removed} +1,{added} @@\n{}", changed.join("\n"));
    read_diff_lines(&fallback)
        .into_iter()
        .map(|line| match line {
            DiffLine::Line { marker, text, .. } => DiffLine::Line {
                marker,
                line: 0,
                text,
            },
            gap => gap,
        })
        .collect()
}

/// Every hunk line, context included, each behind its file line number; a gap
/// opens every hunk. Only hunk bodies are read, so `---`/`+++` inside one are
/// content.
fn read_diff_lines(diff: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut numbers: Option<(usize, usize)> = None;
    for line in diff.split('\n') {
        if let Some(header) = hunk_header(line) {
            numbers = Some(header);
            lines.push(DiffLine::Gap);
            continue;
        }
        let Some((old, new)) = numbers.as_mut() else {
            continue;
        };
        if line.is_empty() || line.starts_with('\\') {
            continue;
        }
        let (marker, number) = match line.as_bytes()[0] {
            b'+' => ('+', post_increment(new)),
            b'-' => ('-', post_increment(old)),
            _ => {
                *old += 1;
                (' ', post_increment(new))
            }
        };
        lines.push(DiffLine::Line {
            marker,
            line: number,
            text: line[1..].to_owned(),
        });
    }
    lines
}

fn post_increment(value: &mut usize) -> usize {
    *value += 1;
    *value - 1
}

/// `@@ -a[,b] +c[,d] @@`: the old and new start lines.
fn hunk_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let start = |range: &str| range.split(',').next()?.parse().ok();
    Some((start(old)?, start(new)?))
}

fn diff_counts(lines: &[DiffLine]) -> (usize, usize) {
    lines
        .iter()
        .fold((0, 0), |(added, removed), line| match line {
            DiffLine::Line { marker: '+', .. } => (added + 1, removed),
            DiffLine::Line { marker: '-', .. } => (added, removed + 1),
            _ => (added, removed),
        })
}

fn format_counts(added: usize, removed: usize) -> String {
    [
        (added > 0).then(|| format!("+{added}")),
        (removed > 0).then(|| format!("-{removed}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn all_edits(members: &[&Item<'_>]) -> bool {
    members
        .iter()
        .all(|item| matches!(item, Item::Edit { write: false, .. }))
}

fn edited_paths(members: &[&Item<'_>]) -> usize {
    let mut paths: Vec<&str> = members.iter().map(|item| item.call().path()).collect();
    paths.sort_unstable();
    paths.dedup();
    paths.len()
}

fn group_state(states: &[State]) -> State {
    if states.contains(&State::Running) {
        State::Running
    } else if states.iter().all(|s| *s == State::Failed) {
        State::Failed
    } else {
        State::Done
    }
}

fn state_mark(state: State) -> Mark {
    match state {
        State::Running => Mark::Running,
        State::Done => Mark::Done,
        State::Failed => Mark::Failed,
    }
}

fn ordered_sections(items: &[Item<'_>]) -> Vec<Section> {
    let mut sections = Vec::new();
    for item in items {
        if !sections.contains(&item.section()) {
            sections.push(item.section());
        }
    }
    sections
}

fn section_runs<'b, 'a>(items: &'b [Item<'a>]) -> Vec<Vec<&'b Item<'a>>> {
    let mut runs: Vec<Vec<&Item<'_>>> = Vec::new();
    for item in items {
        match runs.last_mut() {
            Some(run) if run[0].section() == item.section() => run.push(item),
            _ => runs.push(vec![item]),
        }
    }
    runs
}

/// A counted noun's `(one, other)` forms, English keys of [`t`].
type Noun = (&'static str, &'static str);

const FILE: Noun = ("1 file", "{count} files");
const WEB_PAGE: Noun = ("1 web page", "{count} web pages");
const SKILL: Noun = ("1 skill", "{count} skills");
const SEARCH: Noun = ("1 search", "{count} searches");
const COMMAND: Noun = ("1 command", "{count} commands");
const TASK: Noun = ("1 task", "{count} tasks");
const TOOL: Noun = ("1 tool", "{count} tools");
const AGENT: Noun = ("1 agent", "{count} agents");
const DELEGATION: Noun = ("1 delegation rejected", "{count} delegations rejected");

fn counted(count: usize, (one, other): Noun) -> String {
    if count == 1 {
        t(one).to_owned()
    } else {
        tf(other, &[("count", &count.to_string())])
    }
}

/// Shell words in the roles a highlighter gives them: the program after every
/// operator bold, flags and quoted strings coloured, operators recessive.
/// Every character of the command lands in some span.
fn highlight_command(command: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut expect_program = true;
    let mut rest = command;
    while let Some(c) = rest.chars().next() {
        let take = |len: usize| (&rest[..len], &rest[len..]);
        let (text, tone, next) = if c.is_whitespace() {
            let len = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            let (text, next) = take(len);
            (text, Tone::Text, next)
        } else if c == '"' || c == '\'' {
            let mut end = rest.len();
            let mut escaped = false;
            for (i, ch) in rest.char_indices().skip(1) {
                if c == '"' && !escaped && ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == c && !escaped {
                    end = i + 1;
                    break;
                }
                escaped = false;
            }
            let (text, next) = take(end);
            expect_program = false;
            (text, Tone::Quoted, next)
        } else if let Some(len) = operator_len(rest) {
            let (text, next) = take(len);
            expect_program = !text.contains(['<', '>']);
            (text, Tone::Operator, next)
        } else {
            let len = rest
                .find(|c: char| c.is_whitespace() || "\"'|;&<>".contains(c))
                .unwrap_or(rest.len())
                .max(c.len_utf8());
            let (text, next) = take(len);
            let tone = if expect_program && !text.contains('=') {
                expect_program = false;
                Tone::Program
            } else if text.starts_with('-') {
                Tone::Flag
            } else {
                Tone::Text
            };
            (text, tone, next)
        };
        spans.push(span(text, tone));
        rest = next;
    }
    spans
}

/// Length of the shell operator `text` starts with: `2>&1`, `&&`, `||`, `|`,
/// `;`, `&`, `>`, `>>`, `2>`, `<`.
fn operator_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let digit = bytes[0].is_ascii_digit();
    let at = usize::from(digit);
    match (bytes.get(at), bytes.get(at + 1), bytes.get(at + 2)) {
        (Some(b'>'), Some(b'&'), Some(d)) if d.is_ascii_digit() => Some(at + 3),
        (Some(b'>'), Some(b'>'), _) => Some(at + 2),
        (Some(b'>'), _, _) => Some(at + 1),
        _ if digit => None,
        (Some(b'&'), Some(b'&'), _) | (Some(b'|'), Some(b'|'), _) => Some(2),
        (Some(b'|' | b';' | b'&' | b'<'), _, _) => Some(1),
        _ => None,
    }
}

fn normalize(text: &str) -> String {
    crate::sanitize::safe(&text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// The text's lines, trailing blanks trimmed and blank lines at either end
/// dropped.
fn output_lines(text: Option<&str>) -> Vec<String> {
    let Some(text) = text else {
        return Vec::new();
    };
    let mut lines: Vec<String> = normalize(text)
        .split('\n')
        .map(|line| line.trim_end().to_owned())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let leading = lines.iter().take_while(|line| line.is_empty()).count();
    lines.drain(..leading);
    lines
}

fn first_line(text: Option<&str>) -> String {
    output_lines(text).into_iter().next().unwrap_or_default()
}

fn glob_files(result: Option<&str>) -> impl Iterator<Item = &str> {
    let text = result.unwrap_or("").trim();
    let none = text.is_empty() || text == "No files matched the pattern.";
    text.split('\n')
        .map(str::trim)
        .enumerate()
        .filter(move |(i, line)| {
            !none && !line.is_empty() && !(*i == 0 && line.starts_with("Matched "))
        })
        .map(|(_, line)| line)
}

fn grep_matches(result: Option<&str>) -> impl Iterator<Item = &str> {
    result
        .unwrap_or("")
        .split('\n')
        .filter(|line| line.starts_with("  ") && !line.trim().is_empty())
        .map(str::trim)
}

fn prefix_lines(value: Option<&str>, prefix: char) -> impl Iterator<Item = String> {
    let lines: Vec<String> = match value {
        Some(value) => normalize(value)
            .split('\n')
            .map(|line| format!("{prefix}{line}"))
            .collect(),
        None => Vec::new(),
    };
    lines.into_iter()
}

/// First string among the well-known input keys, else the first string
/// value, whitespace collapsed.
fn summarize_input(input: Option<&Value>) -> String {
    let Some(object) = input.and_then(Value::as_object) else {
        return String::new();
    };
    let collapse = |value: &str| value.split_whitespace().collect::<Vec<_>>().join(" ");
    [
        "command",
        "path",
        "file_path",
        "pattern",
        "url",
        "query",
        "skill_name",
        "action",
    ]
    .iter()
    .find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    })
    .or_else(|| object.values().find_map(Value::as_str))
    .map(collapse)
    .unwrap_or_default()
}

fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!("\"{value}\"")
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The group's rows at `width`. `spinner` is the glyph of running marks.
///
/// The header ends in `▸` (collapsed) or `▾` (expanded), and so
/// does every expanded row with rows of its own under it (a section, a
/// command with output, an edit with its diff). The marker survives the
/// header's truncation.
pub fn render_group(group: &ToolGroup, width: usize, p: &Palette, spinner: &str) -> Vec<String> {
    lay_out_group(group, width, p, spinner).0
}

/// The delegation a row of the drawn group opens: a member card's line.
pub fn action_at<'a>(
    group: &'a ToolGroup,
    width: usize,
    p: &Palette,
    row: usize,
) -> Option<&'a str> {
    let (rows, starts) = lay_out_group(group, width, p, RUNNING_MARK);
    if row >= rows.len() {
        return None;
    }
    let line = starts
        .partition_point(|start| *start <= row)
        .checked_sub(1)?;
    group.lines()[line].action.as_deref()
}

/// The Session cards a keyboard pick can stop on, in drawing order.
pub fn card_actions(group: &ToolGroup) -> Vec<&str> {
    let mut actions: Vec<&str> = Vec::new();
    for action in group
        .lines()
        .iter()
        .filter_map(|line| line.action.as_deref())
    {
        if actions.last() != Some(&action) {
            actions.push(action);
        }
    }
    actions
}

/// Rows `[start, end)` of the drawn group that a card's lines take.
pub fn card_rows(
    group: &ToolGroup,
    width: usize,
    p: &Palette,
    action: &str,
) -> Option<(usize, usize)> {
    let (rows, starts) = lay_out_group(group, width, p, RUNNING_MARK);
    let lines = group.lines();
    let first = lines
        .iter()
        .position(|line| line.action.as_deref() == Some(action))?;
    let end = lines[first + 1..]
        .iter()
        .position(|line| line.mark.is_some() && line.indent <= lines[first].indent)
        .map_or(rows.len(), |offset| starts[first + 1 + offset]);
    Some((starts[first], end))
}

/// The group's rows and the first row of each of its lines.
fn lay_out_group(
    group: &ToolGroup,
    width: usize,
    p: &Palette,
    spinner: &str,
) -> (Vec<String>, Vec<usize>) {
    let lines = group.lines();
    let mut rows = Vec::new();
    let mut starts = Vec::with_capacity(lines.len());
    for (at, line) in lines.iter().enumerate() {
        starts.push(rows.len());
        let disclosure = if at == 0 {
            Some(match group.detail {
                Detail::Collapsed => "▸",
                Detail::Expanded => "▾",
            })
        } else {
            (line.mark.is_some()
                && lines
                    .get(at + 1)
                    .is_some_and(|next| next.indent > line.indent))
            .then_some("▾")
        };
        let mark = line.mark.map(|mark| mark_text(mark, spinner, p));
        let lead = line.indent + if mark.is_some() { 2 } else { 0 };
        let reserve = if disclosure.is_some() { 2 } else { 0 };
        let rest_indent = line.indent + line.hang;
        let laid = layout(
            &line.spans,
            width.saturating_sub(lead + reserve).max(1),
            width.saturating_sub(rest_indent + reserve).max(1),
            line.overflow,
        );
        if line.overflow == Overflow::Tail {
            let hidden = hidden_lines(&line.spans, width.saturating_sub(lead).max(1));
            if hidden > 0 {
                let note = tf("… +{count} lines", &[("count", &hidden.to_string())]);
                rows.push(format!("{}{}", " ".repeat(lead), fg(&note, &p.muted)));
            }
        }
        let last = laid.len().saturating_sub(1);
        for (index, cells) in laid.iter().enumerate() {
            let disclosure = match disclosure {
                Some(glyph) if index == last => format!(" {}", fg(glyph, &p.text)),
                _ => String::new(),
            };
            let prefix = if index == 0 {
                match &mark {
                    Some(mark) => format!("{}{mark} ", " ".repeat(line.indent)),
                    None => " ".repeat(line.indent),
                }
            } else {
                " ".repeat(rest_indent)
            };
            rows.push(format!("{prefix}{}{disclosure}", paint_cells(cells, p)));
        }
    }
    (rows, starts)
}

fn mark_text(mark: Mark, spinner: &str, p: &Palette) -> String {
    let (glyph, color, bold) = match mark {
        Mark::Running => (spinner, &p.accent, false),
        Mark::Done => (DONE_MARK, &p.success, false),
        Mark::Failed => (FAILED_MARK, &p.danger, false),
        Mark::DoneMuted => (DONE_MARK, &p.muted, false),
        Mark::FailedMuted => (FAILED_MARK, &p.muted, false),
        Mark::Command => (SHELL_MARK, &p.success, true),
        Mark::CommandFailed => (SHELL_MARK, &p.danger, true),
    };
    paint(
        glyph,
        Paint {
            fg: Some(color),
            bold,
            ..Paint::default()
        },
    )
}

/// One grapheme of a row with its tone and width.
type Cell = (String, Tone, usize);

fn paint_cells(cells: &[Cell], p: &Palette) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < cells.len() {
        let tone = cells[index].1;
        let mut text = String::new();
        while index < cells.len() && cells[index].1 == tone {
            text.push_str(&cells[index].0);
            index += 1;
        }
        let (color, bold, underline) = match tone {
            Tone::Text => (&p.text, false, false),
            Tone::Muted | Tone::Operator => (&p.muted, false, false),
            Tone::Success | Tone::Quoted => (&p.success, false, false),
            Tone::Danger => (&p.danger, false, false),
            Tone::Path => (&p.accent, false, true),
            Tone::Program => (&p.text, true, false),
            Tone::Flag => (&p.warning, false, false),
        };
        out.push_str(&paint(
            &text,
            Paint {
                fg: Some(color),
                bold,
                underline,
                ..Paint::default()
            },
        ));
    }
    out
}

/// Spans cut into rows by cell width, laid out only as far as the rows that
/// will be shown: a command's output can run to megabytes, and its budget is
/// fifteen rows.
fn layout(
    spans: &[Span],
    first_width: usize,
    rest_width: usize,
    overflow: Overflow,
) -> Vec<Vec<Cell>> {
    if overflow == Overflow::Truncate {
        let cells: Vec<Cell> = spans
            .iter()
            .flat_map(|s| cells_of(&s.text.replace(['\r', '\n'], " "), s.tone))
            .collect();
        let total: usize = cells.iter().map(|c| c.2).sum();
        return vec![if total <= first_width {
            cells
        } else {
            ellipsize(&cells, first_width)
        }];
    }
    let mut sources: Vec<Vec<(String, Tone)>> = vec![Vec::new()];
    for s in spans {
        for (index, part) in normalize(&s.text).split('\n').enumerate() {
            if index > 0 {
                sources.push(Vec::new());
            }
            if !part.is_empty()
                && let Some(line) = sources.last_mut()
            {
                line.push((part.to_owned(), s.tone));
            }
        }
    }
    let widest = first_width.max(rest_width);
    let rows_of = |source: &[(String, Tone)], index: usize, tail: bool, max_rows: usize| {
        let mut cells: Vec<Cell> = source
            .iter()
            .flat_map(|(t, tone)| cells_of(t, *tone))
            .collect();
        let budget = max_rows.saturating_mul(widest);
        if cells.len() > budget {
            if tail {
                cells.drain(..cells.len() - budget);
            } else {
                cells.truncate(budget);
            }
        }
        wrap(
            &cells,
            if index == 0 { first_width } else { rest_width },
            rest_width,
        )
    };
    if overflow == Overflow::Tail {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        for (index, source) in sources.iter().enumerate().rev() {
            if rows.len() >= TAIL_MAX_ROWS {
                break;
            }
            let mut wrapped = rows_of(source, index, true, TAIL_MAX_ROWS);
            wrapped.append(&mut rows);
            rows = wrapped;
        }
        let excess = rows.len().saturating_sub(TAIL_MAX_ROWS);
        rows.drain(..excess);
        return rows;
    }
    if overflow == Overflow::Whole {
        return sources
            .iter()
            .enumerate()
            .flat_map(|(index, source)| rows_of(source, index, false, usize::MAX))
            .collect();
    }
    let cap = match overflow {
        Overflow::Rows(rows) => rows,
        _ => WRAP_MAX_ROWS,
    };
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        if rows.len() > cap {
            break;
        }
        rows.extend(rows_of(source, index, false, cap + 1));
    }
    if rows.len() <= cap {
        return rows;
    }
    rows.truncate(cap);
    if let Some(last) = rows.last_mut() {
        *last = ellipsize(last, rest_width);
    }
    rows
}

/// Source lines of the spans wholly above the last [`TAIL_MAX_ROWS`] rows at
/// `width`, without laying them out: each line takes its width over the row,
/// at least one.
fn hidden_lines(spans: &[Span], width: usize) -> usize {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let text = normalize(&text);
    let mut rows = 0;
    let mut hidden = text.split('\n').count();
    for line in text.rsplit('\n') {
        if rows >= TAIL_MAX_ROWS {
            break;
        }
        rows += display_width(line).div_ceil(width).max(1);
        hidden -= 1;
    }
    hidden
}

fn cells_of(text: &str, tone: Tone) -> Vec<Cell> {
    text.graphemes(true)
        .map(|g| (g.to_owned(), tone, display_width(g)))
        .collect()
}

/// The row cut to fit `width` with a closing `…`, on grapheme boundaries.
fn ellipsize(cells: &[Cell], width: usize) -> Vec<Cell> {
    let budget = width.saturating_sub(1);
    let mut used = 0;
    let mut row: Vec<Cell> = cells
        .iter()
        .take_while(|cell| {
            used += cell.2;
            used <= budget
        })
        .cloned()
        .collect();
    let tone = cells.last().map_or(Tone::Muted, |cell| cell.1);
    row.push(("…".to_owned(), tone, 1));
    row
}

/// Rows of one line: a row breaks at its last space when it has one — the
/// space itself is dropped — and mid-word only when a word is wider than the
/// row.
fn wrap(cells: &[Cell], first_width: usize, rest_width: usize) -> Vec<Vec<Cell>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut limit = first_width;
    while start < cells.len() {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < cells.len() && used + cells[end].2 <= limit {
            if cells[end].0 == " " {
                last_space = Some(end);
            }
            used += cells[end].2;
            end += 1;
        }
        if end == cells.len() {
            rows.push(cells[start..].to_vec());
            break;
        }
        if end == start {
            end = start + 1;
        } else if let Some(space) = last_space.filter(|space| *space > start) {
            end = space;
        }
        rows.push(cells[start..end].to_vec());
        start = if cells.get(end).is_some_and(|c| c.0 == " ") {
            end + 1
        } else {
            end
        };
        limit = rest_width;
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}
