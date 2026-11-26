//! The `exec` tool, copied from TS `packages/agent/src/tool/builtin/exec/`
//! (`tool.ts`, `process.ts`, `shell.ts`, `validate.ts`, `output.ts`). Only the
//! differences in `docs/architecture/rust-agent/plugins/WorkspaceTools.md` apply:
//! processes start through `HostPlatform::spawn`, and a command that moves to
//! the background is handed to this branch's `BackgroundExec`.
use super::{
    WorkspaceTools,
    background::{BackgroundExec, StartError, StartRequest},
};
use crate::{
    host::{HostError, ToolExecutionContext},
    host_platform::{
        HostPlatform, HostPlatformError, ISOLATION_SUMMARY, Isolation, Os, OutputRecord,
        ProcessSpec, ProcessSpecError, ProcessTree, ShellEdition, ShellKind, ShellStatus,
        SpawnReply, SpawnRequest, TaskFailure, TaskKind, TaskRecord, TaskRequest, TaskStatus,
        Terminal, WaitReply, WaitRequest, WaitStatus, signal_name,
    },
    tool_registry::definition,
};
use dim_agent_runtime::Branch;
use dim_agent_types::*;
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::host_platform::output::head_bytes;
pub use crate::host_platform::output::{
    Decoder, EXIT_STDIO_GRACE, OUTPUT_FILE_BYTE_LIMIT, OutputSink, bounded_preview,
    capture_preview, pump, sanitize, tail_bytes, truncate_preview,
};

/// Default foreground wait before a command moves to the background.
const DEFAULT_WAIT_TIME: Duration = Duration::from_millis(30_000);
const DEFAULT_EXEC_TIMEOUT_MS: u64 = 300_000;
const MIN_EXEC_TIMEOUT_MS: u64 = 1_000;
const MAX_EXEC_TIMEOUT_MS: u64 = 3_600_000;
/// Foreground output stays inline until this UTF-16 length is exceeded.
const INLINE_OUTPUT_CHAR_LIMIT: usize = 30_000;
const DESCRIPTION_CHARS: usize = 200;

pub fn definition_for(platform: &crate::host_platform::HostPlatform) -> ToolDefinition {
    let mut env_line = format!(
        "Environment: {}/{}",
        platform.os.node_platform(),
        platform.arch
    );
    if let Some(isolation) = ISOLATION_SUMMARY {
        env_line = format!("{env_line}\n{isolation}");
    }
    let mut definition = definition(
        "exec",
        &format!(
            "{env_line}\n\nExecute a command using the shell shown in the environment context. stdout and stderr are merged. Long-running commands automatically move to background.\n\n- Use read, write, and edit for known-path text file operations. Read an existing file before editing it, and derive edit replacements from the latest read result.\n- Do not use shell commands, Python, or sed for ordinary text editing when a dedicated file tool can perform the operation. Use exec for project scripts, data processing, and commands that require shell semantics.\n- Prefer scoped rg/grep/find searches when the target area is known. Avoid scanning /, home, node_modules, .git, dist, build, target, and cache directories.\n- Set timeoutMs when a command has an expected upper bound. Use a narrow timeout for searches and one-shot checks; use the default only when the command duration is uncertain.\n- Set runInBackground for tasks that don't need immediate results. For intentional long-running background processes such as dev servers, set runInBackground: true and timeoutMs: -1.\n- With runInBackground, do not pipe into tail/head: the task already keeps the output tail, and a pipe hides live output until the command ends.\n- Use wait tool to wait a result. Do not run sleep to wait a result."
        ),
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute."},
                "title": {"type": "string", "minLength": 1, "description": "Short, nonblank single-line task title shown in the background task list; separate from the command."},
                "cwd": {"type": "string", "description": "Working directory. Defaults to session cwd."},
                "runInBackground": {
                    "type": "boolean",
                    "description": "Run in background. Completion notification will be sent automatically."
                },
                "timeoutMs": {
                    "type": "number",
                    "description": "Optional timeout in milliseconds. Estimate how long this command should be allowed to run. If omitted, the command times out after 300000 ms. Negative values disable the total timeout only when runInBackground is true; foreground negative values use the default timeout."
                },
            },
            "required": ["command", "title"]
        }),
    );
    definition.risk = Some("execute_process".into());
    definition
}

pub(super) fn stop_definition() -> ToolDefinition {
    let mut tool = definition(
        "exec_stop",
        "Stop a detached exec command on this branch by the Task ID exec returned. Waits for process cleanup and returns its terminal {task_id, status, result} as JSON. An already-ended command keeps its existing result. Only status cancelled confirms cancellation; a failed status must not be treated as stopped. Collaboration agentId/sessionId values are not exec task IDs.",
        json!({"type":"object","required":["taskId"],"additionalProperties":false,"properties":{"taskId":{"type":"string","minLength":1,"description":"Task ID copied exactly from the detached exec result."}}}),
    );
    tool.risk = Some("session".into());
    tool
}

pub(super) async fn stop(
    branch: &Branch,
    call: &ToolCall,
    cancel: &CancellationToken,
) -> Result<ToolResult, HostError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        task_id: String,
    }
    let input: Input = match serde_json::from_value(Value::Object(call.input.clone())) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                format!("exec_stop requires the taskId returned by exec: {error}"),
                "exec_invalid_request",
            ));
        }
    };
    if input.task_id.is_empty() {
        return Ok(failed(
            "exec_stop taskId must not be empty.",
            "exec_invalid_request",
        ));
    }
    crate::context_policy::check_cancel(cancel)?;
    let owner = branch
        .service::<BackgroundExec>()
        .map_err(HostError::from_agent)?;
    match owner.stop(input.task_id).await {
        Ok(ended) => Ok(ToolResult {
            is_error: (ended.status == ExecStatus::Failed).then_some(true),
            ..ToolResult::text(
                serde_json::to_string(&ended).expect("exec terminal contains JSON data"),
            )
        }),
        Err(super::background::TaskError::Read(error)) => Err(HostError::from_agent(error)),
        Err(super::background::TaskError::Unknown) => Ok(failed(
            "Command is not a task of this branch. Use the Task ID returned by exec on this branch; use agent for Collaboration members. No command was stopped.",
            "exec_task_unknown",
        )),
        Err(super::background::TaskError::Closed) => Err(HostError::Cancelled(
            dim_agent_runtime::CAPABILITY_RELOADING.into(),
        )),
    }
}

fn failed(message: impl Into<String>, code: &str) -> ToolResult {
    ToolResult {
        structured_content: Some(object(json!({ "code": code }))),
        ..ToolResult::error(message)
    }
}

/// Declares the command a started call ran (session boundary A.6): the exit
/// status the result observed, or none.
fn ran(mut result: ToolResult, command: &str, exit: Option<CommandExit>) -> ToolResult {
    result.effects = Some(vec![ToolEffect::CommandRun {
        command: command.to_owned(),
        exit_code: exit.as_ref().and_then(|status| status.code),
        signal: exit.and_then(|status| status.signal),
    }]);
    result
}

/// A call cancelled after its command started: the command ran and the tool
/// stopped it, so the caller declares it with no exit status (session
/// boundary A.6).
fn cancelled_result() -> ToolResult {
    ToolResult::error("Tool execution cancelled.")
}

fn object(value: Value) -> JsonObject {
    let Value::Object(map) = value else {
        unreachable!("literal object")
    };
    map
}

struct Input {
    command: String,
    title: Option<String>,
    cwd: Option<String>,
    run_in_background: bool,
    timeout_ms: u64,
    disable_timeout: bool,
}

/// Validation of the model's input, the external boundary (TS `validate.ts`).
/// Errors are the message and code of the failed result.
fn parse(input: &JsonObject) -> Result<Input, (String, &'static str)> {
    let removed: Vec<&str> = ["action", "processId", "yieldTimeMs", "sandbox"]
        .into_iter()
        .filter(|field| input.contains_key(*field))
        .collect();
    if !removed.is_empty() {
        return Err((
            format!(
                "exec accepts command, title, cwd, runInBackground and timeoutMs; a negative timeoutMs disables the background timeout. Remove {}.",
                removed.join(", ")
            ),
            "exec_invalid_request",
        ));
    }
    let timeout = input.get("timeoutMs");
    if let Some(value) = timeout {
        let number = value.as_f64();
        let valid = number.is_some_and(|number| {
            (number.is_finite() && number < 0.0)
                || (number.fract() == 0.0
                    && number >= MIN_EXEC_TIMEOUT_MS as f64
                    && number <= MAX_EXEC_TIMEOUT_MS as f64)
        });
        if !valid {
            return Err((
                format!(
                    "exec timeoutMs must be negative to disable background timeout, or an integer between {MIN_EXEC_TIMEOUT_MS} and {MAX_EXEC_TIMEOUT_MS}."
                ),
                "exec_invalid_timeout",
            ));
        }
    }
    let command = match input.get("command") {
        Some(Value::Array(_)) => {
            return Err((
                "exec requires a non-empty command string. Array format is no longer accepted; pass a single command string.".into(),
                "invalid_input",
            ));
        }
        Some(Value::String(command)) if !command.is_empty() => command.clone(),
        _ => {
            return Err((
                "exec requires a non-empty command string.".into(),
                "invalid_input",
            ));
        }
    };
    let title = match input.get("title") {
        None => None,
        Some(Value::String(title))
            if !title.trim().is_empty()
                && !title.contains(['\r', '\n', '\u{0085}', '\u{2028}', '\u{2029}']) =>
        {
            Some(title.clone())
        }
        _ => {
            return Err((
                "exec title must be a nonblank single-line string.".into(),
                "exec_invalid_request",
            ));
        }
    };
    let negative = timeout
        .and_then(Value::as_f64)
        .is_some_and(|value| value < 0.0);
    Ok(Input {
        command,
        title,
        cwd: input.get("cwd").and_then(Value::as_str).map(String::from),
        run_in_background: input.get("runInBackground") == Some(&Value::Bool(true)),
        timeout_ms: match timeout.and_then(Value::as_f64) {
            Some(value) if value >= 0.0 => value as u64,
            _ => DEFAULT_EXEC_TIMEOUT_MS,
        },
        disable_timeout: negative,
    })
}

/// The POSIX `&` that the shell reads as a background marker, if any.
///
/// The shell reads `&` as a list terminator except inside quotes, a comment, a
/// heredoc body, arithmetic, `${...}`, a backslash escape, or as the second half
/// of `&&`, `<&`, `>&` or `;;&`. A command substitution runs a shell of its own
/// wherever the shell runs it: in plain text, in `" "`, in `${...}` and in a
/// heredoc body whose delimiter is unquoted, so its `&` is a marker. A quoted or
/// backslashed heredoc delimiter runs none, and its `&` is text.
///
/// A few contexts differ by edition, and [`ShellReading`] holds them: whether
/// `((` is an arithmetic command, whether `|&` is a pipe or a ksh coprocess
/// marker, whether `&>` and `&>>` are redirects or markers, and whether an
/// unquoted heredoc delimiter matches a line joined from several physical ones.
///
/// A command this scan cannot read to the end falls back to
/// [`quote_only_marker`], which refuses any `&` outside quotes, and `&>` and
/// `&>>` in an edition without that redirect.
fn background_marker(script: &str, edition: ShellEdition) -> Option<BackgroundMarker> {
    let kind = edition.kind();
    if kind != ShellKind::Posix {
        return None;
    }
    readings(edition)
        .iter()
        .find_map(|reading| scan_marker(script, *reading))
}

/// The marker one reading refuses, if that reading refuses one.
fn scan_marker(script: &str, reading: ShellReading) -> Option<BackgroundMarker> {
    let mut scan = ShellScan::new(script, reading);
    scan.text(None);
    match scan.marker {
        Some(marker) => Some(marker),
        None if scan.unclosed => quote_only_marker(script, reading),
        None => None,
    }
}

/// The `&` the running shell reads as a background marker.
#[derive(Clone, Copy)]
struct BackgroundMarker {
    /// Character index into the command as given.
    index: usize,
    /// The marker as written there.
    token: &'static str,
}

