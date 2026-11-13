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
        EXEC_SHELL_CONFIG_INVALID, EXEC_SHELL_UNAVAILABLE, HostPlatform, ISOLATION_SUMMARY,
        Isolation, Os, ProcessTree, ShellEdition, ShellKind, TemporaryDirectory,
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
    process::ExitStatus,
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::Mutex, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

/// Default foreground wait before a command moves to the background.
const DEFAULT_WAIT_TIME: Duration = Duration::from_millis(30_000);
const DEFAULT_EXEC_TIMEOUT_MS: u64 = 300_000;
const MIN_EXEC_TIMEOUT_MS: u64 = 1_000;
const MAX_EXEC_TIMEOUT_MS: u64 = 3_600_000;
/// Foreground output stays inline until this UTF-16 length is exceeded.
const INLINE_OUTPUT_CHAR_LIMIT: usize = 30_000;
const OUTPUT_PREVIEW_MAX_LINES: usize = 2_000;
const OUTPUT_PREVIEW_MAX_BYTES: usize = 50 * 1024;
const PREVIEW_BUFFER_HEADROOM_BYTES: usize = OUTPUT_PREVIEW_MAX_BYTES * 2;
/// Maximum bytes written to one exec output file. Read by the paged output
/// read (`BackgroundExec::output_page`) as the file-level truncation signal.
pub(super) const OUTPUT_FILE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
/// Pipe reading continues this long after the root process exits.
pub const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const DESCRIPTION_CHARS: usize = 200;

pub fn definition_for(platform: &HostPlatform) -> ToolDefinition {
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
            "{env_line}\n\nExecute a command using the shell shown in the environment context. stdout and stderr are merged. Long-running commands automatically move to background.\n\n- Use read, write, and edit for known-path text file operations. Read an existing file before editing it, and derive edit replacements from the latest read result.\n- Do not use shell commands, Python, or sed for ordinary text editing when a dedicated file tool can perform the operation. Use exec for project scripts, data processing, and commands that require shell semantics.\n- Prefer scoped rg/grep/find searches when the target area is known. Avoid scanning /, home, node_modules, .git, dist, build, target, and cache directories.\n- Set timeoutMs when a command has an expected upper bound. Use a narrow timeout for searches and one-shot checks; use the default only when the command duration is uncertain.\n- Set runInBackground for tasks that don't need immediate results. For intentional long-running background processes such as dev servers, set runInBackground: true and timeoutMs: -1.\n- Use wait tool to wait a result. Do not run sleep to wait a result."
        ),
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute."},
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
            "required": ["command"]
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
        .map_err(super::host_error)?;
    match owner.stop(&input.task_id).await {
        Ok(ended) => Ok(ToolResult {
            is_error: (ended.status == ExecStatus::Failed).then_some(true),
            ..ToolResult::text(
                serde_json::to_string(&ended).expect("exec terminal contains JSON data"),
            )
        }),
        Err(super::background::TaskError::Unknown) => Ok(failed(
            "Command is not a task of this branch. Use the Task ID returned by exec on this branch; use agent for Collaboration members. No command was stopped.",
            "exec_task_unknown",
        )),
        Err(super::background::TaskError::Closed) => Err(HostError::Cancelled(
            dim_agent_runtime::CAPABILITY_RELOADING.into(),
        )),
    }
}

/// The process tree could not be confirmed gone; dropping it kills what is
/// left, but the caller must not take the command as stopped. Like the
/// background result, it keeps the output captured so far, including the
/// reference to an output file already written.
fn terminate_failed(error: &io::Error, settled: &Settled) -> ToolResult {
    let mut structured = object(json!({
        "code": "exec_terminate_failed",
        "outputBytes": settled.output_bytes,
        "outputTruncated": settled.truncated,
    }));
    if let Some(path) = &settled.output_path {
        structured.insert("outputPath".into(), path.clone().into());
    }
    let note = format!("Command could not be terminated: {error}");
    ToolResult {
        structured_content: Some(structured),
        ..ToolResult::error(settled_content(settled, Some(&note)))
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
fn ran(mut result: ToolResult, command: &str, exit: Option<ExitStatus>) -> ToolResult {
    result.effects = Some(vec![ToolEffect::CommandRun {
        command: command.to_owned(),
        exit_code: exit.and_then(|status| status.code()),
        signal: exit.and_then(signal_name),
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
                "exec accepts command, cwd, runInBackground and timeoutMs; a negative timeoutMs disables the background timeout. Remove {}.",
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
    let negative = timeout
        .and_then(Value::as_f64)
        .is_some_and(|value| value < 0.0);
    Ok(Input {
        command,
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
            single = !single;
        } else if ch == '"' && !single {
            double = !double;
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
    let tokens = tokenize_shell_command(command);
    let first = tokens.first()?;
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
    let args = &tokens[1..];
    let positional: Vec<&String> = args
        .iter()
        .filter(|token| !token.starts_with('-'))
        .collect();
    let targets: Vec<String> = match family.as_str() {
        "find" => find_starting_points(args),
        "fd" | "tree" | "du" if positional.is_empty() => vec![".".into()],
        "fd" | "tree" | "du" => positional.into_iter().cloned().collect(),
        "rg" if args.iter().any(|arg| arg == "--files") => ripgrep_files_targets(args),
        _ if positional.len() <= 1 => vec![],
        _ => positional[1..]
            .iter()
            .map(|target| (*target).clone())
            .collect(),
    };
    targets
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
    for ch in command.chars() {
        if ch == '\'' && !double {
            single = !single;
            continue;
        }
        if ch == '"' && !single {
            double = !double;
            continue;
        }
        if !single && !double && ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !single && !double && matches!(ch, ';' | '|' | '&') {
            break;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
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
            single = !single;
        } else if ch == '"' && !single {
            double = !double;
        } else if !single && (ch == '`' || (ch == '$' && chars.get(i + 1) == Some(&'('))) {
            return true;
        }
        i += 1;
    }
    false
}

fn ripgrep_files_targets(args: &[String]) -> Vec<String> {
    const WITH_VALUE: [&str; 11] = [
        "-g",
        "--glob",
        "-t",
        "--type",
        "-T",
        "--type-not",
        "--ignore-file",
        "--path-separator",
        "--sort",
        "--sortr",
        "--threads",
    ];
    const INLINE: [&str; 8] = [
        "--glob=",
        "--type=",
        "--type-not=",
        "--ignore-file=",
        "--path-separator=",
        "--sort=",
        "--sortr=",
        "--threads=",
    ];
    let mut targets = vec![];
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if token == "--files" {
            continue;
        }
        if WITH_VALUE.contains(&token.as_str()) {
            iter.next();
            continue;
        }
        if INLINE.iter().any(|prefix| token.starts_with(prefix)) || token.starts_with('-') {
            continue;
        }
        targets.push(token.clone());
    }
    if targets.is_empty() {
        vec![".".into()]
    } else {
        targets
    }
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
fn resolve_cwd(value: &str, cwd: &Path, platform: &HostPlatform) -> PathBuf {
    let expanded = if value == "~" {
        platform.home.clone()
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        platform.home.join(rest)
    } else if platform.os == Os::Windows {
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
                let tail = tail.strip_prefix('/').unwrap_or(tail);
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

/// Incremental canonical UTF-8 decoding of one stream (ADR 018): invalid and
/// truncated bytes become `<0xNN>`, never U+FFFD.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(chunk);
        self.decode(&input, false)
    }
    pub fn end(&mut self) -> String {
        let input = std::mem::take(&mut self.pending);
        self.decode(&input, true)
    }
    fn decode(&mut self, input: &[u8], last: bool) -> String {
        let mut output = String::new();
        let mut offset = 0;
        while offset < input.len() {
            let first = input[offset];
            if first <= 0x7f {
                output.push(first as char);
                offset += 1;
                continue;
            }
            let length = match first {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            if length == 0 {
                output.push_str(&escape_byte(first));
                offset += 1;
                continue;
            }
            if offset + length > input.len() {
                if !last {
                    self.pending = input[offset..].to_vec();
                    break;
                }
                output.push_str(&escape_byte(first));
                offset += 1;
                continue;
            }
            match std::str::from_utf8(&input[offset..offset + length]) {
                Ok(text) => {
                    output.push_str(text);
                    offset += length;
                }
                Err(_) => {
                    output.push_str(&escape_byte(first));
                    offset += 1;
                }
            }
        }
        output
    }
}

fn escape_byte(byte: u8) -> String {
    format!("<0x{byte:02X}>")
}

/// Removes control characters except `\t\n\r`, and U+FFF9–U+FFFB.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|&ch| {
            !(matches!(ch, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}')
                || matches!(ch, '\u{7f}'..='\u{9f}' | '\u{fff9}'..='\u{fffb}'))
        })
        .collect()
}

/// The tail of `text` within `max` UTF-8 bytes, on a character boundary.
pub fn tail_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The head of `text` within `max` UTF-8 bytes, on a character boundary.
fn head_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// TS `truncatePreview`: the last 2000 lines within 50 KiB.
pub fn truncate_preview(text: &str) -> String {
    let sanitized = sanitize(text);
    if sanitized.is_empty() {
        return sanitized;
    }
    let lines: Vec<&str> = sanitized.split('\n').collect();
    let tail = &lines[lines.len().saturating_sub(OUTPUT_PREVIEW_MAX_LINES)..];
    let joined = tail.join("\n");
    if joined.len() <= OUTPUT_PREVIEW_MAX_BYTES {
        return joined;
    }
    if tail.len() == 1 || (tail.len() == 2 && tail[1].is_empty()) {
        let suffix = if tail.len() == 2 { "\n" } else { "" };
        return format!(
            "{}{suffix}",
            tail_bytes(tail[0], OUTPUT_PREVIEW_MAX_BYTES - suffix.len())
        );
    }
    let (mut low, mut high) = (0, tail.len() - 1);
    while low < high {
        let middle = (low + high) / 2;
        if tail[middle..].join("\n").len() <= OUTPUT_PREVIEW_MAX_BYTES {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let joined = tail[low..].join("\n");
    tail_bytes(&joined, OUTPUT_PREVIEW_MAX_BYTES).to_owned()
}

/// exec's preview bounds — the sanitized tail within 2,000 lines and 50 KiB —
/// and whether they left anything out: bytes outside the captured `preview`
/// buffer, or a sanitized tail the rule cut. ShellInput's command record and a
/// background task's final result both read this one rule.
pub fn bounded_preview(preview: &str, decoded: usize) -> (String, bool) {
    let text = truncate_preview(preview);
    let truncated = decoded > preview.len() || text.len() < sanitize(preview).len();
    (text, truncated)
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
    failure: Option<String>,
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

/// Appends `text` to the bounded output tail that [`truncate_preview`] reads:
/// past twice the preview budget, only the last part from a line start stays.
pub fn capture_preview(preview: &mut String, text: &str) {
    preview.push_str(text);
    if preview.len() > PREVIEW_BUFFER_HEADROOM_BYTES {
        let keep = tail_bytes(preview, PREVIEW_BUFFER_HEADROOM_BYTES);
        let keep = keep
            .find('\n')
            .filter(|index| index + 1 < keep.len())
            .map_or(keep, |index| &keep[index + 1..]);
        *preview = keep.to_owned();
    }
}

/// Where [`pump`] delivers the decoded text of a pipe, in read order.
pub trait OutputSink: Send + Sync {
    fn push(&self, text: &str) -> impl Future<Output = ()> + Send;
}

pub(super) struct Output {
    capture: Mutex<Capture>,
    /// Cancelled when the capture failed; the command is then terminated.
    failed: CancellationToken,
}
impl Output {
    pub(super) async fn preview(&self) -> String {
        truncate_preview(&self.capture.lock().await.preview)
    }
    /// Whether the capture stopped appending at `OUTPUT_FILE_BYTE_LIMIT`.
    pub(super) async fn truncated(&self) -> bool {
        self.capture.lock().await.truncated
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
            capture.failure = Some(error.to_string());
            self.failed.cancel();
        }
    }
}

/// Reads one pipe through its own [`Decoder`] until EOF, a read error, or
/// [`EXIT_STDIO_GRACE`] of reading after `stop` (the root exited) fires.
pub async fn pump(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    output: Arc<impl OutputSink>,
    stop: CancellationToken,
) {
    let mut decoder = Decoder::default();
    let mut buffer = vec![0; 16 * 1024];
    let mut drain_budget = EXIT_STDIO_GRACE;
    loop {
        let draining = stop.is_cancelled();
        let started = Instant::now();
        let read = if draining {
            if drain_budget.is_zero() {
                break;
            }
            match tokio::time::timeout(drain_budget, pipe.read(&mut buffer)).await {
                Ok(read) => read,
                Err(_) => break,
            }
        } else {
            tokio::select! {
                read = pipe.read(&mut buffer) => read,
                _ = stop.cancelled() => continue,
            }
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let text = decoder.write(&buffer[..count]);
                if draining {
                    drain_budget = drain_budget.saturating_sub(started.elapsed());
                }
                output.push(&text).await;
            }
        }
    }
    output.push(&decoder.end()).await;
}

/// What ended the wait for a running command.
pub(super) enum Observed {
    Exited(ExitStatus),
    TimedOut,
    OutputFailed,
}

/// Summary of a settled command's output.
struct Settled {
    output_path: Option<String>,
    output_bytes: u64,
    truncated: bool,
    preview: String,
    /// The preview bounds left output out of `preview`.
    preview_truncated: bool,
    inline: String,
    failure: Option<String>,
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

/// A started command: its process tree, the pipe readers and the output.
/// One owner at a time; moved from the handler to the supervising task.
pub struct ExecProcess {
    tree: ProcessTree,
    pub(super) output: Arc<Output>,
    pumps: Vec<JoinHandle<()>>,
    stop_pumps: CancellationToken,
    started: Instant,
    deadline: Option<Instant>,
    /// Set when HostPlatform sandboxed the command.
    isolation: Option<Isolation>,
}
impl ExecProcess {
    /// Waits for the root exit, the total deadline or an output failure;
    /// cancel safe, terminates nothing. An exit already there wins.
    pub(super) async fn next(&mut self) -> io::Result<Observed> {
        let deadline = self.deadline;
        tokio::select! {
            biased;
            status = self.tree.wait() => status.map(Observed::Exited),
            _ = async { tokio::time::sleep_until(deadline.expect("guarded")).await },
                if deadline.is_some() => Ok(Observed::TimedOut),
            _ = self.output.failed.cancelled() => Ok(Observed::OutputFailed),
        }
    }
    pub(super) async fn terminate(&mut self) -> io::Result<()> {
        self.tree.terminate().await
    }
    async fn ensure_file(&self) -> Result<String, String> {
        let mut capture = self.output.capture.lock().await;
        if let Some(failure) = &capture.failure {
            return Err(failure.clone());
        }
        match capture.create_file().await {
            Ok(()) => Ok(capture.path.to_string_lossy().into_owned()),
            Err(error) => {
                let error = error.to_string();
                capture.failure = Some(error.clone());
                Err(error)
            }
        }
    }
    /// Starts the bounded pipe drain and waits for output writes to finish.
    async fn close(&mut self) -> Settled {
        self.stop_pumps.cancel();
        futures_util::future::join_all(std::mem::take(&mut self.pumps)).await;
        let mut capture = self.output.capture.lock().await;
        if let Some(file) = capture.file.as_mut()
            && let Err(error) = tokio::io::AsyncWriteExt::flush(file).await
            && capture.failure.is_none()
        {
            capture.failure = Some(error.to_string());
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
    /// Terminates, reaps and closes a process nobody will own. Preserve the
    /// failure and partial output if termination could not be confirmed.
    pub(super) async fn abandon(mut self) -> Result<(), Box<ToolResult>> {
        let terminated = self.terminate().await;
        let settled = self.close().await;
        terminated.map_err(|error| Box::new(terminate_failed(&error, &settled)))
    }
    fn wall_time_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
    /// Adds HostPlatform's note to a failed command's `note` when the command
    /// was sandboxed and its output shows a typical denial.
    fn failure_note(&self, note: String, settled: &Settled) -> String {
        let isolation = self.isolation.as_ref();
        match isolation.and_then(|isolation| isolation.failure_hint(settled.shown())) {
            Some(hint) => format!("{note}\n\n{hint}"),
            None => note,
        }
    }

    /// Supervises a background command until it ends or `cancel` fires; the
    /// tree is released before the result is returned.
    pub(super) async fn finish_background(
        mut self,
        task_id: &str,
        cancel: CancellationToken,
    ) -> (ExecStatus, ToolResult) {
        let observed = tokio::select! {
            biased;
            observed = self.next() => observed.map(Some),
            _ = cancel.cancelled() => Ok(None),
        };
        let mut unterminated = None;
        let exit = match &observed {
            Ok(Some(Observed::Exited(status))) => Some(*status),
            _ => {
                if let Err(error) = self.terminate().await {
                    tracing::warn!(%error, task_id, "background exec termination failed");
                    unterminated = Some(error);
                }
                None
            }
        };
        let settled = self.close().await;
        let (status, summary, name) = match (&observed, exit) {
            _ if let Some(error) = &unterminated => (
                ExecStatus::Failed,
                format!("Command could not be terminated: {error}"),
                "failed",
            ),
            _ if settled.failure.is_some() => (
                ExecStatus::Failed,
                format!(
                    "exec output offload failed: {}",
                    settled.failure.as_deref().unwrap_or_default()
                ),
                "failed",
            ),
            (Err(error), _) => (
                ExecStatus::Failed,
                format!("exec process wait failed: {error}"),
                "failed",
            ),
            (Ok(None), _) => (
                ExecStatus::Cancelled,
                "Command cancelled by user.".into(),
                "cancelled",
            ),
            (Ok(Some(Observed::TimedOut)), _) => {
                (ExecStatus::Failed, "Command timed out.".into(), "timed_out")
            }
            (_, Some(status)) if status.success() => (
                ExecStatus::Completed,
                "Command completed.".into(),
                "completed",
            ),
            (_, Some(status)) => (ExecStatus::Failed, exit_failure_summary(status), "failed"),
            (_, None) => (
                ExecStatus::Failed,
                "Command failed without an exit code.".into(),
                "failed",
            ),
        };
        let note = match exit {
            Some(status) if !status.success() && settled.failure.is_none() => {
                self.failure_note(summary.clone(), &settled)
            }
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
        insert_exit(&mut structured, exit);
        let content = match &settled.output_path {
            Some(path) => file_backed_content(
                path,
                settled.output_bytes,
                settled.truncated,
                &settled.preview,
                Some(&note),
            ),
            None => note,
        };
        let result = ToolResult {
            structured_content: Some(structured),
            is_error: (status != ExecStatus::Completed).then_some(true),
            ..ToolResult::text(content)
        };
        (status, result)
    }
}

fn insert_exit(structured: &mut JsonObject, exit: Option<ExitStatus>) {
    let Some(status) = exit else { return };
    if let Some(code) = status.code() {
        structured.insert("exitCode".into(), code.into());
    }
    if let Some(signal) = signal_name(status) {
        structured.insert("signal".into(), signal.into());
    }
}

/// The name of the signal that ended a process, as `exec` reports it.
#[cfg(unix)]
pub fn signal_name(status: ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal()?;
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        _ => return Some(format!("SIG{signal}")),
    };
    Some(name.into())
}
#[cfg(windows)]
pub fn signal_name(_: ExitStatus) -> Option<String> {
    None
}

fn exit_failure_summary(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("Command exited with code {code}.");
    }
    match signal_name(status) {
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
        Some(value) => resolve_cwd(value, &tools.cwd, platform),
        None => tools.cwd.clone(),
    };
    let (spec, shell) = match platform.build_process_spec(&input.command, &cwd).await {
        Ok(built) => built,
        Err(error)
            if matches!(
                error.code,
                EXEC_SHELL_CONFIG_INVALID | EXEC_SHELL_UNAVAILABLE
            ) =>
        {
            return Ok(failed(
                format!(
                    "exec shell is unavailable ({}): {} Fix exec-shell.json and retry.",
                    error.code, error.reason
                ),
                "exec_shell_unavailable",
            ));
        }
        Err(error) => return Ok(failed(error.reason, error.code)),
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
    if let Some(refused) =
        check_heavy_search(&input.command, &cwd_text, &platform.home.to_string_lossy())
    {
        return Ok(refused);
    }
    let task_id = format!("exec-{}", uuid::Uuid::new_v4());
    let background = input.run_in_background && !tools.member;
    let effective_timeout = (!(input.disable_timeout && background)).then_some(input.timeout_ms);
    let request = Call {
        task_id: task_id.clone(),
        command: &input.command,
        cwd: cwd_text,
        timeout_ms: effective_timeout.map_or(-1, |timeout| timeout as i64),
    };
    let roots = match tools.ctx.get_at(&tools.facts) {
        Some(facts) => facts.0.workspace_roots(&cwd, cancel.clone()).await?,
        None => vec![tools.cwd.clone()],
    };
    let (spec, temporary, isolation) =
        match platform.prepare_isolated_process(spec, &roots, None).await {
            Ok(prepared) => prepared,
            Err(error) => return Ok(failed(error.reason, error.code)),
        };
    let tree = match platform.spawn(spec) {
        Ok(tree) => tree,
        Err(error) => {
            return Ok(failed(
                format!("Failed to spawn process: {error}"),
                "exec_invalid_request",
            ));
        }
    };
    let (result, exit) = run_process(
        tools,
        branch,
        call,
        context,
        &request,
        tree,
        temporary,
        isolation,
        background,
        effective_timeout,
        cancel,
    )
    .await?;
    Ok(ran(result, request.command, exit))
}

/// Runs one started command to its answer: waits for its exit, hands it to
/// the background, or stops it, and returns the result with the exit it
/// observed, when it observed one. The caller declares the call's effect
/// (session boundary A.6).
#[allow(clippy::too_many_arguments)]
async fn run_process(
    tools: &WorkspaceTools,
    branch: &Branch,
    call: &ToolCall,
    context: &ToolExecutionContext,
    request: &Call<'_>,
    mut tree: ProcessTree,
    temporary: Option<TemporaryDirectory>,
    isolation: Option<Isolation>,
    background: bool,
    timeout: Option<u64>,
    cancel: CancellationToken,
) -> Result<(ToolResult, Option<ExitStatus>), HostError> {
    tree.hold(temporary);
    let output = Arc::new(Output {
        capture: Mutex::new(Capture {
            path: tools.output_dir.join(format!("{}.log", request.task_id)),
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
    let stop_pumps = CancellationToken::new();
    let started = Instant::now();
    let pumps = vec![
        tokio::spawn(pump(tree.stdout(), output.clone(), stop_pumps.clone())),
        tokio::spawn(pump(tree.stderr(), output.clone(), stop_pumps.clone())),
    ];
    let mut process = ExecProcess {
        tree,
        output,
        pumps,
        stop_pumps,
        started,
        deadline: timeout.map(|timeout| started + Duration::from_millis(timeout)),
        isolation,
    };
    if cancel.is_cancelled() {
        if let Err(result) = process.abandon().await {
            return Ok((*result, None));
        }
        return Ok((cancelled_result(), None));
    }
    if background {
        return hand_off(branch, call, context, request, process, true, &cancel).await;
    }

    let handoff = async {
        if tools.member {
            std::future::pending::<()>().await
        } else {
            tokio::time::sleep(DEFAULT_WAIT_TIME).await
        }
    };
    let observed = tokio::select! {
        biased;
        observed = process.next() => Some(observed),
        _ = cancel.cancelled() => {
            if let Err(error) = process.terminate().await {
                let settled = process.close().await;
                return Ok((terminate_failed(&error, &settled), None));
            }
            let settled = process.close().await;
            if let Some(failure) = &settled.failure {
                return Ok((offload_failed(failure), None));
            }
            return Ok((
                ToolResult::error(settled_content(&settled, Some("Command cancelled."))),
                None,
            ));
        }
        _ = handoff => None,
    };
    let wall = process.wall_time_ms();
    let observed = match observed {
        Some(observed) => Some(observed),
        None => futures_util::FutureExt::now_or_never(process.next()),
    };
    let Some(observed) = observed else {
        return hand_off(branch, call, context, request, process, false, &cancel).await;
    };
    let exit = match observed {
        Ok(Observed::Exited(status)) => status,
        Ok(Observed::OutputFailed) => {
            if let Err(error) = process.terminate().await {
                let settled = process.close().await;
                return Ok((terminate_failed(&error, &settled), None));
            }
            let settled = process.close().await;
            return Ok((
                offload_failed(settled.failure.as_deref().unwrap_or_default()),
                None,
            ));
        }
        Ok(Observed::TimedOut) => {
            if let Err(error) = process.terminate().await {
                let settled = process.close().await;
                return Ok((terminate_failed(&error, &settled), None));
            }
            let settled = process.close().await;
            if let Some(failure) = &settled.failure {
                return Ok((offload_failed(failure), None));
            }
            let mut fields = json!({
                "type": "finished",
                "status": "timed_out",
                "outputBytes": settled.output_bytes,
                "outputTruncated": settled.truncated,
                "wallTimeMs": wall,
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
        Err(error) => {
            if let Err(error) = process.terminate().await {
                let settled = process.close().await;
                return Ok((terminate_failed(&error, &settled), None));
            }
            process.close().await;
            return Ok((
                failed(
                    format!("Failed to wait for process: {error}"),
                    "exec_wait_failed",
                ),
                None,
            ));
        }
    };
    let settled = process.close().await;
    if let Some(failure) = &settled.failure {
        return Ok((offload_failed(failure), None));
    }
    let completed = exit.success();
    let note = (!completed).then(|| {
        let note = match exit.code() {
            Some(code) => format!("Command exited with code {code}."),
            None => exit_failure_summary(exit),
        };
        process.failure_note(note, &settled)
    });
    let mut fields = object(json!({
        "type": "finished",
        "status": if completed { "completed" } else { "failed" },
        "outputBytes": settled.output_bytes,
        "outputTruncated": settled.truncated,
        "wallTimeMs": wall,
        "timeoutMs": request.timeout_ms,
    }));
    if let Some(path) = &settled.output_path {
        fields.insert("outputPath".into(), path.clone().into());
    }
    insert_exit(&mut fields, Some(exit));
    Ok((
        ToolResult {
            content: ToolContent::Text(settled_content(&settled, note.as_deref())),
            is_error: (!completed).then_some(true),
            structured_content: Some(request.structured(Value::Object(fields))),
            ..ToolResult::default()
        },
        Some(exit),
    ))
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

/// Moves a running command to this branch's BackgroundExec; `immediate`
/// for `runInBackground`, otherwise after the foreground wait. Its answer
/// observed no exit: the call saw that the command started and nothing more.
async fn hand_off(
    branch: &Branch,
    call: &ToolCall,
    context: &ToolExecutionContext,
    request: &Call<'_>,
    process: ExecProcess,
    immediate: bool,
    cancel: &CancellationToken,
) -> Result<(ToolResult, Option<ExitStatus>), HostError> {
    let output_path = match process.ensure_file().await {
        Ok(path) => path,
        Err(error) => {
            if let Err(result) = process.abandon().await {
                return Ok((*result, None));
            }
            return Ok((offload_failed(&error), None));
        }
    };
    if cancel.is_cancelled() {
        if let Err(result) = process.abandon().await {
            return Ok((*result, None));
        }
        return Ok((cancelled_result(), None));
    }
    let background = match branch.service::<BackgroundExec>() {
        Ok(background) => background,
        Err(error) => {
            if let Err(result) = process.abandon().await {
                return Ok((*result, None));
            }
            return Ok((start_failed(&error.to_string()), None));
        }
    };
    let preview = process.output.preview().await;
    let wall = if immediate { 0 } else { process.wall_time_ms() };
    let started = background
        .start(
            StartRequest {
                turn_id: context.turn_id.clone(),
                request_id: context.request_id.clone(),
                call_id: call.id.clone(),
                task_id: request.task_id.clone(),
                description: request
                    .command
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(DESCRIPTION_CHARS)
                    .collect(),
                output_path: output_path.clone(),
            },
            process,
        )
        .await;
    let task_id = match started {
        Ok(task_id) => task_id,
        Err(StartError::Closing) => {
            return Ok((start_failed("the branch is closing"), None));
        }
        Err(StartError::Timeline(error)) => {
            return Ok((start_failed(&error.to_string()), None));
        }
        Err(StartError::Termination(result)) => {
            return Ok((*result, None));
        }
    };
    let mut structured = request.structured(json!({
        "type": "running",
        "status": "running",
        "outputPath": output_path,
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
            ..ToolResult::text(running_content(&reason, &task_id, &output_path, &preview))
        },
        None,
    ))
}

fn start_failed(reason: &str) -> ToolResult {
    failed(
        format!(
            "Background task could not be registered: {reason}. The command was stopped; retry it."
        ),
        "background_start_failed",
    )
}

#[cfg(test)]
#[test]
fn termination_failure_keeps_captured_output() {
    for output_path in [None, Some("/outputs/exec-partial.log".into())] {
        let settled = Settled {
            output_path,
            output_bytes: 37,
            truncated: false,
            preview: "partial stdout before termination".into(),
            preview_truncated: false,
            inline: "partial stdout before termination".into(),
            failure: None,
        };
        let result = terminate_failed(&io::Error::other("tree still alive"), &settled);
        let ToolContent::Text(content) = &result.content else {
            panic!("exec returns text");
        };
        assert!(content.contains("Command could not be terminated: tree still alive"));
        assert!(content.contains("partial stdout before termination"));
        assert_eq!(result.is_error, Some(true));
        let fields = result.structured_content.unwrap();
        assert_eq!(fields["code"], "exec_terminate_failed");
        assert_eq!(fields["outputBytes"], 37);
        assert_eq!(fields["outputTruncated"], false);
        if let Some(path) = &settled.output_path {
            assert!(content.contains(path));
            assert_eq!(fields["outputPath"], path.as_str());
        } else {
            assert!(!fields.contains_key("outputPath"));
        }
    }
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