impl BackgroundMarker {
    /// The 1-based line and column, counted in characters of the command as
    /// given.
    fn position(&self, script: &str) -> (usize, usize) {
        let before: String = script.chars().take(self.index).collect();
        let line = before.matches('\n').count() + 1;
        let column = before.chars().rev().take_while(|ch| *ch != '\n').count() + 1;
        (line, column)
    }
}

/// The marker written at `at`.
fn marker_at(chars: &[char], at: usize) -> &'static str {
    match (chars.get(at), chars.get(at + 1)) {
        (Some('|'), _) => "|&",
        (_, Some('!')) => "&!",
        (_, Some('|')) => "&|",
        _ => "&",
    }
}

/// The readings a command may run under.
///
/// `sh` is bash on one host and dash on another, and the fields do not order:
/// dash joins an operator across a continuation where bash does not, and bash
/// joins a heredoc delimiter where dash does not, so neither is stricter.
fn readings(edition: ShellEdition) -> &'static [ShellReading] {
    match edition {
        ShellEdition::Bash | ShellEdition::GitBash => &[BASH],
        ShellEdition::Zsh => &[ZSH],
        ShellEdition::Dash => &[DASH],
        ShellEdition::Ksh => &[KSH],
        ShellEdition::Sh => &[BASH, DASH],
        ShellEdition::Pwsh | ShellEdition::WindowsPowershell | ShellEdition::Cmd => &[],
    }
}

/// How the running edition reads the constructs that differ between shells.
#[derive(Clone, Copy)]
struct ShellReading {
    /// `(( ... ))` at a command start is arithmetic, not two subshells: bash,
    /// zsh and ksh have it, dash and sh do not.
    arithmetic_command: bool,
    /// `|&` is an ordinary pipe: bash and zsh. Elsewhere it is a ksh coprocess,
    /// which runs in the background, or a syntax error.
    pipe_ampersand: bool,
    /// `&>` and `&>>` are redirects: bash and zsh. Elsewhere they are a marker
    /// followed by a redirect.
    amp_redirect: bool,
    /// An unquoted heredoc delimiter matches a logical line joined from several
    /// physical lines: bash and zsh. dash and ksh compare physical lines only.
    joined_delimiter: bool,
    /// A continuation between two characters of an operator joins them: bash
    /// and dash. zsh rejects a split operator and ksh reads two markers.
    joins_operators: bool,
    /// `$'...'` is a quote in which `\` escapes the next character and only an
    /// unescaped `'` closes it: bash, zsh and ksh. dash has no such quote.
    ansi_c_quote: bool,
}

/// How one shell reads the constructs that differ. Measured on bash 3.2.57,
/// zsh 5.9, Apple dash-16 and ksh93u+ 2012-08-01; another program behind `ksh`
/// or `dash` is read as the one measured here.
const BASH: ShellReading = ShellReading {
    arithmetic_command: true,
    pipe_ampersand: true,
    amp_redirect: true,
    joined_delimiter: true,
    joins_operators: true,
    ansi_c_quote: true,
};
const ZSH: ShellReading = ShellReading {
    arithmetic_command: true,
    pipe_ampersand: true,
    amp_redirect: true,
    joined_delimiter: true,
    joins_operators: false,
    ansi_c_quote: true,
};
const DASH: ShellReading = ShellReading {
    arithmetic_command: false,
    pipe_ampersand: false,
    amp_redirect: false,
    joined_delimiter: false,
    joins_operators: true,
    ansi_c_quote: false,
};
const KSH: ShellReading = ShellReading {
    arithmetic_command: true,
    pipe_ampersand: false,
    amp_redirect: false,
    joined_delimiter: false,
    joins_operators: false,
    ansi_c_quote: true,
};

/// The last `&` outside `' '` and `" "` that neither neighbour makes inert.
///
/// This is the fallback for a command [`ShellScan`] gives up on, so it reads no
/// continuations: a scan that stopped early may have stopped inside a quote
/// whose extent joining would move.
fn quote_only_marker(script: &str, reading: ShellReading) -> Option<BackgroundMarker> {
    let chars: Vec<char> = script.chars().collect();
    let (mut single, mut double) = (false, false);
    let mut found = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '$' && chars.get(i + 1) == Some(&'\'') && reading.ansi_c_quote && !double {
            i += 2;
            while let Some(inner) = chars.get(i) {
                i += 1;
                match inner {
                    '\\' => i += 1,
                    '\'' => break,
                    _ => {}
                }
            }
            continue;
        }
        if ch == '\'' && !double {
            single.toggle();
        } else if ch == '"' && !single {
            double.toggle();
        } else if ch == '&' && !single && !double {
            let next = chars.get(i + 1).copied();
            let previous = i.checked_sub(1).map(|index| chars[index]);
            if next == Some('&') {
                i += 2;
                continue;
            }
            let redirect = reading.amp_redirect && next == Some('>');
            if previous != Some('&') && !redirect && previous != Some('>') {
                found = Some(BackgroundMarker {
                    index: i,
                    token: marker_at(&chars, i),
                });
            }
        }
        i += 1;
    }
    found
}

const HEAVY_SEARCH_COMMANDS: [&str; 7] = ["rg", "grep", "ag", "find", "fd", "tree", "du"];

/// The deepest nesting read before the rest of the command is left to
/// [`quote_only_marker`]. A model-written command can nest substitutions without
/// bound, and this scan recurses.
const SHELL_SCAN_DEPTH: usize = 64;

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    expand: bool,
}

/// Walks a POSIX command and records whether an `&` in it is a list terminator.
struct ShellScan {
    chars: Vec<char>,
    index: usize,
    marker: Option<BackgroundMarker>,
    unclosed: bool,
    heredocs: Vec<Heredoc>,
    depth: usize,
    reading: ShellReading,
    /// Whether a continuation is removed while reading ahead. Every shell joins
    /// in `" "`, `${...}`, arithmetic, backquotes, a heredoc delimiter and an
    /// unquoted heredoc body, and none joins in a comment, `' '` or a quoted
    /// heredoc body. In shell text [`ShellReading::joins_operators`] decides.
    join: bool,
}

impl ShellScan {
    fn new(script: &str, reading: ShellReading) -> Self {
        Self {
            chars: script.chars().collect(),
            index: 0,
            marker: None,
            unclosed: false,
            heredocs: Vec::new(),
            depth: 0,
            reading,
            join: false,
        }
    }

    /// The physical index of the character `offset` logical characters ahead.
    ///
    /// A logical character is a physical one, with a continuation removed first
    /// where [`Self::join`] is set. A continuation is a newline preceded by an
    /// odd run of backslashes; an even run is a sequence of escapes ending in
    /// a literal `\`, and its newline stays a newline.
    fn at(&self, offset: usize) -> Option<usize> {
        let mut index = self.index;
        let mut left = offset;
        loop {
            let ch = *self.chars.get(index)?;
            if self.join
                && ch == '\\'
                && self.chars.get(index + 1) == Some(&'\n')
                && self.backslashes_before(index) % 2 == 1
            {
                index += 2;
                continue;
            }
            if left == 0 {
                return Some(index);
            }
            left -= 1;
            index += 1;
        }
    }

    /// How many backslashes end at `index`, counting backwards.
    fn backslashes_before(&self, index: usize) -> usize {
        self.chars[..=index]
            .iter()
            .rev()
            .take_while(|ch| **ch == '\\')
            .count()
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.at(offset).map(|index| self.chars[index])
    }

    fn skip(&mut self, count: usize) {
        self.index = self.at(count).unwrap_or(self.chars.len());
    }

    /// Records the marker at `at`, keeping the first one found.
    fn mark(&mut self, at: usize) {
        if self.marker.is_none() {
            self.marker = Some(BackgroundMarker {
                index: at,
                token: marker_at(&self.chars, at),
            });
        }
    }

    fn enter(&mut self) -> bool {
        if self.depth == SHELL_SCAN_DEPTH {
            self.unclosed = true;
            self.skip(self.chars.len());
            return false;
        }
        self.depth += 1;
        true
    }

    /// Shell text, up to `close` where that ends an enclosing substitution.
    fn text(&mut self, close: Option<char>) {
        if !self.enter() {
            return;
        }
        let joins = self.join || self.reading.joins_operators;
        let joined = std::mem::replace(&mut self.join, joins);
        let mut done = false;
        let mut word_start = true;
        let mut parens = 0_usize;
        while !done {
            let Some(ch) = self.peek(0) else { break };
            match ch {
                '\n' => {
                    self.skip(1);
                    word_start = true;
                    self.read_heredocs();
                }
                ' ' | '\t' => {
                    self.skip(1);
                    word_start = true;
                }
                '#' if word_start => self.comment(),
                '\\' => {
                    self.skip(2);
                    word_start = false;
                }
                '\'' => {
                    self.single_quoted();
                    word_start = false;
                }
                '"' => {
                    self.skip(1);
                    self.double_quoted();
                    word_start = false;
                }
                '`' => {
                    self.skip(1);
                    if close == Some('`') {
                        done = true;
                    } else {
                        self.text(Some('`'));
                    }
                    word_start = false;
                }
                '$' if self.peek(1) == Some('\'') && self.reading.ansi_c_quote => {
                    self.ansi_c_quoted();
                    word_start = false;
                }
                '$' => {
                    self.dollar();
                    word_start = false;
                }
                '(' => {
                    if self.peek(1) == Some('(') && word_start && self.reading.arithmetic_command {
                        self.arithmetic(2);
                    } else {
                        self.skip(1);
                        parens += 1;
                    }
                    word_start = true;
                }
                ')' if parens > 0 => {
                    parens -= 1;
                    self.skip(1);
                    word_start = false;
                }
                ')' if close == Some(')') => {
                    self.skip(1);
                    done = true;
                }
                '}' if close == Some('}') => {
                    self.skip(1);
                    done = true;
                }
                ')' => {
                    self.skip(1);
                    word_start = false;
                }
                '<' if self.peek(1) == Some('<') && self.peek(2) != Some('<') => {
                    self.heredoc_opener();
                    word_start = false;
                }
                '<' if self.peek(1) == Some('<') => {
                    self.skip(3);
                    word_start = false;
                }
                '<' if self.peek(1) == Some('&') => {
                    self.skip(2);
                    word_start = false;
                }
                '<' if self.peek(1) == Some('(') => {
                    self.skip(2);
                    self.text(Some(')'));
                    word_start = false;
                }
                '>' if self.peek(1) == Some('&') => {
                    self.skip(2);
                    word_start = false;
                }
                '>' if self.peek(1) == Some('(') => {
                    self.skip(2);
                    self.text(Some(')'));
                    word_start = false;
                }
                '<' | '>' => {
                    self.skip(1);
                    word_start = false;
                }
                '|' => {
                    if self.peek(1) == Some('&') && !self.reading.pipe_ampersand {
                        self.mark(self.index);
                        self.skip(2);
                    } else {
                        let width = if matches!(self.peek(1), Some('&') | Some('|')) {
                            2
                        } else {
                            1
                        };
                        self.skip(width);
                    }
                    word_start = true;
                }
                ';' => {
                    let width = if self.peek(1) == Some(';') && self.peek(2) == Some('&') {
                        3
                    } else if matches!(self.peek(1), Some(';') | Some('&')) {
                        2
                    } else {
                        1
                    };
                    self.skip(width);
                    word_start = true;
                }
                '&' => {
                    let redirect = self.reading.amp_redirect && self.peek(1) == Some('>');
                    let width = if self.peek(1) == Some('&') {
                        2
                    } else if redirect {
                        if self.peek(2) == Some('>') { 3 } else { 2 }
                    } else {
                        self.mark(self.index);
                        1
                    };
                    self.skip(width);
                    word_start = true;
                }
                _ => {
                    self.skip(1);
                    word_start = false;
                }
            }
        }
        if !done && close.is_some() {
            self.unclosed = true;
        }
        self.join = joined;
        self.depth -= 1;
    }

    fn comment(&mut self) {
        while let Some(ch) = self.chars.get(self.index) {
            if *ch == '\n' {
                return;
            }
            self.index += 1;
        }
    }

    /// `$'...'`: here `\` escapes the next character, so `\'` does not close
    /// the quote.
    fn ansi_c_quoted(&mut self) {
        self.skip(2);
        loop {
            let Some(ch) = self.peek(0) else {
                self.unclosed = true;
                return;
            };
            self.skip(1);
            match ch {
                '\\' => self.skip(1),
                '\'' => return,
                _ => {}
            }
        }
    }

    fn single_quoted(&mut self) {
        self.index += 1;
        loop {
            let Some(ch) = self.chars.get(self.index) else {
                self.unclosed = true;
                return;
            };
            self.index += 1;
            if *ch == '\'' {
                return;
            }
        }
    }

    /// `" "`, where the shell runs substitutions and reads every other `&` as
    /// text.
    fn double_quoted(&mut self) {
        let joined = std::mem::replace(&mut self.join, true);
        self.double_quoted_inner();
        self.join = joined;
    }

    fn double_quoted_inner(&mut self) {
        loop {
            let Some(ch) = self.peek(0) else {
                self.unclosed = true;
                return;
            };
            match ch {
                '\\' => self.skip(2),
                '"' => {
                    self.skip(1);
                    return;
                }
                '`' => {
                    self.skip(1);
                    self.text(Some('`'));
                }
                '$' => self.dollar(),
                _ => self.skip(1),
            }
        }
    }
    /// `$((...))` and `((...))`, where `&` is bitwise and, when the context
    /// closes with `))` as arithmetic must.
    ///
    /// A context that closes its own level with a single `)` is not arithmetic:
    /// the shell reads `((` as a subshell of a subshell, or `$((` as a
    /// substitution around a subshell, and an `&` inside is then a marker. Such
    /// a context is rewound to its opener and read as shell text instead.
    fn arithmetic(&mut self, opener: usize) {
        let start = self.index;
        let joined = std::mem::replace(&mut self.join, true);
        let end = self.arithmetic_end(opener);
        self.join = joined;
        if let Some(end) = end {
            self.index = end;
            return;
        }
        self.index = start;
        self.nested(opener);
    }

    /// Reads the arithmetic context to its `))` and answers where it ends, or
    /// resets the index and answers nothing when it closes with one `)`.
    fn arithmetic_end(&mut self, opener: usize) -> Option<usize> {
        if !self.enter() {
            return Some(self.index);
        }
        self.skip(opener);
        let mut parens = 0_usize;
        let end = loop {
            let Some(ch) = self.peek(0) else {
                self.unclosed = true;
                break Some(self.index);
            };
            match ch {
                '(' => {
                    parens += 1;
                    self.skip(1);
                }
                ')' if parens == 0 && self.peek(1) == Some(')') => {
                    self.skip(2);
                    break Some(self.index);
                }
                ')' if parens == 0 => {
                    self.index -= opener;
                    break None;
                }
                ')' => {
                    parens -= 1;
                    self.skip(1);
                }
                '$' => self.dollar(),
                '`' => {
                    self.skip(1);
                    self.text(Some('`'));
                }
                _ => self.skip(1),
            }
        };
        self.depth -= 1;
        end
    }

    /// A `$(` or `(` that turned out to hold a subshell rather than arithmetic,
    /// read as the shell reads it, up to the `)` that closes it.
    ///
    /// Only the outer opener is skipped, one `(` of `((` and the `$(` of `$((`:
    /// the `(` that follows it is an ordinary subshell opener, which `text`'s
    /// `'('` arm counts, so the scan stops at the outer `)` instead of reading to
    /// the end of the command.
    fn nested(&mut self, opener: usize) {
        self.skip(opener - 1);
        self.text(Some(')'));
    }

    /// `${...}`, where `&` is part of the word unless a substitution runs.
    ///
    /// `${ command; }` and `${|command; }` hold shell text rather than a
    /// parameter name. bash 5.3 and ksh93 run the command in the current shell;
    /// an edition without the form rejects it as a bad substitution and starts
    /// nothing, so reading it as command text loses nothing real.
    fn parameter(&mut self) {
        let joined = std::mem::replace(&mut self.join, true);
        self.parameter_inner();
        self.join = joined;
    }

    fn parameter_inner(&mut self) {
        if matches!(
            self.peek(2),
            Some(' ') | Some('\t') | Some('\n') | Some('|')
        ) {
            self.skip(2);
            self.text(Some('}'));
            return;
        }
        if !self.enter() {
            return;
        }
        self.skip(2);
        let mut braces = 0_usize;
        loop {
            let Some(ch) = self.peek(0) else {
                self.unclosed = true;
                break;
            };
            match ch {
                '\\' => self.skip(2),
                '{' => {
                    braces += 1;
                    self.skip(1);
                }
                '}' if braces == 0 => {
                    self.skip(1);
                    break;
                }
                '}' => {
                    braces -= 1;
                    self.skip(1);
                }
                '"' => {
                    self.skip(1);
                    self.double_quoted();
                }
                '\'' => self.single_quoted(),
                '$' if self.peek(1) == Some('\'') && self.reading.ansi_c_quote => {
                    self.ansi_c_quoted();
                }
                '$' => self.dollar(),
                '`' => {
                    self.skip(1);
                    self.text(Some('`'));
                }
                _ => self.skip(1),
            }
        }
        self.depth -= 1;
    }

    fn dollar(&mut self) {
        match (self.peek(1), self.peek(2)) {
            (Some('('), Some('(')) => self.arithmetic(3),
            (Some('('), _) => {
                self.skip(2);
                self.text(Some(')'));
            }
            (Some('{'), _) => self.parameter(),
            _ => self.skip(1),
        }
    }

    fn heredoc_opener(&mut self) {
        let joined = std::mem::replace(&mut self.join, true);
        self.heredoc_opener_inner();
        self.join = joined;
    }

    fn heredoc_opener_inner(&mut self) {
        self.skip(2);
        let strip_tabs = self.peek(0) == Some('-');
        if strip_tabs {
            self.skip(1);
        }
        while matches!(self.peek(0), Some(' ') | Some('\t')) {
            self.skip(1);
        }
        let mut delimiter = String::new();
        let mut expand = true;
        while let Some(ch) = self.peek(0) {
            match ch {
                '\'' | '"' => {
                    expand = false;
                    self.skip(1);
                    while let Some(inner) = self.peek(0) {
                        self.skip(1);
                        if inner == ch {
                            break;
                        }
                        delimiter.push(inner);
                    }
                }
                '\\' => {
                    expand = false;
                    self.skip(1);
                    if let Some(inner) = self.peek(0) {
                        delimiter.push(inner);
                        self.skip(1);
                    }
                }
                ch if ch.is_whitespace()
                    || matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')') =>
                {
                    break;
                }
                ch => {
                    delimiter.push(ch);
                    self.skip(1);
                }
            }
        }
        self.heredocs.push(Heredoc {
            delimiter,
            strip_tabs,
            expand,
        });
    }

    /// Reads the heredoc bodies queued on the line that just ended.
    ///
    /// A body is a run of logical lines. `<<-` strips the leading tabs of each
    /// logical line's first physical line. The delimiter matches a logical line
    /// of one physical line in every edition, and one joined across
    /// continuations only where the edition joins it.
    fn read_heredocs(&mut self) {
        while !self.heredocs.is_empty() {
            let heredoc = self.heredocs.remove(0);
            let mut body = String::new();
            let mut origin = Vec::new();
            loop {
                let start = self.index;
                let Some((first, terminated)) = self.physical_line() else {
                    self.unclosed = true;
                    break;
                };
                let strip = if heredoc.strip_tabs {
                    first.len() - first.trim_start_matches('\t').len()
                } else {
                    0
                };
                let mut logical = first[strip..].to_owned();
                let mut origin_of_line: Vec<usize> = (0..first.len())
                    .map(|offset| start + offset)
                    .skip(strip)
                    .collect();
                let mut joined = false;
                let mut last_terminated = terminated;
                while heredoc.expand && last_terminated && ends_in_continuation(&logical) {
                    logical.pop();
                    origin_of_line.pop();
                    let next_start = self.index;
                    let Some((next, terminated)) = self.physical_line() else {
                        last_terminated = false;
                        break;
                    };
                    logical.push_str(&next);
                    origin_of_line.extend((0..next.len()).map(|offset| next_start + offset));
                    joined = true;
                    last_terminated = terminated;
                }
                if logical == heredoc.delimiter && (!joined || self.reading.joined_delimiter) {
                    break;
                }
                body.push_str(&logical);
                body.push('\n');
                origin.extend(origin_of_line);
                origin.push(start);
                if !last_terminated {
                    self.unclosed = true;
                    break;
                }
            }
            if heredoc.expand {
                let mut inner = ShellScan::new(&body, self.reading);
                inner.expansions();
                if let Some(marker) = inner.marker {
                    let at = origin.get(marker.index).copied().unwrap_or(marker.index);
                    self.mark(at);
                }
                self.unclosed |= inner.unclosed;
            }
        }
    }

    /// One physical line and whether a newline followed it.
    fn physical_line(&mut self) -> Option<(String, bool)> {
        if self.index >= self.chars.len() {
            return None;
        }
        let start = self.index;
        let mut end = start;
        while end < self.chars.len() && self.chars[end] != '\n' {
            end += 1;
        }
        let line: String = self.chars[start..end].iter().collect();
        let terminated = end < self.chars.len();
        self.index = if terminated { end + 1 } else { end };
        Some((line, terminated))
    }

    /// A heredoc body under an unquoted delimiter: only expansions run, and
    /// every other character, `&` and quotes included, is text.
    fn expansions(&mut self) {
        self.join = true;
        while let Some(ch) = self.peek(0) {
            match ch {
                '\\' => self.skip(2),
                '$' => self.dollar(),
                '`' => {
                    self.skip(1);
                    self.text(Some('`'));
                }
                _ => self.skip(1),
            }
        }
    }
}

/// Whether a physical line ends in a line continuation: an odd run of
/// backslashes before its newline.
fn ends_in_continuation(line: &str) -> bool {
    line.bytes().rev().take_while(|byte| *byte == b'\\').count() % 2 == 1
}

const HIGH_COST_BASENAMES: [&str; 5] = ["node_modules", ".git", "dist", "build", "target"];

fn check_heavy_search(command: &str, cwd: &str, home: &str) -> Option<ToolResult> {
    let words = tokenize_shell_command(command);
    let first = words.first()?;
    let family = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_lowercase();
    if !HEAVY_SEARCH_COMMANDS.contains(&family.as_str()) {
        return None;
    }
    if has_unquoted_command_substitution(command) {
        return Some(failed(
            format!(
                "Refusing {family} command with command substitution. Use a literal scoped path or grep/glob tool."
            ),
            "exec_dynamic_search_refused",
        ));
    }
    scan_targets(&family, &words[1..])
        .into_iter()
        .find(|target| high_cost_target(target, cwd, home))
        .map(|target| {
            failed(
                format!(
                    "Refusing high-cost {family} scan over \"{target}\". Use a scoped project path or grep/glob tool."
                ),
                "exec_high_cost_target_refused",
            )
        })
}

/// The paths a family's words scan, in the order the command names them.
///
/// An option that takes a value belongs in the family's table; one left out is
/// read as before the table existed, so its value becomes a scanned path or the
/// pattern.
fn scan_targets(family: &str, args: &[String]) -> Vec<String> {
    if family == "find" {
        return find_starting_points(args);
    }
    let Some(mut reading) = reading(family) else {
        return vec![];
    };
    if family == "rg" && args.iter().any(|arg| arg == "--files") {
        reading.shape = Shape::Paths;
    }
    reading.targets(args)
}

/// What a family reads its positional words as once the options are removed.
#[derive(Clone, Copy)]
enum Shape {
    /// `PATTERN [PATH...]`: the first positional is a pattern.
    Pattern,
    /// `[PATH...]`: every positional is a path.
    Paths,
}

/// What an option's value names.
#[derive(Clone, Copy, PartialEq)]
enum Named {
    /// The command's pattern: it takes the place of the positional one.
    Pattern,
    /// A path the command scans like a positional one.
    Path,
    /// Neither: the value names nothing the command scans.
    Other,
}

/// How one family reads its words.
struct Reading {
    shape: Shape,
    /// Options that take a value, and what the value names. A value is the
    /// next word, or attached to the option's own word: `-m 5`, `-m5`,
    /// `--max-count=5`, `--glob build`.
    valued: &'static [(&'static str, Named)],
}

/// What one argument word holds.
enum Taken<'a> {
    /// An option no table lists, written bare: like any word starting with
    /// `-`, it names nothing the command scans and takes no value.
    Flag,
    /// A value written inside this word, and what it names.
    Attached(&'a str, Named),
    /// The next word is this option's value.
    Next(Named),
    /// A word the command reads as a positional.
    Operand,
}

impl Reading {
    /// The paths this family scans, in the order the command names them.
    fn targets(&self, args: &[String]) -> Vec<String> {
        let mut operands: Vec<&str> = vec![];
        let mut named: Vec<&str> = vec![];
        let mut pattern_named = false;
        let mut index = 0;
        while index < args.len() {
            let word = args[index].as_str();
            index += 1;
            let (value, role) = match self.take(word) {
                Taken::Operand => {
                    operands.push(word);
                    continue;
                }
                Taken::Flag => continue,
                Taken::Attached(value, role) => (value, role),
                Taken::Next(role) => match args.get(index) {
                    Some(value) => {
                        index += 1;
                        (value.as_str(), role)
                    }
                    None => continue,
                },
            };
            match role {
                Named::Pattern => pattern_named = true,
                Named::Path => named.push(value),
                Named::Other => {}
            }
        }
        let scanned = match (self.shape, pattern_named) {
            (Shape::Pattern, false) => operands.get(1..).unwrap_or_default(),
            (Shape::Pattern, true) | (Shape::Paths, _) => &operands[..],
        };
        scanned
            .iter()
            .chain(named.iter())
            .map(|word| (*word).to_owned())
            .collect()
    }

    fn take<'a>(&self, word: &'a str) -> Taken<'a> {
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            return match self.valued.iter().find(|(spelling, _)| *spelling == name) {
                Some((_, role)) => match attached {
                    Some(value) => Taken::Attached(value, *role),
                    None => Taken::Next(*role),
                },
                None => Taken::Flag,
            };
        }
        let Some(cluster) = word.strip_prefix('-') else {
            return Taken::Operand;
        };
        for (offset, letter) in cluster.char_indices() {
            let end = offset + letter.len_utf8();
            let short = &cluster[offset..end];
            if let Some((_, role)) = self.valued.iter().find(|(spelling, _)| *spelling == short) {
                return match cluster[end..].is_empty() {
                    true => Taken::Next(*role),
                    false => Taken::Attached(&cluster[end..], *role),
                };
            }
        }
        Taken::Flag
    }
}

/// Each family's options that take a value, named as the manual it ships
/// lists them: grep(1) (BSD, whose long spellings GNU grep shares), rg(1)
/// (ripgrep 15), fd(1), tree(1) 2.3, du(1) (BSD and GNU) and ag(1). An option
/// left out takes no value here, so the word after it stays an operand.
fn reading(family: &str) -> Option<Reading> {
    let reading = match family {
        "grep" => Reading {
            shape: Shape::Pattern,
            valued: &[
                ("e", Named::Pattern),
                ("regexp", Named::Pattern),
                ("f", Named::Pattern),
                ("file", Named::Pattern),
                ("A", Named::Other),
                ("after-context", Named::Other),
                ("B", Named::Other),
                ("before-context", Named::Other),
                ("C", Named::Other),
                ("context", Named::Other),
                ("D", Named::Other),
                ("devices", Named::Other),
                ("d", Named::Other),
                ("directories", Named::Other),
                ("m", Named::Other),
                ("max-count", Named::Other),
                ("exclude", Named::Other),
                ("exclude-dir", Named::Other),
                ("exclude-from", Named::Other),
                ("include", Named::Other),
                ("include-dir", Named::Other),
                ("binary-files", Named::Other),
                ("label", Named::Other),
                ("group-separator", Named::Other),
            ],
        },
        "ag" => Reading {
            shape: Shape::Pattern,
            valued: &[
                ("E", Named::Other),
                ("extension", Named::Other),
                ("G", Named::Other),
                ("file-search-regex", Named::Other),
                ("g", Named::Other),
                ("filename-pattern", Named::Other),
                ("m", Named::Other),
                ("max-count", Named::Other),
                ("W", Named::Other),
                ("width", Named::Other),
                ("P", Named::Other),
                ("pager", Named::Other),
                ("I", Named::Other),
                ("ignore", Named::Other),
                ("ignore-dir", Named::Other),
                ("X", Named::Other),
                ("invert-file-search-regex", Named::Other),
                ("workers", Named::Other),
                ("depth", Named::Other),
                ("agrc", Named::Other),
            ],
        },
        "rg" => Reading {
            shape: Shape::Pattern,
            valued: &[
                ("e", Named::Pattern),
                ("regexp", Named::Pattern),
                ("f", Named::Pattern),
                ("file", Named::Pattern),
                ("g", Named::Other),
                ("glob", Named::Other),
                ("iglob", Named::Other),
                ("ignore-file", Named::Other),
                ("pre", Named::Other),
                ("pre-glob", Named::Other),
                ("E", Named::Other),
                ("encoding", Named::Other),
                ("engine", Named::Other),
                ("m", Named::Other),
                ("max-count", Named::Other),
                ("j", Named::Other),
                ("threads", Named::Other),
                ("d", Named::Other),
                ("max-depth", Named::Other),
                ("t", Named::Other),
                ("type", Named::Other),
                ("T", Named::Other),
                ("type-not", Named::Other),
                ("type-add", Named::Other),
                ("type-clear", Named::Other),
                ("A", Named::Other),
                ("after-context", Named::Other),
                ("B", Named::Other),
                ("before-context", Named::Other),
                ("C", Named::Other),
                ("context", Named::Other),
                ("context-separator", Named::Other),
                ("field-context-separator", Named::Other),
                ("field-match-separator", Named::Other),
                ("colors", Named::Other),
                ("hostname-bin", Named::Other),
                ("hyperlink-format", Named::Other),
                ("M", Named::Other),
                ("max-columns", Named::Other),
                ("path-separator", Named::Other),
                ("r", Named::Other),
                ("replace", Named::Other),
                ("sort", Named::Other),
                ("sortr", Named::Other),
            ],
        },
        "fd" => Reading {
            shape: Shape::Pattern,
            valued: &[
                ("and", Named::Other),
                ("d", Named::Other),
                ("max-depth", Named::Other),
                ("min-depth", Named::Other),
                ("exact-depth", Named::Other),
                ("t", Named::Other),
                ("type", Named::Other),
                ("e", Named::Other),
                ("extension", Named::Other),
                ("E", Named::Other),
                ("exclude", Named::Other),
                ("ignore-contain", Named::Other),
                ("ignore-file", Named::Other),
                ("c", Named::Other),
                ("color", Named::Other),
                ("j", Named::Other),
                ("threads", Named::Other),
                ("S", Named::Other),
                ("size", Named::Other),
                ("changed-within", Named::Other),
                ("changed-before", Named::Other),
                ("o", Named::Other),
                ("owner", Named::Other),
                ("C", Named::Other),
                ("base-directory", Named::Other),
                ("path-separator", Named::Other),
                ("format", Named::Other),
                ("x", Named::Other),
                ("exec", Named::Other),
                ("X", Named::Other),
                ("exec-batch", Named::Other),
                ("batch-size", Named::Other),
                ("max-results", Named::Other),
                ("search-path", Named::Path),
            ],
        },
        "tree" => Reading {
            shape: Shape::Paths,
            valued: &[
                ("H", Named::Other),
                ("L", Named::Other),
                ("P", Named::Other),
                ("I", Named::Other),
                ("o", Named::Other),
                ("T", Named::Other),
                ("gitfile", Named::Other),
                ("hintro", Named::Other),
                ("houtro", Named::Other),
                ("sort", Named::Other),
                ("filelimit", Named::Other),
                ("charset", Named::Other),
                ("timefmt", Named::Other),
                ("scheme", Named::Other),
                ("authority", Named::Other),
                ("compress", Named::Other),
                ("infofile", Named::Other),
            ],
        },
        "du" => Reading {
            shape: Shape::Paths,
            valued: &[
                ("B", Named::Other),
                ("block-size", Named::Other),
                ("d", Named::Other),
                ("max-depth", Named::Other),
                ("t", Named::Other),
                ("threshold", Named::Other),
                ("I", Named::Other),
                ("exclude", Named::Other),
                ("exclude-from", Named::Other),
                ("X", Named::Other),
                ("files0-from", Named::Other),
                ("time", Named::Other),
                ("time-style", Named::Other),
            ],
        },
        _ => return None,
    };
    Some(reading)
}

/// `find [options] [starting-point...] [expression]`: only the starting
/// points are searched; expression operands such as `-name node_modules`
/// are patterns, not targets. Without starting points `find` searches `.`.
fn find_starting_points(args: &[String]) -> Vec<String> {
    let points: Vec<String> = args
        .iter()
        .skip_while(|token| {
            matches!(
                token.as_str(),
                "-H" | "-L" | "-P" | "-D" | "-E" | "-X" | "-d" | "-s" | "-x" | "-f"
            ) || (token.starts_with("-O") && token.len() > 2)
        })
        .take_while(|token| !token.starts_with(['-', '(', ')', '!']))
        .cloned()
        .collect();
    if points.is_empty() {
        vec![".".into()]
    } else {
        points
    }
}

fn tokenize_shell_command(command: &str) -> Vec<String> {
    let mut tokens = vec![];
    let mut current = String::new();
    let (mut single, mut double) = (false, false);
    // A redirect's target is a file the shell opens, not a word the command
    // reads: `>`, `>>`, `2>`, `<` and the attached `>file` alike.
    let mut target = false;
    for ch in command.chars() {
        if ch == '\'' && !double {
            single.toggle();
            continue;
        }
        if ch == '"' && !single {
            double.toggle();
            continue;
        }
        if single || double {
            current.push(ch);
            continue;
        }
        match ch {
            '<' | '>' => {
                // A leading run of digits is a file descriptor, not an operand.
                if !current.chars().all(|c| c.is_ascii_digit()) {
                    end_word(&mut tokens, &mut current, &mut target);
                }
                current.clear();
                target = true;
            }
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    end_word(&mut tokens, &mut current, &mut target);
                }
            }
            ';' | '|' | '&' => break,
            _ => current.push(ch),
        }
    }
    end_word(&mut tokens, &mut current, &mut target);
    tokens
}

/// Ends the word being read, unless it names a redirect's target.
fn end_word(tokens: &mut Vec<String>, word: &mut String, target: &mut bool) {
    let redirected = std::mem::take(target);
    if !redirected && !word.is_empty() {
        tokens.push(std::mem::take(word));
    }
    word.clear();
}

fn has_unquoted_command_substitution(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' && !single {
            i += 2;
            continue;
        }
        if ch == '\'' && !double {
            single.toggle();
        } else if ch == '"' && !single {
            double.toggle();
        } else if !single && (ch == '`' || (ch == '$' && chars.get(i + 1) == Some(&'('))) {
            return true;
        }
        i += 1;
    }
    false
}

fn strip_trailing_slash(value: &str) -> &str {
    if value == "/" {
        value
    } else {
        value.trim_end_matches('/')
    }
}

fn collapse_slashes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if !(ch == '/' && output.ends_with('/')) {
            output.push(ch);
        }
    }
    output
}

fn high_cost_target(raw: &str, cwd: &str, home: &str) -> bool {
    let target = strip_trailing_slash(raw);
    if target == "/" || target == "~" {
        return true;
    }
    if target == "." || target == "./" {
        return false;
    }
    let normalized = target.replace('\\', "/");
    if normalized
        .split('/')
        .any(|part| HIGH_COST_BASENAMES.contains(&part))
    {
        return true;
    }
    let resolved = if target == "~" {
        home.to_owned()
    } else if let Some(rest) = target.strip_prefix("~/") {
        collapse_slashes(&format!("{home}/{rest}"))
    } else if target.starts_with('/') {
        let stripped = strip_trailing_slash(target);
        if stripped.is_empty() { "/" } else { stripped }.to_owned()
    } else {
        let joined = collapse_slashes(&format!("{cwd}/{target}"));
        let stripped = strip_trailing_slash(&joined);
        if stripped.is_empty() { cwd } else { stripped }.to_owned()
    };
    if resolved == "/" || resolved == home {
        return true;
    }
    strip_trailing_slash(&resolved)
        .split('/')
        .rfind(|part| !part.is_empty())
        .is_some_and(|base| HIGH_COST_BASENAMES.contains(&base))
}

/// TS `path.ts` resolution of the `cwd` argument.
fn resolve_cwd(value: &str, cwd: &Path, home: &Path, os: Os) -> PathBuf {
    let expanded = if value == "~" {
        home.to_owned()
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home.join(rest)
    } else if os == Os::Windows {
        PathBuf::from(correct_windows_path(value))
    } else {
        PathBuf::from(value)
    };
    let mut resolved = PathBuf::new();
    for component in cwd.join(expanded).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// `/cygdrive/c/x` and `/C:/x` become native Windows paths.
fn correct_windows_path(value: &str) -> String {
    let drive = |letter: char| letter.is_ascii_alphabetic();
    if let Some(rest) = value.strip_prefix("/cygdrive/") {
        let mut chars = rest.chars();
        if let Some(letter) = chars.next().filter(|letter| drive(*letter)) {
            let tail = chars.as_str();
            if tail.is_empty() || tail.starts_with('/') {
                let tail = tail.trim_prefix('/');
                return format!(
                    "{}:\\{}",
                    letter.to_ascii_uppercase(),
                    tail.replace('/', "\\")
                );
            }
        }
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && drive(bytes[1] as char) && bytes[2] == b':' {
        return format!("{}{}", &value[1..3], value[3..].replace('/', "\\"));
    }
    value.to_owned()
}

/// Merged canonical output of one command, inline until it outgrows the
/// budget, then in the output file (TS `ExecOutputCapture`).
struct Capture {
    path: PathBuf,
    inline: String,
    inline_units: usize,
    preview: String,
    file: Option<tokio::fs::File>,
    file_bytes: u64,
    output_bytes: u64,
    truncated: bool,
    failure: Option<TaskFailure>,
}
impl Capture {
    async fn process(&mut self, text: &str) -> io::Result<()> {
        self.output_bytes += text.len() as u64;
        if self.file.is_none() {
            self.inline.push_str(text);
            self.inline_units += text.encode_utf16().count();
            if self.inline_units > INLINE_OUTPUT_CHAR_LIMIT {
                self.create_file().await?;
            }
            return Ok(());
        }
        self.append(text).await
    }
    async fn create_file(&mut self) -> io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let initial = head_bytes(&self.inline, OUTPUT_FILE_BYTE_LIMIT as usize);
        let mut directory = tokio::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        directory.mode(0o700);
        directory
            .create(self.path.parent().expect("output file has a directory"))
            .await?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.path).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, initial.as_bytes()).await?;
        file.sync_all().await?;
        self.file_bytes = initial.len() as u64;
        if initial.len() < self.inline.len() {
            self.truncated = true;
        }
        self.file = Some(file);
        self.inline = String::new();
        self.inline_units = 0;
        Ok(())
    }
    async fn append(&mut self, text: &str) -> io::Result<()> {
        if self.truncated {
            return Ok(());
        }
        let remaining = OUTPUT_FILE_BYTE_LIMIT.saturating_sub(self.file_bytes);
        if remaining == 0 {
            self.truncated = true;
            return Ok(());
        }
        let writable = head_bytes(text, remaining as usize);
        if !writable.is_empty() {
            let file = self.file.as_mut().expect("appending needs the file");
            tokio::io::AsyncWriteExt::write_all(file, writable.as_bytes()).await?;
            self.file_bytes += writable.len() as u64;
        }
        if writable.len() < text.len() {
            self.truncated = true;
        }
        Ok(())
    }
    fn output_path(&self) -> Option<String> {
        self.file
            .as_ref()
            .map(|_| self.path.to_string_lossy().into_owned())
    }
}

pub(crate) struct Output {
    capture: Mutex<Capture>,
    /// Cancelled when the capture failed; the command is then terminated.
    failed: CancellationToken,
}
impl Output {
    async fn ensure_file(&self) -> Result<String, String> {
        let mut capture = self.capture.lock().await;
        if let Some(failure) = &capture.failure {
            return Err(failure.to_string());
        }
        match capture.create_file().await {
            Ok(()) => Ok(capture.path.to_string_lossy().into_owned()),
            Err(error) => {
                let error = error.to_string();
                capture.failure = Some(TaskFailure::OutputWriteFailed {
                    message: error.clone(),
                });
                Err(error)
            }
        }
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputTail {
    pub text: String,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("Unknown execution task")]
    Unknown,
    #[error("{0}")]
    Unavailable(String),
}

impl HostPlatform {
    pub(crate) async fn start_command(
        &self,
        task_id: &str,
        spec: ProcessSpec,
        roots: &[PathBuf],
        output_dir: &Path,
        timeout: Option<u64>,
    ) -> Result<(), ProcessSpecError> {
        if tokio::fs::create_dir_all(output_dir).await.is_err() {
            return Err(ProcessSpecError {
                code: "invalid_request",
                reason: "output directory is not usable".into(),
            });
        }
        if std::fs::symlink_metadata(output_dir.join(format!("{task_id}.log"))).is_ok() {
            return Err(ProcessSpecError {
                code: "task_exists",
                reason: "Execution task already exists".into(),
            });
        }
        let (spec, temporary, isolation) = self.prepare_isolated_process(spec, roots, None).await?;
        let path = output_dir.join(format!("{task_id}.log"));
        let output = Arc::new(Output {
            capture: Mutex::new(Capture {
                path: path.clone(),
                inline: String::new(),
                inline_units: 0,
                preview: String::new(),
                file: None,
                file_bytes: 0,
                output_bytes: 0,
                truncated: false,
                failure: None,
            }),
            failed: CancellationToken::new(),
        });
        let task = Arc::new(CommandTask {
            output: output.clone(),
            path,
            isolation,
            kill: CancellationToken::new(),
        });
        let record = TaskRecord::new(task_id.to_owned(), TaskKind::Command(task.clone()));
        {
            let mut commands = self.commands.lock().unwrap();
            if self.command_shutdown.is_cancelled() {
                return Err(ProcessSpecError {
                    code: "cancelled",
                    reason: "Execution host closed".into(),
                });
            }
            if commands.contains_key(task_id) {
                return Err(ProcessSpecError {
                    code: "task_exists",
                    reason: "Execution task already exists".into(),
                });
            }
            commands.insert(task_id.to_owned(), record.clone());
        }
        let mut tree = match self.spawn_tree(spec) {
            Ok(tree) => tree,
            Err(error) => {
                let output = task.output_record().await;
                record.finish(crate::host_platform::Terminal {
                    status: crate::host_platform::TaskStatus::Failed,
                    exit_code: None,
                    signal: None,
                    duration_ms: record.started.elapsed().as_millis() as u64,
                    failure: Some(crate::host_platform::TaskFailure::CommandFailed),
                    output,
                    note: None,
                });
                self.commands.lock().unwrap().remove(task_id);
                return Err(ProcessSpecError {
                    code: "exec_invalid_request",
                    reason: format!("Failed to spawn process: {error}"),
                });
            }
        };
        tree.hold(temporary);
        let stop_pumps = CancellationToken::new();
        let pumps = vec![
            tokio::spawn(pump(tree.stdout(), output.clone(), stop_pumps.clone())),
            tokio::spawn(pump(tree.stderr(), output.clone(), stop_pumps.clone())),
        ];
        let running = RunningCommand {
            tree,
            stop_pumps,
            pumps,
            output: output.clone(),
        };
        let worker = task.clone();
        let supervised = record.clone();
        let shutdown = self.command_shutdown.clone();
        record.start();
        self.command_workers.spawn(async move {
            running
                .supervise(supervised, &worker, timeout, shutdown)
                .await;
        });
        Ok(())
    }

    pub(crate) fn command_task(&self, task_id: &str) -> Result<Arc<CommandTask>, CaptureError> {
        match &self.task(task_id).ok_or(CaptureError::Unknown)?.kind {
            TaskKind::Command(task) => Ok(task.clone()),
            _ => Err(CaptureError::Unknown),
        }
    }

    pub async fn retain_output_by_id(&self, task_id: &str) -> Result<String, CaptureError> {
        self.command_task(task_id)?
            .output
            .ensure_file()
            .await
            .map_err(CaptureError::Unavailable)
    }
    pub async fn tail_by_id(&self, task_id: &str) -> Result<OutputTail, CaptureError> {
        let task = self.command_task(task_id)?;
        let output = &task.output;
        let capture = output.capture.lock().await;
        Ok(OutputTail {
            text: truncate_preview(&capture.preview),
            total_bytes: capture.output_bytes,
            truncated: capture.truncated,
        })
    }
}

impl OutputSink for Output {
    async fn push(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut capture = self.capture.lock().await;
        capture_preview(&mut capture.preview, text);
        if capture.failure.is_some() {
            return;
        }
        if let Err(error) = capture.process(text).await {
            capture.failure = Some(TaskFailure::OutputWriteFailed {
                message: error.to_string(),
            });
            self.failed.cancel();
        }
    }
}

/// What ended the wait for a running command.
#[derive(Clone, Debug)]
pub(super) struct CommandExit {
    pub(super) code: Option<i32>,
    pub(super) signal: Option<String>,
}
impl CommandExit {
    pub(super) fn success(&self) -> bool {
        self.signal.is_none() && self.code == Some(0)
    }
}

#[derive(Clone)]
pub(super) enum Observed {
    Cancelled,
    OwnerUnloaded,
    Exited(CommandExit),
    TimedOut,
    OutputFailed,
}

/// Summary of a settled command's output.
#[derive(Clone)]
struct Settled {
    output_path: Option<String>,
    output_bytes: u64,
    truncated: bool,
    preview: String,
    /// The preview bounds left output out of `preview`.
    preview_truncated: bool,
    inline: String,
    failure: Option<TaskFailure>,
}
impl Settled {
    /// The output text that the result shows.
    fn shown(&self) -> &str {
        match self.output_path {
            Some(_) => &self.preview,
            None => &self.inline,
        }
    }
}

pub(crate) struct CommandTask {
    pub(super) output: Arc<Output>,
    pub(super) path: PathBuf,
    pub(super) isolation: Option<Isolation>,
    pub(crate) kill: CancellationToken,
}

struct RunningCommand {
    tree: ProcessTree,
    output: Arc<Output>,
    pumps: Vec<JoinHandle<()>>,
    stop_pumps: CancellationToken,
}

impl RunningCommand {
    async fn supervise(
        mut self,
        record: Arc<TaskRecord>,
        task: &CommandTask,
        timeout: Option<u64>,
        shutdown: CancellationToken,
    ) {
        let deadline = timeout.map(|ms| record.started + Duration::from_millis(ms));
        let observed = tokio::select! {
            biased;
            status = self.tree.wait() => status
                .map(|status| {
                    let code = status.code();
                    Observed::Exited(CommandExit { code, signal: signal_name(status) })
                })
                .map_err(|error| error.to_string()),
            _ = async { tokio::time::sleep_until(deadline.expect("guarded").into()).await }, if deadline.is_some() => Ok(Observed::TimedOut),
            _ = self.output.failed.cancelled() => Ok(Observed::OutputFailed),
            _ = task.kill.cancelled() => Ok(Observed::Cancelled),
            _ = shutdown.cancelled() => Ok(Observed::OwnerUnloaded),
        };
        let termination_error = if matches!(observed, Ok(Observed::Exited(_))) {
            None
        } else {
            self.tree
                .terminate()
                .await
                .err()
                .map(|error| error.to_string())
        };
        let output = self.close().await;
        drop(self);
        record.finish(command_terminal(
            &record,
            observed,
            termination_error,
            output,
            task,
        ));
    }

    /// Starts the bounded pipe drain and waits for output writes to finish.
    async fn close(&mut self) -> Settled {
        let mut pumps = std::mem::take(&mut self.pumps);
        let mut drained = Box::pin(futures_util::future::join_all(pumps.iter_mut()));
        tokio::select! {
            _ = &mut drained => {}
            _ = tokio::time::sleep(EXIT_STDIO_GRACE) => {
                self.stop_pumps.cancel();
                drained.await;
            }
        }
        let mut capture = self.output.capture.lock().await;
        if let Some(file) = capture.file.as_mut()
            && let Err(error) = tokio::io::AsyncWriteExt::flush(file).await
            && capture.failure.is_none()
        {
            capture.failure = Some(TaskFailure::OutputWriteFailed {
                message: error.to_string(),
            });
        }
        let (preview, preview_truncated) = bounded_preview(
            &capture.preview,
            usize::try_from(capture.output_bytes).unwrap_or(usize::MAX),
        );
        Settled {
            output_path: capture.output_path(),
            output_bytes: capture.output_bytes,
            truncated: capture.truncated,
            preview,
            preview_truncated,
            inline: capture.inline.clone(),
            failure: capture.failure.clone(),
        }
    }
}

fn command_terminal(
    record: &TaskRecord,
    observed: Result<Observed, String>,
    termination_error: Option<String>,
    output: Settled,
    task: &CommandTask,
) -> Terminal {
    let (mut status, exit_code, signal, failed_exit) = match &observed {
        Ok(Observed::Exited(exit)) => (
            if exit.signal.is_some() {
                TaskStatus::Signaled
            } else {
                TaskStatus::Exited
            },
            exit.code,
            exit.signal.clone(),
            !exit.success(),
        ),
        Ok(Observed::TimedOut) => (TaskStatus::TimedOut, None, None, false),
        Ok(Observed::Cancelled) => (TaskStatus::Cancelled, None, None, false),
        Ok(Observed::OwnerUnloaded) => (TaskStatus::Cancelled, None, None, false),
        Ok(Observed::OutputFailed) | Err(_) => (TaskStatus::Failed, None, None, false),
    };
    if termination_error.is_some() || output.failure.is_some() {
        status = TaskStatus::Failed;
    }
    let note = failed_exit
        .then(|| {
            task.isolation
                .as_ref()
                .and_then(|isolation| isolation.failure_hint(output.shown()))
        })
        .flatten();
    let failure = if matches!(observed, Ok(Observed::OwnerUnloaded)) {
        Some(TaskFailure::OwnerUnloaded)
    } else {
        match &observed {
            Err(message) if termination_error.is_none() && output.failure.is_none() => {
                Some(TaskFailure::TerminateFailed {
                    message: message.clone(),
                })
            }
            _ => termination_error
                .map(|message| TaskFailure::TerminateFailed { message })
                .or_else(|| output.failure.clone()),
        }
    };
    let output = OutputRecord {
        path: task.path.to_string_lossy().into_owned(),
        bytes: output.output_bytes,
        truncated: output.truncated,
        text: output.output_path.is_none().then(|| output.inline.clone()),
        preview: output.preview.clone(),
        preview_truncated: output.preview_truncated,
    };
    Terminal {
        status,
        exit_code,
        signal,
        duration_ms: record.started.elapsed().as_millis() as u64,
        failure,
        output,
        note,
    }
}

impl CommandTask {
    pub(crate) async fn output_record(&self) -> OutputRecord {
        let capture = self.output.capture.lock().await;
        let preview = truncate_preview(&capture.preview);
        OutputRecord {
            path: self.path.to_string_lossy().into_owned(),
            bytes: capture.output_bytes,
            truncated: capture.truncated,
            text: capture.file.is_none().then(|| capture.inline.clone()),
            preview: preview.clone(),
            preview_truncated: preview.len() < capture.preview.len(),
        }
    }

    pub(crate) async fn page(
        &self,
        cursor: u64,
        limit: usize,
    ) -> Result<super::background::OutputPage, super::background::OutputPageError> {
        use super::background::{OutputPageError, PAGE_CHAR_TRAILING_BYTES, decode_page};
        use tokio::io::AsyncSeekExt;
        let capture = self.output.capture.lock().await;
        let truncated = capture.truncated;
        let total_bytes = capture.output_bytes;
        let budget = limit + PAGE_CHAR_TRAILING_BYTES;
        if capture.file.is_none() {
            let bytes = capture.inline.as_bytes();
            let start = usize::try_from(cursor)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let end = start.saturating_add(budget).min(bytes.len());
            let mut page = decode_page(&bytes[start..end], cursor, limit, truncated)?;
            page.total_bytes = total_bytes;
            return Ok(page);
        }
        drop(capture);
        let mut file = tokio::fs::File::open(&self.path)
            .await
            .map_err(|_| OutputPageError::Unavailable)?;
        file.seek(io::SeekFrom::Start(cursor))
            .await
            .map_err(|_| OutputPageError::Unavailable)?;
        let mut bytes = Vec::new();
        file.take(budget as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| OutputPageError::Unavailable)?;
        let mut page = decode_page(&bytes, cursor, limit, truncated)?;
        page.total_bytes = total_bytes;
        Ok(page)
    }
}

fn settled_from_reply(reply: &WaitReply) -> (Settled, Option<CommandExit>) {
    let settled = Settled {
        output_path: reply
            .text
            .is_none()
            .then(|| reply.output_path.clone())
            .filter(|path| std::path::Path::new(path).exists()),
        output_bytes: reply.output_bytes,
        truncated: reply.output_truncated,
        preview: reply.preview.clone(),
        preview_truncated: reply.preview_truncated,
        inline: reply.text.clone().unwrap_or_default(),
        failure: reply.failure.clone(),
    };
    let exit =
        matches!(reply.status, WaitStatus::Exited | WaitStatus::Signaled).then(|| CommandExit {
            code: reply.exit_code,
            signal: reply.signal.clone(),
        });
    (settled, exit)
}

pub(super) fn background_result(task_id: &str, reply: &WaitReply) -> (ExecStatus, ToolResult) {
    let (settled, exit) = settled_from_reply(reply);
    let (status, summary, name) = match reply.status {
        WaitStatus::Cancelled => (
            ExecStatus::Cancelled,
            "Command cancelled by user.".to_owned(),
            "cancelled",
        ),
        WaitStatus::TimedOut => (
            ExecStatus::Failed,
            "Command timed out.".to_owned(),
            "timed_out",
        ),
        _ if settled.failure.is_some() => {
            let failure = settled.failure.as_ref().expect("guarded");
            let summary = match failure {
                TaskFailure::OutputWriteFailed { message } => {
                    format!("exec output offload failed: {message}")
                }
                _ => failure.to_string(),
            };
            (ExecStatus::Failed, summary, "failed")
        }
        WaitStatus::Exited | WaitStatus::Signaled => match &exit {
            Some(exit) if exit.success() => (
                ExecStatus::Completed,
                "Command completed.".to_owned(),
                "completed",
            ),
            Some(exit) => (ExecStatus::Failed, exit_failure_summary(exit), "failed"),
            None => (
                ExecStatus::Failed,
                "Command failed without an exit code.".to_owned(),
                "failed",
            ),
        },
        WaitStatus::Failed | WaitStatus::Running => (
            ExecStatus::Failed,
            reply.failure.as_ref().map_or_else(
                || "Command failed without an exit code.".to_owned(),
                ToString::to_string,
            ),
            "failed",
        ),
    };
    let note = match (&settled.failure, &exit) {
        (None, Some(exit)) if !exit.success() => reply
            .failure_note
            .clone()
            .unwrap_or_else(|| summary.clone()),
        _ => summary.clone(),
    };
    let mut structured = object(json!({
        "type": "finished",
        "status": name,
        "taskId": task_id,
        "outputBytes": settled.output_bytes,
        "outputTruncated": settled.truncated,
        "preview": settled.preview.clone(),
        "previewTruncated": settled.preview_truncated,
        "timedOut": name == "timed_out",
        "summary": summary,
    }));
    if let Some(path) = &settled.output_path {
        structured.insert("outputPath".into(), path.clone().into());
    }
    insert_exit(&mut structured, exit.as_ref());
    let result = ToolResult {
        structured_content: Some(structured),
        is_error: (status != ExecStatus::Completed).then_some(true),
        ..ToolResult::text(settled_content(&settled, Some(&note)))
    };
    (status, result)
}

fn insert_exit(structured: &mut JsonObject, exit: Option<&CommandExit>) {
    let Some(status) = exit else { return };
    if let Some(code) = status.code {
        structured.insert("exitCode".into(), code.into());
    }
    if let Some(signal) = &status.signal {
        structured.insert("signal".into(), signal.clone().into());
    }
}

fn exit_failure_summary(status: &CommandExit) -> String {
    if let Some(code) = status.code {
        return format!("Command exited with code {code}.");
    }
    match &status.signal {
        Some(signal) => format!("Command terminated by signal {signal}."),
        None => "Command failed without an exit code.".into(),
    }
}

fn file_backed_content(
    path: &str,
    bytes: u64,
    truncated: bool,
    preview: &str,
    note: Option<&str>,
) -> String {
    let mut lines = vec![
        format!("Output saved to: {path}"),
        format!("Output bytes: {bytes}"),
    ];
    if truncated {
        lines.push(format!(
            "Output file truncated at {OUTPUT_FILE_BYTE_LIMIT} bytes."
        ));
    }
    if let Some(note) = note {
        lines.push(note.into());
    }
    lines.extend(["".into(), "Preview:".into(), preview.into()]);
    lines.join("\n")
}

fn append_note(content: &str, note: Option<&str>) -> String {
    match note {
        None => content.into(),
        Some(note) if content.is_empty() => note.into(),
        Some(note) => format!("{content}\n\n{note}"),
    }
}

fn settled_content(settled: &Settled, note: Option<&str>) -> String {
    match &settled.output_path {
        Some(path) => file_backed_content(
            path,
            settled.output_bytes,
            settled.truncated,
            &settled.preview,
            note,
        ),
        None => append_note(&settled.inline, note),
    }
}

fn running_content(reason: &str, task_id: &str, output_path: &str, preview: &str) -> String {
    [
        reason,
        &format!("Task ID: {task_id}"),
        &format!("Output: {output_path}"),
        "Stop with exec_stop({\"taskId\":\"<Task ID>\"}); this is not a Collaboration agentId.",
        "",
        "Preview:",
        preview,
    ]
    .join("\n")
}

fn offload_failed(reason: &str) -> ToolResult {
    failed(
        format!("exec output offload failed: {reason}"),
        "exec_output_offload_failed",
    )
}

/// The recovery `exec/ended` of a task whose owner restarted (§8.5).
pub(super) fn recovered_result(task_id: &str, output_path: &str) -> ToolResult {
    ToolResult {
        structured_content: Some(object(json!({
            "type": "finished",
            "status": "failed",
            "taskId": task_id,
            "outputPath": output_path,
        }))),
        ..ToolResult::error(format!(
            "Background task {task_id} did not survive a restart of its owner; its final result is unknown. Output so far: {output_path}."
        ))
    }
}

/// Model-visible request facts reused by every result of one call.
struct Call<'a> {
    task_id: String,
    command: &'a str,
    title: Option<&'a str>,
    cwd: String,
    timeout_ms: i64,
}
impl Call<'_> {
    fn structured(&self, fields: Value) -> JsonObject {
        let mut structured = object(json!({
            "taskId": self.task_id,
            "command": self.command,
            "cwd": self.cwd,
            "chunkId": self.task_id,
        }));
        structured.extend(object(fields));
        structured
    }
}

/// Executes one `exec` call on the branch of `context`; `cancel` covers the
/// call and the unloading of WorkspaceTools.
pub(super) async fn execute(
    tools: &WorkspaceTools,
    branch: &Branch,
    call: &ToolCall,
    context: &ToolExecutionContext,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let cancelled = || HostError::Cancelled("Tool execution cancelled.".into());
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let input = match parse(&call.input) {
        Ok(input) => input,
        Err((message, code)) => return Ok(failed(message, code)),
    };
    let platform = &tools.platform;
    let cwd = match &input.cwd {
        Some(value) => resolve_cwd(value, &tools.cwd, &tools.platform.home, tools.platform.os),
        None => tools.cwd.clone(),
    };
    let shell = match platform.exec_shell().await {
        ShellStatus::Available(shell) => shell,
        ShellStatus::Unavailable { code, reason } => {
            return Ok(failed(
                format!(
                    "exec shell is unavailable ({code}): {reason} Fix exec-shell.json and retry."
                ),
                "exec_shell_unavailable",
            ));
        }
    };
    if let Some(marker) = background_marker(&input.command, shell.edition) {
        let (line, column) = marker.position(&input.command);
        let token = marker.token;
        return Ok(failed(
            format!(
                "exec manages background tasks; remove shell background markers such as & and set runInBackground to true when the result can be handled later. The {token} at line {line}, column {column} starts a background job."
            ),
            "exec_invalid_request",
        ));
    }
    let cwd_text = cwd.to_string_lossy().into_owned();
    if let Some(refused) = check_heavy_search(
        &input.command,
        &cwd_text,
        &tools.platform.home.to_string_lossy(),
    ) {
        return Ok(refused);
    }
    let host_task_id = format!("exec-{}", uuid::Uuid::new_v4());
    let background = input.run_in_background && !tools.member;
    let effective_timeout = (!(input.disable_timeout && background)).then_some(input.timeout_ms);
    let request = Call {
        task_id: host_task_id.clone(),
        command: &input.command,
        title: input.title.as_deref(),
        cwd: cwd_text,
        timeout_ms: effective_timeout.map_or(-1, |timeout| timeout as i64),
    };
    let roots = match tools.ctx.get_at(&tools.facts) {
        Some(facts) => facts.0.workspace_roots(&cwd, cancel.clone()).await?,
        None => vec![tools.cwd.clone()],
    };
    let spawn = match platform
        .spawn(SpawnRequest {
            task_id: host_task_id,
            command: input.command.clone(),
            cwd: request.cwd.clone(),
            workspace: tools.cwd.to_string_lossy().into_owned(),
            roots: roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            output_dir: tools.output_dir.to_string_lossy().into_owned(),
            timeout_ms: effective_timeout,
        })
        .await
    {
        Ok(spawn) => spawn,
        Err(error) => return Ok(spawn_failed(error)),
    };
    let (result, exit) = run_process(
        tools, branch, call, context, &request, spawn, background, cancel,
    )
    .await?;
    Ok(ran(result, request.command, exit))
}

/// Runs one started command to its answer: waits for its exit, hands it to
/// the background, or stops it, and returns the result with the exit it
/// observed, when it observed one. The caller declares the call's effect
/// (session boundary A.6).
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per input a process run carries"
)]
async fn run_process(
    tools: &WorkspaceTools,
    branch: &Branch,
    call: &ToolCall,
    context: &ToolExecutionContext,
    request: &Call<'_>,
    spawn: SpawnReply,
    background: bool,
    cancel: CancellationToken,
) -> Result<(ToolResult, Option<CommandExit>), HostError> {
    let platform = &tools.platform;
    let host_task_id = spawn.task_id;
    let output_path = spawn.output_path;
    if background {
        return hand_off(
            platform,
            branch,
            call,
            context,
            request,
            &host_task_id,
            &output_path,
            None,
            true,
            &cancel,
        )
        .await;
    }
    let timeout = (!tools.member).then_some(DEFAULT_WAIT_TIME.as_millis() as u64);
    let waited = platform
        .wait(
            WaitRequest {
                task_id: host_task_id.clone(),
                timeout_ms: timeout,
            },
            cancelled_fn(&cancel),
        )
        .await;
    let reply = match waited {
        Ok(reply) if reply.status == WaitStatus::Running => {
            return hand_off(
                platform,
                branch,
                call,
                context,
                request,
                &host_task_id,
                &output_path,
                Some(reply),
                false,
                &cancel,
            )
            .await;
        }
        Ok(reply) => reply,
        Err(HostPlatformError::Cancelled) => {
            return cancel_foreground(platform, &host_task_id).await;
        }
        Err(error) => {
            stop_and_settle(platform, &host_task_id).await;
            return Ok((
                failed(format!("exec wait failed: {error}"), "exec_wait_failed"),
                None,
            ));
        }
    };
    finish_foreground(platform, request, &host_task_id, reply).await
}

async fn finish_foreground(
    platform: &HostPlatform,
    request: &Call<'_>,
    host_task_id: &str,
    reply: WaitReply,
) -> Result<(ToolResult, Option<CommandExit>), HostError> {
    let (settled, exit) = settled_from_reply(&reply);
    let _ = platform
        .settle(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await;
    let completed = reply.status == WaitStatus::Exited
        && exit.as_ref().is_some_and(CommandExit::success)
        && settled.failure.is_none();
    if let Some(TaskFailure::OutputWriteFailed { message }) = &settled.failure {
        let note = format!("exec output offload failed: {message}");
        let mut result = failed(note.clone(), "exec_output_offload_failed");
        result.content = ToolContent::Text(settled_content(&settled, Some(&note)));
        return Ok((result, None));
    }
    if reply.status == WaitStatus::Cancelled {
        return Ok((
            ToolResult::error(settled_content(&settled, Some("Command cancelled."))),
            None,
        ));
    }
    if reply.status == WaitStatus::TimedOut {
        let mut fields = json!({
            "type": "finished",
            "status": "timed_out",
            "outputBytes": settled.output_bytes,
            "outputTruncated": settled.truncated,
            "wallTimeMs": reply.duration_ms,
            "timeoutMs": request.timeout_ms,
            "timedOut": true,
        });
        if let Some(path) = &settled.output_path {
            fields["outputPath"] = path.clone().into();
        }
        return Ok((
            ToolResult {
                structured_content: Some(request.structured(fields)),
                ..ToolResult::error(settled_content(&settled, Some("Command timed out.")))
            },
            None,
        ));
    }
    let note = (!completed).then(|| {
        let base = match &exit {
            Some(exit) => exit_failure_summary(exit),
            None => settled.failure.as_ref().map_or_else(
                || "Command failed without an exit code.".to_owned(),
                ToString::to_string,
            ),
        };
        match &reply.failure_note {
            Some(hint) => format!("{base}\n\n{hint}"),
            None => base,
        }
    });
    let mut fields = object(json!({
        "type": "finished",
        "status": if completed { "completed" } else { "failed" },
        "outputBytes": settled.output_bytes,
        "outputTruncated": settled.truncated,
        "wallTimeMs": reply.duration_ms,
        "timeoutMs": request.timeout_ms,
    }));
    if let Some(path) = &settled.output_path {
        fields.insert("outputPath".into(), path.clone().into());
    }
    insert_exit(&mut fields, exit.as_ref());
    Ok((
        ToolResult {
            content: ToolContent::Text(settled_content(&settled, note.as_deref())),
            is_error: (!completed).then_some(true),
            structured_content: Some(request.structured(Value::Object(fields))),
            ..ToolResult::default()
        },
        exit,
    ))
}

async fn cancel_foreground(
    platform: &HostPlatform,
    host_task_id: &str,
) -> Result<(ToolResult, Option<CommandExit>), HostError> {
    let killed = platform
        .kill(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await;
    let reply = platform
        .wait(
            WaitRequest {
                task_id: host_task_id.to_owned(),
                timeout_ms: None,
            },
            never_cancelled(),
        )
        .await
        .ok();
    let result = match (killed, reply) {
        (Err(error), _) => failed(
            format!("Command could not be terminated: {error}"),
            "exec_terminate_failed",
        ),
        (_, Some(reply)) => {
            let (settled, _) = settled_from_reply(&reply);
            ToolResult::error(settled_content(&settled, Some("Command cancelled.")))
        }
        (_, None) => cancelled_result(),
    };
    let _ = platform
        .settle(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await;
    Ok((result, None))
}

/// Moves a running command to this branch's BackgroundExec; `immediate`
/// for `runInBackground`, otherwise after the foreground wait. Its answer
/// observed no exit: the call saw that the command started and nothing more.
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per input a handoff carries"
)]
async fn hand_off(
    platform: &HostPlatform,
    branch: &Branch,
    call: &ToolCall,
    context: &ToolExecutionContext,
    request: &Call<'_>,
    host_task_id: &str,
    output_path: &str,
    reply: Option<WaitReply>,
    immediate: bool,
    cancel: &CancellationToken,
) -> Result<(ToolResult, Option<CommandExit>), HostError> {
    let retained = match platform
        .retain_output(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await
    {
        Ok(path) => path.output_path,
        Err(error) => {
            stop_and_settle(platform, host_task_id).await;
            return Ok((offload_failed(&error.to_string()), None));
        }
    };
    if cancel.is_cancelled() {
        stop_and_settle(platform, host_task_id).await;
        return Ok((cancelled_result(), None));
    }
    let background = match branch.service::<BackgroundExec>() {
        Ok(background) => background,
        Err(error) => {
            stop_and_settle(platform, host_task_id).await;
            return Ok((start_failed(&error.to_string()), None));
        }
    };
    let started = background
        .start(StartRequest {
            turn_id: context.turn_id.clone(),
            request_id: context.request_id.clone(),
            call_id: call.id.clone(),
            task_id: request.task_id.clone(),
            host_task_id: host_task_id.to_owned(),
            description: request
                .command
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(DESCRIPTION_CHARS)
                .collect(),
            title: request.title.map(String::from),
            output_path: retained.clone(),
        })
        .await;
    let task_id = match started {
        Ok(task_id) => task_id,
        Err(StartError::Closing) => {
            return Ok((start_failed("the branch is closing"), None));
        }
        Err(StartError::Timeline(error)) => {
            return Ok((start_failed(&error.to_string()), None));
        }
    };
    let preview = match platform
        .tail(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await
    {
        Ok(tail) => tail.text,
        Err(_) => reply
            .as_ref()
            .map(|reply| reply.preview.clone())
            .unwrap_or_default(),
    };
    let wall = reply.as_ref().map(|reply| reply.duration_ms).unwrap_or(0);
    let mut structured = request.structured(json!({
        "type": "running",
        "status": "running",
        "outputPath": retained,
        "wallTimeMs": wall,
        "timeoutMs": request.timeout_ms,
        "notification": "completion-will-be-sent",
    }));
    structured.insert("taskId".into(), task_id.clone().into());
    structured.insert("chunkId".into(), task_id.clone().into());
    let reason = if immediate {
        structured.insert("runInBackground".into(), true.into());
        "Command started in background.".to_owned()
    } else {
        format!(
            "Command still running after {}ms.",
            DEFAULT_WAIT_TIME.as_millis()
        )
    };
    Ok((
        ToolResult {
            structured_content: Some(structured),
            ..ToolResult::text(running_content(&reason, &task_id, output_path, &preview))
        },
        None,
    ))
}

fn cancelled_fn(token: &CancellationToken) -> crate::host::CancelledFn {
    let token = token.clone();
    std::sync::Arc::new(move || {
        let token = token.clone();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    })
}

fn never_cancelled() -> crate::host::CancelledFn {
    let token = CancellationToken::new();
    std::sync::Arc::new(move || {
        let token = token.clone();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    })
}

async fn stop_and_settle(platform: &HostPlatform, host_task_id: &str) {
    let _ = platform
        .kill(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await;
    let _ = platform
        .wait(
            WaitRequest {
                task_id: host_task_id.to_owned(),
                timeout_ms: None,
            },
            never_cancelled(),
        )
        .await;
    let _ = platform
        .settle(TaskRequest {
            task_id: host_task_id.to_owned(),
        })
        .await;
}

fn spawn_failed(error: HostPlatformError) -> ToolResult {
    let (code, message) = match error {
        HostPlatformError::ExecShellConfigInvalid(reason)
        | HostPlatformError::ExecShellUnavailable(reason) => ("exec_shell_unavailable", reason),
        HostPlatformError::IsolationConfigInvalid(reason) => ("isolation_config_invalid", reason),
        HostPlatformError::IsolationUnavailable(reason) => ("isolation_unavailable", reason),
        HostPlatformError::Invalid(reason) => ("exec_invalid_request", reason),
        HostPlatformError::TaskExists => {
            ("task_exists", "Execution task already exists".to_owned())
        }
        HostPlatformError::Cancelled => ("cancelled", "Tool execution cancelled.".to_owned()),
        other => ("exec_invalid_request", other.to_string()),
    };
    failed(message, code)
}
fn start_failed(reason: &str) -> ToolResult {
    failed(
        format!(
            "Background task could not be registered: {reason}. The command was stopped; retry it."
        ),
        "background_start_failed",
    )
}

/// An `&` that every POSIX edition reads as a background marker.
#[cfg(test)]
const ALWAYS_REFUSED: &[&str] = &[
    "sleep 5 &",
    "sleep 5 & echo done",
    "sleep 5 & sleep 6; wait",
    "for i in 1 2; do sleep 1 & done; wait",
    "nohup python3 app.py &",
    "( sleep 5 & )",
    "{ sleep 5 & }",
    "cat <<'EOF'\nx\nEOF\nsleep 5 &",
    "# a & b\nsleep 5 &",
    "cmd1 && cmd2 &",
    "cmd 2>&1 &",
    "cat <<'EOF' | wc -l\na & b\nEOF\nsleep 5 &",
    "echo $((1&1)) &",
    "x=$(echo a & echo b)",
    "x=`echo a & echo b`",
    "sleep 5 & # start it\nwait",
    "python3 - <<'PY' &\nimport time\nPY",
    "echo \"$(sleep 5 &)\"",
    "echo \"`sleep 5 &`\"",
    "x=\"${v:-$(sleep 5 &)}\"",
    "cat <<EOF\n$(sleep 5 &)\nEOF",
    "echo \"$(sleep 5 > /dev/null &)\"",
    "python3 -c \"print(\"x\")\" ; sleep 5 &",
    "((sleep 5 &) )",
    "echo $((sleep 5 &) )",
    "((true) && (sleep 5 &))",
    "echo ${ sleep 5 & }",
    "echo ${| sleep 5 & }",
    "sleep 5 &!",
    "sleep 5 &|",
    "echo \"$((true) )\"; sleep 5 &",
    "echo \"$((true) )\" \"a & b\"; sleep 5 &",
    "cat <<EOF\nabc\\\\\nEOF\nsleep 5 &\nEOF",
    "cat <<'EOF'\nabc\\\nEOF\nsleep 5 &\nEOF",
    "cat <<EOF\nabc\\\nEOF\n'\nEOF\nsleep 5 &\necho '",
    "echo \"$\\\n(sleep 5 &)\"",
    "echo \"${v:-$\\\n(sleep 5 &)}\"",
    "# note \\\nsleep 5 &",
    "/bin/sle\\\nep 5 &",
    "cat <<EOF \\\\\nEOF\nsleep 5 &\nx=1\nEOF",
    "echo \"a\\\\\n\" ; sleep 5 & echo \"b\" \"\"",
    "echo ${v:-a\\\\\n} ; sleep 5 & echo }",
];

/// An `&` whose verdict differs by edition, in [`EDITIONS`] order: `true` is
/// refused. Every row follows the edition table on [`ShellReading`], and a
/// `false` here is a row the shell accepts without starting the job.
#[cfg(test)]
const BY_EDITION: &[(&str, [bool; 6])] = &[
    (
        "cat <<true\ntr\\\nue\nsleep 5 &\ntrue",
        [true, true, true, true, false, false],
    ),
    (
        "cat <<-true\n\ttr\\\nue\nsleep 5 &\ntrue",
        [true, true, true, true, false, false],
    ),
    (
        "echo $'a\\' ' & sleep 5 &",
        [true, true, true, true, false, true],
    ),
    ("(( sleep 5 & ))", [false, false, false, true, true, false]),
    ("(( a & b ))", [false, false, false, true, true, false]),
    ("(( (1 & 2) ))", [false, false, false, true, true, false]),
    (
        "sleep 5 &\\\n& echo hi",
        [false, false, true, false, false, true],
    ),
    (
        "echo \"$\\\n(sleep 5 &)\"",
        [true, true, true, true, true, true],
    ),
    (
        "echo \"${v:-$\\\n(sleep 5 &)}\"",
        [true, true, true, true, true, true],
    ),
    (
        "cat <\\\n<EOF\n'\nEOF\nsleep 5 &\necho '",
        [true, true, false, true, true, false],
    ),
    (
        "echo $'it\\'s & fine'",
        [false, false, false, true, true, false],
    ),
    (
        "echo ${v:-$'a\\'b'} & sleep 5 &",
        [true, true, true, true, false, true],
    ),
    ("sleep 5 |& cat", [false, false, false, true, true, true]),
    (
        "sleep 5 &> /dev/null",
        [false, false, false, true, true, true],
    ),
    (
        "sleep 5 &>> /dev/null",
        [false, false, false, true, true, true],
    ),
];

/// An `&` that no edition reads as a background marker.
#[cfg(test)]
const INERT: &[&str] = &[
    "# a & b",
    "echo hi # a & b",
    "echo hi; # a & b",
    "python3 - <<'PY'\nmask = a & b\nprint(mask)\nPY",
    "cat <<EOF\na & b\nEOF",
    "cat <<-EOF\n\ta & b\n\tEOF",
    "cat <<\\EOF\na & b\nEOF",
    "cat <<EOF\nabc\\\nEOF\n'\nEOF",
    "cat <<'true'\ntr\\\nue\n(sleep 5 &)\ntrue",
    "cat <<\"EOF\"\na & b\nEOF",
    "cat <<A <<B\nx & y\nA\na & b\nB",
    "v=$(cat <<'EOF'\na & b\nEOF\n)",
    "cat <<'EOF' | wc -l\na & b\nEOF",
    "cat <<EOF\n# a & b\nEOF",
    "cat <<'EOF'\n$(sleep 5 &)\nEOF",
    "cat <<\"EOF\"\n$(sleep 5 &)\nEOF",
    "cat <<\\EOF\n$(sleep 5 &)\nEOF",
    "cat <<EOF\n$(echo 'a & b')\nEOF",
    "while read -r l; do echo \"$l\"; done <<'EOF'\na & b\nEOF",
    "cat <<'EOF' > out.txt\na & b\nEOF",
    "x=`cat <<'EOF'\na & b\nEOF\n`",
    "echo $(( 6 & 3 ))",
    "v=$(( (n & 1) + 2 ))",
    "echo $(( (1 & 2) ))",
    "${v/&/-}",
    "echo a \\& b",
    "# don't do this & that\nls",
    "echo \"a & b\"",
    "echo 'a & b'",
    "python3 -c \"print('a & b')\"",
    "python3 -c \"print(\\\"a & b\\\")\"",
    "x=$(grep 'a & b' file.txt)",
    "x=`grep \"a & b\" file.txt`",
    "echo \"$(python3 - <<'PY'\nprint(1 & 2)\nPY\n)\"",
    "echo $(echo \"a & b\")",
    "echo \"$((true) )\" \"a & b\"",
    "echo ${v:-$(echo 'a & b')}",
    "case x in a) echo 1 ;& b) echo 2 ;; esac",
    "case x in a) echo 1 ;;& b) echo 2 ;; esac",
    "exec 3<&0",
    "cat <&3",
    "cmd 2>&1",
    "cmd1 && cmd2",
    "cmd1 || cmd2",
    "cat <<-EOF\n\t$(echo 'a & b')\n\tEOF",
];

/// Every POSIX edition exec can run.
#[cfg(test)]
const EDITIONS: &[ShellEdition] = &[
    ShellEdition::Bash,
    ShellEdition::GitBash,
    ShellEdition::Zsh,
    ShellEdition::Sh,
    ShellEdition::Dash,
    ShellEdition::Ksh,
];

#[cfg(test)]
fn refuses(script: &str, edition: ShellEdition) -> bool {
    background_marker(script, edition).is_some()
}

#[cfg(test)]
#[test]
fn the_shell_decides_which_ampersands_start_a_background_job() {
    for edition in EDITIONS {
        for script in INERT {
            assert!(
                !refuses(script, *edition),
                "{edition:?} accepts: {script:?}"
            );
        }
        for script in ALWAYS_REFUSED {
            assert!(refuses(script, *edition), "{edition:?} refuses: {script:?}");
        }
    }
    for (script, verdicts) in BY_EDITION {
        for (edition, refused) in EDITIONS.iter().zip(verdicts) {
            assert_eq!(
                refuses(script, *edition),
                *refused,
                "{edition:?} on {script:?}"
            );
        }
    }
    for script in [
        "echo \"$((true) )\"; sleep 5 &",
        "echo \"$((true) )\" \"a & b\"; sleep 5 &",
        "echo \"$((true) )\" \"a & b\"",
    ] {
        for edition in EDITIONS {
            for reading in readings(*edition) {
                let mut scan = ShellScan::new(script, *reading);
                scan.text(None);
                assert!(
                    !scan.unclosed,
                    "{edition:?} reads the arithmetic rewind to the end: {script:?}"
                );
            }
        }
    }
}

#[cfg(test)]
#[test]
fn an_unreadable_command_keeps_the_quote_only_verdict() {
    for (script, refused) in [
        ("echo \"a & b", false),
        ("echo 'a & b", false),
        ("echo $(echo 'a & b'", false),
        ("cat <<'EOF'\na & b", true),
        ("x=$(sleep 5 &", true),
        ("echo ${v &", true),
        ("x=`sleep 5 &", true),
    ] {
        assert_eq!(refuses(script, ShellEdition::Bash), refused, "{script:?}");
    }
}

/// Appending a marker to a script the scan already holds at the top level must
/// refuse it, on every edition. A scan left inside a quote or a body it never
/// left reads the appended marker as text.
#[cfg(test)]
#[test]
fn every_inert_script_leaves_the_scan_at_the_top_level() {
    for script in INERT {
        for edition in EDITIONS {
            assert!(
                refuses(&format!("{script}\nsleep 5 &"), *edition),
                "{edition:?} still accepts after a marker: {script:?}"
            );
        }
    }
    for (script, verdicts) in BY_EDITION {
        for (edition, refused) in EDITIONS.iter().zip(verdicts) {
            if *refused {
                continue;
            }
            assert!(
                refuses(&format!("{script}\nsleep 5 &"), *edition)
                    || REJECTS_THE_LINE
                        .iter()
                        .any(|(row, rejected)| row == script && rejected == edition),
                "{edition:?} still accepts after a marker: {script:?}"
            );
        }
    }
    for (row, edition) in REJECTS_THE_LINE {
        let listed = BY_EDITION.iter().any(|(script, verdicts)| {
            script == row
                && EDITIONS
                    .iter()
                    .zip(verdicts)
                    .any(|(candidate, refused)| candidate == edition && !*refused)
        });
        assert!(
            listed,
            "{edition:?} does not accept {row:?}: remove the entry from REJECTS_THE_LINE"
        );
        assert!(
            !refuses(&format!("{row}\nsleep 5 &"), *edition),
            "{edition:?} already refuses {row:?} with a marker: remove the entry from REJECTS_THE_LINE"
        );
    }
}

/// Accepted pairs whose shell rejects the whole line, so a marker appended after
/// it starts nothing.
#[cfg(test)]
const REJECTS_THE_LINE: &[(&str, ShellEdition)] = &[
    ("echo $'a\\' ' & sleep 5 &", ShellEdition::Dash),
    ("echo ${v:-$'a\\'b'} & sleep 5 &", ShellEdition::Dash),
];

/// The refusal names the marker that decided: the first one a reading refuses,
/// as written, counted in characters of the command as given.
#[cfg(test)]
#[test]
fn the_refusal_names_the_marker_that_decided() {
    for (script, edition, token, line, column) in [
        ("sleep 5 &", ShellEdition::Bash, "&", 1, 9),
        ("sleep 5 &! ", ShellEdition::Bash, "&!", 1, 9),
        ("sleep 5 &|", ShellEdition::Bash, "&|", 1, 9),
        ("sleep 5 |& cat", ShellEdition::Ksh, "|&", 1, 9),
        ("echo hi\nsleep 5 &", ShellEdition::Bash, "&", 2, 9),
        (
            "cat <<EOF\n$(sleep 5 &)\nEOF",
            ShellEdition::Bash,
            "&",
            2,
            11,
        ),
        ("cat <<'EOF'\na & b", ShellEdition::Bash, "&", 2, 3),
    ] {
        let marker = background_marker(script, edition)
            .unwrap_or_else(|| panic!("{edition:?} refuses {script:?}"));
        assert_eq!(marker.token, token, "{script:?}");
        assert_eq!(marker.position(script), (line, column), "{script:?}");
    }
}
