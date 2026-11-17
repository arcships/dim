//! `glob` and `grep` (TS `packages/plugin/src/builtin/grep-glob/`).
//!
//! The TS tools run ripgrep; these use ripgrep's libraries with the same
//! traversal (`--hidden`, ignore files, the default exclusions), the same
//! per-file and preview limits, and the same output.

#[cfg(test)]
use super::SearchHook;
use super::{
    Access, FileTools, NotRegular, Target, TargetError, blocking, cancelled, expand_tilde, failed,
    field, inside, js_len, lexical, succeeded,
};
use crate::host::HostError;
use dim_agent_types::{JsonObject, ToolResult};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Read,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// No search hook in a build without tests.
#[cfg(not(test))]
type SearchHook = std::convert::Infallible;

const SEARCH_RESULT_BYTE_LIMIT: usize = 2 * 1024 * 1024;
const SEARCH_MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const SEARCH_MAX_RESULTS: u64 = 100;
const GREP_MAX_MATCHES_PER_FILE: usize = 5;
const GREP_MAX_CONTEXT_LINES: u64 = 10;
const GLOB_QUERY_MAX_LENGTH: usize = 256;
const GLOB_DEADLINE: Duration = Duration::from_millis(5_000);
const GREP_HARD_DEADLINE_MS: u64 = 30_000;
const GREP_DEFAULT_TIMEOUT_MS: u64 = 3_000;
const GREP_MAX_LINE_LENGTH: usize = 500;
const GREP_MAX_REGEX_LENGTH: usize = 512;
const GREP_MAX_NESTING_DEPTH: usize = 32;
const EXCLUDED_DIRS: [&str; 7] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl", "node_modules"];

pub(super) fn glob_definition() -> Value {
    json!({
        "name": "glob",
        "description": "Find files with a safe case-insensitive git-style glob. Returns a bounded preview; narrow path or pattern when truncated.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 256,
                    "description": "Case-insensitive git-style glob supporting *, ?, component-only **, and character classes. The final component must contain at least two consecutive literal characters, for example *.mjs or **/*.rs. Bare *, **, and **/* are rejected. Do not use a leading ./, an absolute path, .., braces, or backslashes; choose the search directory with path.",
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory path to search in. Relative paths resolve against the current working directory.",
                },
                "limit": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Optional preview record limit from 1 to 100.",
                },
            },
            "required": ["pattern"],
        },
        "source": "builtin",
        "risk": "read_filesystem",
    })
}

pub(super) fn grep_definition() -> Value {
    let context = |description: &str| json!({ "type": "number", "minimum": 0, "maximum": 10, "description": description });
    json!({
        "name": "grep",
        "description": "Search file contents with smart-case regex or literal matching. Returns a bounded preview with at most 5 matches per file.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for using smart case. Results are a bounded preview with at most 5 matches per file. Narrow path or include when truncated. Use literal=true for plain text.",
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory path to search in. Relative paths resolve against the current working directory.",
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob pattern to limit searched files, for example \"*.{ts,tsx}\".",
                },
                "glob": { "type": "string", "description": "Alias for include." },
                "outputMode": {
                    "type": "string",
                    "enum": ["content", "files"],
                    "description": "Output style. Defaults to \"content\".",
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files"],
                    "description": "Alias for outputMode.",
                },
                "beforeContext": context("Number of context lines to show before each match."),
                "afterContext": context("Number of context lines to show after each match."),
                "context": context("Shared context line count used for both beforeContext and afterContext."),
                "-B": context("Alias for beforeContext."),
                "-A": context("Alias for afterContext."),
                "-C": context("Alias for context."),
                "lineNumbers": {
                    "type": "boolean",
                    "description": "When true, include line numbers in content output.",
                },
                "-n": { "type": "boolean", "description": "Alias for lineNumbers." },
                "limit": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Optional preview record limit from 1 to 100.",
                },
                "multiline": {
                    "type": "boolean",
                    "description": "When true, allow regex matches to span multiple lines.",
                },
                "literal": {
                    "type": "boolean",
                    "description": "When true, treat pattern as a literal string instead of a regex. Regex syntax checks are skipped; the pattern length ceiling still applies.",
                },
                "timeoutMs": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 30000,
                    "description": "Optional regex search timeout in milliseconds. Defaults to 3000.",
                },
            },
            "required": ["pattern"],
        },
        "source": "builtin",
        "risk": "read_filesystem",
    })
}

enum SearchError {
    /// Rejected input (TS `GrepGlobInputError` / `GrepGlobRegexError`).
    Input(String),
    /// Search failure (TS `GrepGlobExecutorError` codes).
    Search(&'static str, String),
    Cancelled,
}
fn input_error(message: impl Into<String>) -> SearchError {
    SearchError::Input(message.into())
}

async fn run(
    work: impl FnOnce() -> Result<ToolResult, SearchError> + Send + 'static,
) -> Result<ToolResult, HostError> {
    match blocking(work).await? {
        Ok(result) => Ok(result),
        Err(SearchError::Input(message)) => Ok(failed(message, json!({ "code": "invalid_input" }))),
        Err(SearchError::Search(code, message)) => Ok(failed(message, json!({ "code": code }))),
        Err(SearchError::Cancelled) => Err(cancelled()),
    }
}

/// Deadline and cancellation checks inside a blocking search.
struct Active {
    cancel: CancellationToken,
    deadline: Instant,
    timeout_message: &'static str,
}
impl Active {
    fn check(&self) -> Result<(), SearchError> {
        if self.cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(SearchError::Search(
                "search_timeout",
                self.timeout_message.into(),
            ));
        }
        Ok(())
    }
}

/// TS `Number.isSafeInteger` over a JSON value.
fn safe_integer(value: &Value) -> Option<i64> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0).then_some(number as i64)
}

fn preview_limit(value: Option<&Value>) -> Result<usize, SearchError> {
    let Some(value) = value else {
        return Ok(SEARCH_MAX_RESULTS as usize);
    };
    match safe_integer(value) {
        Some(limit) if (1..=SEARCH_MAX_RESULTS as i64).contains(&limit) => Ok(limit as usize),
        _ => Err(input_error("limit must be an integer from 1 to 100")),
    }
}

fn optional_path(input: &JsonObject) -> Result<Option<String>, SearchError> {
    match field(input, "path") {
        None => Ok(None),
        Some(Value::String(path)) => Ok(Some(path.clone())),
        Some(_) => Err(input_error("path must be a string")),
    }
}

enum Scope {
    File(PathBuf),
    Directory(PathBuf),
}
impl Scope {
    /// Root that result paths are relative to.
    fn output_root(&self) -> &Path {
        match self {
            Scope::File(path) => path.parent().unwrap_or(path),
            Scope::Directory(path) => path,
        }
    }
}

/// TS `resolveSearchScope`; directory searches must not be the filesystem
/// root or the user home. With `boundary` configured, `~` expands against
/// the configured home through the file tools' one tilde rule
/// (`expand_tilde`), and a scope outside the roots is refused; without one
/// a leading `~` stays a literal component under the cwd, the product's
/// long-standing behaviour.
fn resolve_scope(
    cwd: &Path,
    home: &Path,
    boundary: Option<&super::FileToolBoundary>,
    path: Option<&str>,
) -> Result<Scope, SearchError> {
    let path = path.unwrap_or("");
    let expanded = match boundary {
        Some(_) => expand_tilde(path, home).unwrap_or_else(|| PathBuf::from(path)),
        // Without a boundary the search tools have always taken a leading
        // `~` as a literal component under the cwd (`<cwd>/~`, `main`'s
        // `resolve_scope`); keep that product behaviour.
        None => PathBuf::from(path),
    };
    let absolute = lexical(&cwd.join(expanded));
    let unreadable = || {
        input_error(format!(
            "Search path is missing or unreadable: {}",
            absolute.display()
        ))
    };
    let metadata = std::fs::metadata(&absolute).map_err(|_| unreadable())?;
    if metadata.is_file() {
        std::fs::File::open(&absolute).map_err(|_| unreadable())?;
        if let Some(boundary) = boundary {
            let canonical = std::fs::canonicalize(&absolute).map_err(|_| unreadable())?;
            if !inside(boundary, &canonical, Access::Read) {
                return Err(refused_scope(&canonical));
            }
            // The bounded content read walks from the target's parent, so
            // the scope is the admitted canonical file.
            return Ok(Scope::File(canonical));
        }
        return Ok(Scope::File(absolute));
    }
    if !metadata.is_dir() {
        return Err(input_error(format!(
            "Search path is not a file or directory: {}",
            absolute.display()
        )));
    }
    std::fs::read_dir(&absolute).map_err(|_| unreadable())?;
    let canonical = std::fs::canonicalize(&absolute).map_err(|_| unreadable())?;
    if canonical.parent().is_none() || std::fs::canonicalize(home).ok() == Some(canonical.clone()) {
        return Err(input_error(
            "Open-ended searches cannot use the filesystem root or user home",
        ));
    }
    if let Some(boundary) = boundary {
        if !inside(boundary, &canonical, Access::Read) {
            return Err(refused_scope(&canonical));
        }
        // The walk starts from the admitted canonical directory, not the
        // lexical one.
        return Ok(Scope::Directory(canonical));
    }
    Ok(Scope::Directory(absolute))
}

/// The refusal for a search scope outside the configured roots.
fn refused_scope(path: &Path) -> SearchError {
    SearchError::Search(
        "outside_file_roots",
        format!(
            "Search path is outside the configured file roots: {}",
            path.display()
        ),
    )
}

fn relative(root: &Path, path: &Path) -> String {
    path.trim_prefix(root)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Default exclusions of an explicit file target.
fn excluded_file(path: &Path) -> bool {
    file_name(path).ends_with(".min.js")
        || path.parent().is_some_and(|parent| {
            parent.components().any(|component| {
                EXCLUDED_DIRS.contains(&component.as_os_str().to_string_lossy().as_ref())
            })
        })
}

/// A ripgrep-like walk: hidden files included, ignore files honored,
/// `first` as the leading override glob, then the default exclusions.
fn walk(
    root: &Path,
    first: Option<(&str, bool)>,
    max_filesize: Option<u64>,
) -> Result<ignore::Walk, SearchError> {
    let backend = |error: ignore::Error| SearchError::Search("search_backend", error.to_string());
    let mut overrides = OverrideBuilder::new(root);
    if let Some((glob, case_insensitive)) = first {
        overrides
            .case_insensitive(case_insensitive)
            .map_err(backend)?;
        overrides
            .add(glob)
            .map_err(|error| input_error(format!("Invalid glob: {error}")))?;
        overrides.case_insensitive(false).map_err(backend)?;
    }
    for dir in EXCLUDED_DIRS {
        overrides.add(&format!("!{dir}")).map_err(backend)?;
        overrides.add(&format!("!{dir}/**")).map_err(backend)?;
    }
    overrides.add("!*.min.js").map_err(backend)?;
    Ok(WalkBuilder::new(root)
        .hidden(false)
        .overrides(overrides.build().map_err(backend)?)
        .add_custom_ignore_filename(".rgignore")
        .max_filesize(max_filesize)
        .sort_by_file_name(|left, right| left.cmp(right))
        .build())
}

fn is_file(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|kind| kind.is_file())
}

pub(super) async fn glob(
    tools: &FileTools,
    input: &JsonObject,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let (cwd, home, boundary, input) = (
        tools.cwd.clone(),
        tools.home.clone(),
        tools.boundary.clone(),
        input.clone(),
    );
    run(move || run_glob(&cwd, &home, boundary.as_ref(), &input, cancel)).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GlobFile {
    label: String,
    path: String,
    fs_path: String,
}

fn run_glob(
    cwd: &Path,
    home: &Path,
    boundary: Option<&super::FileToolBoundary>,
    input: &JsonObject,
    cancel: CancellationToken,
) -> Result<ToolResult, SearchError> {
    if input.contains_key("sortBy") {
        return Err(input_error("sortBy is no longer supported"));
    }
    let pattern = match field(input, "pattern") {
        Some(Value::String(pattern)) if is_valid_glob_pattern(pattern) => pattern.clone(),
        _ => {
            return Err(input_error(
                "pattern must be a relative git-style glob with at least two consecutive literal characters in its final component, for example *.mjs or **/*.rs; bare *, **, **/*, leading ./, absolute paths, .., braces, and backslashes are not supported. Use path to choose the search directory",
            ));
        }
    };
    let limit = preview_limit(field(input, "limit"))?;
    let scope = resolve_scope(cwd, home, boundary, optional_path(input)?.as_deref())?;
    let started = Instant::now();
    let active = Active {
        cancel,
        deadline: started + GLOB_DEADLINE,
        timeout_message: "Glob search deadline expired",
    };
    active.check()?;
    let root = scope.output_root().to_owned();
    let record = |path: &Path| GlobFile {
        label: file_name(path),
        path: relative(&root, path),
        fs_path: path.to_string_lossy().into_owned(),
    };
    // `glob` returns names only: each result's canonical path is re-checked
    // against the roots before it is returned, and a path that no longer
    // canonicalizes inside is dropped.
    let keep = |path: &Path| match boundary {
        None => true,
        Some(boundary) => std::fs::canonicalize(path)
            .is_ok_and(|canonical| inside(boundary, &canonical, Access::Read)),
    };
    let mut files = Vec::new();
    let mut truncated = false;
    match &scope {
        Scope::File(path) => {
            let matcher = globset::GlobBuilder::new(&pattern)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
                .map_err(|error| input_error(format!("Invalid glob: {error}")))?
                .compile_matcher();
            if !excluded_file(path) && matcher.is_match(file_name(path)) && keep(path) {
                files.push(record(path));
            }
        }
        Scope::Directory(path) => {
            let glob = if pattern.contains('/') {
                pattern.clone()
            } else {
                format!("**/{pattern}")
            };
            for entry in walk(path, Some((&glob, true)), None)? {
                active.check()?;
                let Ok(entry) = entry else { continue };
                if !is_file(&entry) || !keep(entry.path()) {
                    continue;
                }
                if files.len() >= limit {
                    truncated = true;
                    break;
                }
                files.push(record(entry.path()));
            }
        }
    }
    active.check()?;
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let content = if truncated {
        format!(
            "Matched at least {0} files. Showing {0} files. More files may remain; narrow path or pattern and retry.\n\n{1}",
            files.len(),
            paths.join("\n")
        )
    } else if files.is_empty() {
        "Matched 0 files. Showing 0 files. Not truncated.".to_owned()
    } else {
        format!(
            "Matched {0} files. Showing {0} files. Not truncated.\n\n{1}",
            files.len(),
            paths.join("\n")
        )
    };
    Ok(succeeded(
        content,
        json!({
            "files": files,
            "truncated": truncated,
            "durationMs": started.elapsed().as_millis() as u64,
        }),
    ))
}

/// TS `isValidGlobPattern`: a safe, anchored git-style glob.
fn is_valid_glob_pattern(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(1..=GLOB_QUERY_MAX_LENGTH).contains(&js_len(value))
        || value.trim().is_empty()
        || value
            .chars()
            .any(|c| c <= '\u{1f}' || matches!(c, '\u{7f}' | '\\' | '{' | '}'))
        || value.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
        || value.starts_with('!')
        || ["?(", "*(", "+(", "@(", "!("]
            .iter()
            .any(|extglob| value.contains(extglob))
    {
        return false;
    }
    let components: Vec<&str> = value.split('/').collect();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return false;
    }
    for component in &components {
        let chars: Vec<char> = component.chars().collect();
        if (component.contains("**") && *component != "**") || !has_valid_classes(&chars) {
            return false;
        }
    }
    let last = components[components.len() - 1];
    last != "**" && has_literal_anchor(&last.chars().collect::<Vec<_>>())
}

/// End index of a well-formed character class starting at `start`.
fn class_end(chars: &[char], start: usize) -> Option<usize> {
    let mut body_start = start + 1;
    let negated = matches!(chars.get(body_start), Some('!' | '^'));
    if negated {
        body_start += 1;
    }
    let search_from = body_start + usize::from(chars.get(body_start) == Some(&']'));
    let close = search_from + chars.get(search_from..)?.iter().position(|c| *c == ']')?;
    let body: String = chars[body_start..close].iter().collect();
    if body.is_empty() || body.contains('[') {
        return None;
    }
    let body = match body.strip_prefix(']') {
        Some(rest) => format!("\\]{rest}"),
        None => body,
    };
    let source = format!("[{}{body}]", if negated { "^" } else { "" });
    regex::Regex::new(&source).ok()?;
    Some(close + 1)
}

fn has_valid_classes(chars: &[char]) -> bool {
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            ']' => return false,
            '[' => match class_end(chars, index) {
                Some(end) => index = end,
                None => return false,
            },
            _ => index += 1,
        }
    }
    true
}

/// At least two consecutive literal characters in the final component.
fn has_literal_anchor(chars: &[char]) -> bool {
    let mut consecutive = 0;
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '[' => {
                consecutive = 0;
                index = class_end(chars, index).expect("classes were validated");
            }
            '*' | '?' => {
                consecutive = 0;
                index += 1;
            }
            _ => {
                consecutive += 1;
                if consecutive >= 2 {
                    return true;
                }
                index += 1;
            }
        }
    }
    false
}

pub(super) async fn grep(
    tools: &FileTools,
    input: &JsonObject,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let (cwd, home, boundary, input) = (
        tools.cwd.clone(),
        tools.home.clone(),
        tools.boundary.clone(),
        input.clone(),
    );
    #[cfg(test)]
    let hook = tools.before_search.clone();
    #[cfg(not(test))]
    let hook = None;
    run(move || run_grep(&cwd, &home, boundary.as_ref(), &input, cancel, hook)).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Content,
    Files,
}

struct GrepRequest {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    mode: OutputMode,
    before: usize,
    after: usize,
    line_numbers: bool,
    multiline: bool,
    literal: bool,
    limit: usize,
    timeout_ms: u64,
}

/// TS `normalizeGrepInput`, aliases included.
fn normalize_grep_input(input: &JsonObject) -> Result<GrepRequest, SearchError> {
    for removed in ["ignoreCase", "-i", "offset", "headLimit", "head_limit"] {
        if input.contains_key(removed) {
            return Err(input_error(format!("{removed} is no longer supported")));
        }
    }
    let pattern = match field(input, "pattern") {
        Some(Value::String(pattern)) if !pattern.trim().is_empty() => pattern.clone(),
        _ => {
            return Err(input_error(
                "pattern is required and must be a non-empty string",
            ));
        }
    };
    let path = optional_path(input)?;
    let first = |keys: &[&str]| keys.iter().find_map(|key| field(input, key));
    let include = match first(&["include", "glob"]) {
        None => None,
        Some(Value::String(include)) => Some(include.clone()),
        Some(_) => return Err(input_error("include must be a string")),
    };
    let mode = match first(&["outputMode", "output_mode"]) {
        None => OutputMode::Content,
        Some(value) if value == "content" => OutputMode::Content,
        Some(value) if value == "files" => OutputMode::Files,
        Some(_) => return Err(input_error("outputMode must be \"content\" or \"files\"")),
    };
    let shared = first(&["context", "-C"]);
    let context_limit = |value: Option<&Value>, name: &str| match value {
        None => Ok(0),
        Some(value) => match safe_integer(value) {
            Some(lines) if (0..=GREP_MAX_CONTEXT_LINES as i64).contains(&lines) => {
                Ok(lines as usize)
            }
            _ => Err(input_error(format!(
                "{name} must be an integer from 0 to 10"
            ))),
        },
    };
    let before = context_limit(
        shared.or_else(|| first(&["beforeContext", "-B"])),
        "beforeContext",
    )?;
    let after = context_limit(
        shared.or_else(|| first(&["afterContext", "-A"])),
        "afterContext",
    )?;
    let flag = |value: Option<&Value>, name: &str| match value {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(input_error(format!("{name} must be a boolean"))),
    };
    let line_numbers = flag(first(&["lineNumbers", "-n"]), "lineNumbers")?;
    let multiline = flag(field(input, "multiline"), "multiline")?;
    let literal = flag(field(input, "literal"), "literal")?;
    let limit = preview_limit(field(input, "limit"))?;
    let timeout_ms = match field(input, "timeoutMs") {
        None => GREP_DEFAULT_TIMEOUT_MS,
        Some(value) => match safe_integer(value) {
            Some(timeout) if (1..=GREP_HARD_DEADLINE_MS as i64).contains(&timeout) => {
                timeout as u64
            }
            _ => {
                return Err(input_error("timeoutMs must be an integer from 1 to 30000"));
            }
        },
    };
    Ok(GrepRequest {
        pattern,
        path,
        include,
        mode,
        before,
        after,
        line_numbers,
        multiline,
        literal,
        limit,
        timeout_ms,
    })
}

static LOOKBEHIND: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\(\?<[=!]([^)]*)\)").unwrap());
static RANGE_QUANTIFIER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\{[^}]*,").unwrap());
static BACKREFERENCE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\\[1-9]|\\k<").unwrap());
static CATASTROPHIC: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\([^)]*[.|*+][^)]*\)[*+]{1,}|\([^)]*\|[^)]*\)[*+]{1,}").unwrap()
});

/// TS `validateRegexSafety`.
fn validate_regex_safety(pattern: &str) -> Result<(), SearchError> {
    if js_len(pattern) > GREP_MAX_REGEX_LENGTH {
        return Err(input_error(format!(
            "Pattern exceeds maximum length of {GREP_MAX_REGEX_LENGTH} characters"
        )));
    }
    for captures in LOOKBEHIND.captures_iter(pattern) {
        let inner = &captures[1];
        if inner.contains(['*', '+']) || RANGE_QUANTIFIER.is_match(inner) {
            return Err(input_error(
                "Variable-length lookbehind assertions are not allowed",
            ));
        }
    }
    if BACKREFERENCE.is_match(pattern) {
        return Err(input_error(
            "Backreferences (\\1, \\k<name>) are not allowed",
        ));
    }
    let (mut depth, mut max_depth) = (0usize, 0usize);
    for c in pattern.chars() {
        if c == '(' {
            depth += 1;
            max_depth = max_depth.max(depth);
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        }
    }
    if max_depth > GREP_MAX_NESTING_DEPTH {
        return Err(input_error(format!(
            "Pattern nesting depth {max_depth} exceeds maximum of {GREP_MAX_NESTING_DEPTH}"
        )));
    }
    if CATASTROPHIC.is_match(pattern) {
        return Err(input_error(
            "Potentially catastrophic quantifier pattern detected (group with alternation/wildcard + broad quantifier)",
        ));
    }
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ContentMatch {
    file: String,
    line: u64,
    column: usize,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    before_start_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<MatchContext>,
}
#[derive(Serialize, Clone, Default)]
struct MatchContext {
    before: Vec<String>,
    after: Vec<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileCount {
    file: String,
    match_count: usize,
}
#[derive(Serialize)]
struct SkippedFile {
    file: String,
    reason: &'static str,
}

fn json_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// Serialized size of one result array: element bytes plus separators.
#[derive(Default, Clone, Copy)]
struct ArraySize {
    bytes: usize,
    count: usize,
}
impl ArraySize {
    fn with(self, bytes: usize) -> Self {
        Self {
            bytes: self.bytes + bytes,
            count: self.count + 1,
        }
    }
    fn total(self) -> usize {
        self.bytes + self.count.saturating_sub(1)
    }
}

/// Collected results with the TS preview limits and byte budget.
struct Results {
    limit: usize,
    base_bytes: usize,
    matches: Vec<ContentMatch>,
    files: Vec<FileCount>,
    skipped: Vec<SkippedFile>,
    sizes: [ArraySize; 3],
    truncated: bool,
    /// Stop the whole search.
    frozen: bool,
}
impl Results {
    fn new(mode: OutputMode, limit: usize) -> Self {
        let base = match mode {
            OutputMode::Content => json!({
                "outputMode": "content", "matches": [], "truncated": true,
                "skippedFiles": [], "durationMs": 9_007_199_254_740_991u64,
            }),
            OutputMode::Files => json!({
                "outputMode": "files", "files": [], "truncated": true,
                "skippedFiles": [], "durationMs": 9_007_199_254_740_991u64,
            }),
        };
        Self {
            limit,
            base_bytes: json_len(&base),
            matches: Vec::new(),
            files: Vec::new(),
            skipped: Vec::new(),
            sizes: [ArraySize::default(); 3],
            truncated: false,
            frozen: false,
        }
    }
    fn fits(&self, sizes: [ArraySize; 3]) -> bool {
        self.base_bytes + sizes.iter().map(|size| size.total()).sum::<usize>()
            <= SEARCH_RESULT_BYTE_LIMIT
    }
    fn stop_for_budget(&mut self) -> Result<bool, SearchError> {
        if self.matches.is_empty() && self.files.is_empty() {
            return Err(SearchError::Search(
                "search_limit",
                "Grep result exceeds byte limit".into(),
            ));
        }
        self.truncated = true;
        self.frozen = true;
        Ok(true)
    }
    /// Returns whether the search must stop.
    fn add_match(&mut self, record: ContentMatch) -> Result<bool, SearchError> {
        if self.matches.len() >= self.limit {
            self.truncated = true;
            self.frozen = true;
            return Ok(true);
        }
        let mut sizes = self.sizes;
        sizes[0] = sizes[0].with(json_len(&record));
        if !self.fits(sizes) {
            return self.stop_for_budget();
        }
        self.sizes = sizes;
        self.matches.push(record);
        Ok(false)
    }
    fn add_file(&mut self, record: FileCount) -> Result<bool, SearchError> {
        if self.files.len() >= self.limit {
            self.truncated = true;
            self.frozen = true;
            return Ok(true);
        }
        let mut sizes = self.sizes;
        sizes[1] = sizes[1].with(json_len(&record));
        if !self.fits(sizes) {
            return self.stop_for_budget();
        }
        self.sizes = sizes;
        self.files.push(record);
        Ok(false)
    }
    fn add_skipped(&mut self, file: String, reason: &'static str) -> Result<bool, SearchError> {
        let record = SkippedFile { file, reason };
        let mut sizes = self.sizes;
        sizes[2] = sizes[2].with(json_len(&record));
        if !self.fits(sizes) {
            return self.stop_for_budget();
        }
        self.sizes = sizes;
        self.skipped.push(record);
        Ok(false)
    }
}

fn run_grep(
    cwd: &Path,
    home: &Path,
    boundary: Option<&super::FileToolBoundary>,
    input: &JsonObject,
    cancel: CancellationToken,
    hook: Option<SearchHook>,
) -> Result<ToolResult, SearchError> {
    #[cfg(not(test))]
    let _ = &hook;
    let request = normalize_grep_input(input)?;
    if request.literal {
        if js_len(&request.pattern) > GREP_MAX_REGEX_LENGTH {
            return Err(input_error(format!(
                "Pattern exceeds maximum length of {GREP_MAX_REGEX_LENGTH} characters"
            )));
        }
    } else {
        validate_regex_safety(&request.pattern)?;
    }
    let mut builder = RegexMatcherBuilder::new();
    builder.case_smart(true).fixed_strings(request.literal);
    if request.multiline {
        builder.multi_line(true).dot_matches_new_line(true);
    } else {
        builder.line_terminator(Some(b'\n'));
    }
    let matcher = builder
        .build(&request.pattern)
        .map_err(|error| input_error(format!("Invalid regex pattern: {error}")))?;
    let scope = resolve_scope(cwd, home, boundary, request.path.as_deref())?;
    let started = Instant::now();
    let active = Active {
        cancel,
        deadline: started + Duration::from_millis(request.timeout_ms),
        timeout_message: "Grep search deadline expired",
    };
    active.check()?;
    let root = scope.output_root().to_owned();
    let mut results = Results::new(request.mode, request.limit);
    let mut search = FileSearch {
        request: &request,
        matcher: &matcher,
        active: &active,
        searcher: SearcherBuilder::new()
            .line_number(true)
            .before_context(request.before)
            .after_context(request.after)
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .build(),
    };
    let bounded = boundary.is_some();
    match &scope {
        Scope::File(path) => {
            let include_matches = match &request.include {
                None => true,
                Some(include) => globset::GlobBuilder::new(include)
                    .literal_separator(true)
                    .build()
                    .map_err(|error| input_error(format!("Invalid glob: {error}")))?
                    .compile_matcher()
                    .is_match(file_name(path)),
            };
            let file = relative(&root, path);
            if excluded_file(path) || !include_matches {
            } else if std::fs::metadata(path)
                .is_ok_and(|metadata| metadata.len() > SEARCH_MAX_FILE_SIZE_BYTES)
            {
                results.add_skipped(file, "too_large")?;
            } else {
                search.file(path, file, &mut results, bounded)?;
            }
        }
        Scope::Directory(path) => {
            let include = request.include.as_deref().map(|include| (include, false));
            for entry in walk(path, include, Some(SEARCH_MAX_FILE_SIZE_BYTES))? {
                active.check()?;
                let Ok(entry) = entry else { continue };
                #[cfg(test)]
                if let Some(hook) = &hook {
                    hook(entry.path());
                }
                if !is_file(&entry) {
                    continue;
                }
                search.file(
                    entry.path(),
                    relative(&root, entry.path()),
                    &mut results,
                    bounded,
                )?;
                if results.frozen {
                    break;
                }
            }
        }
    }
    active.check()?;
    let duration = started.elapsed().as_millis() as u64;
    Ok(match request.mode {
        OutputMode::Content => succeeded(
            format_content(&results.matches, request.line_numbers, results.truncated),
            json!({
                "matches": results.matches,
                "truncated": results.truncated,
                "skippedFiles": results.skipped,
                "durationMs": duration,
            }),
        ),
        OutputMode::Files => succeeded(
            format_files(&results.files, results.truncated),
            json!({
                "files": results.files,
                "truncated": results.truncated,
                "skippedFiles": results.skipped,
                "durationMs": duration,
            }),
        ),
    })
}

/// Opens one walked file for its content through the same symlink-free
/// walk `read` uses, so a path swapped for an outside symlink after the
/// walk listed it is not read. `None` is a file whose path no longer walks
/// cleanly; the caller skips it like any other unreadable file.
fn open_bounded(path: &Path) -> Result<Option<std::fs::File>, std::io::Error> {
    let target = match Target::for_read(path, true) {
        Ok(target) => target,
        Err(TargetError::Refused) => return Ok(None),
        Err(TargetError::Io(error)) => return Err(error),
    };
    match target.open_regular() {
        Ok(Ok(file)) => Ok(Some(file)),
        Ok(Err(NotRegular)) => Ok(None),
        Err(TargetError::Refused) => Ok(None),
        Err(TargetError::Io(error)) => Err(error),
    }
}

/// The skip reason an unreadable file reports, as the search always has.
fn unreadable(error: &std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        "permission_denied"
    } else {
        "read_error"
    }
}

struct FileSearch<'a> {
    request: &'a GrepRequest,
    matcher: &'a RegexMatcher,
    active: &'a Active,
    searcher: Searcher,
}
impl FileSearch<'_> {
    fn file(
        &mut self,
        path: &Path,
        file: String,
        results: &mut Results,
        bounded: bool,
    ) -> Result<(), SearchError> {
        if self.request.multiline {
            return self.multiline_file(path, file, results, bounded);
        }
        let opened = if bounded {
            match open_bounded(path) {
                Ok(Some(opened)) => Some(opened),
                Ok(None) => {
                    results.add_skipped(file, "read_error")?;
                    return Ok(());
                }
                Err(error) => {
                    results.add_skipped(file, unreadable(&error))?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        let mut sink = FileSink {
            request: self.request,
            matcher: self.matcher,
            active: self.active,
            results,
            file,
            active_file: false,
            raw: 0,
            eligible: 0,
            current: Vec::new(),
            prefix_committed: false,
            before: VecDeque::new(),
            last: None,
            binary: false,
            failure: None,
        };
        let searched = match opened {
            Some(opened) => self.searcher.search_reader(self.matcher, opened, &mut sink),
            None => self.searcher.search_path(self.matcher, path, &mut sink),
        };
        if let Some(failure) = sink.failure.take() {
            return Err(failure);
        }
        if let Err(error) = searched
            && !sink.active_file
        {
            let reason = unreadable(&error);
            let file = std::mem::take(&mut sink.file);
            sink.results.add_skipped(file, reason)?;
            return Ok(());
        }
        sink.end()
    }

    /// Multi-line matching over the whole file (the TS JavaScript path).
    fn multiline_file(
        &mut self,
        path: &Path,
        file: String,
        results: &mut Results,
        bounded: bool,
    ) -> Result<(), SearchError> {
        let bytes = match if bounded {
            open_bounded(path).and_then(|opened| {
                let Some(mut opened) = opened else {
                    return Err(std::io::Error::other("the path no longer walks cleanly"));
                };
                let mut bytes = Vec::new();
                opened.read_to_end(&mut bytes).map(|_| bytes)
            })
        } else {
            std::fs::read(path)
        } {
            Ok(bytes) => bytes,
            Err(error) => {
                results.add_skipped(file, unreadable(&error))?;
                return Ok(());
            }
        };
        self.active.check()?;
        let text = String::from_utf8_lossy(&bytes);
        if text.contains('\0') {
            results.add_skipped(file, "binary")?;
            return Ok(());
        }
        let haystack = text.as_bytes();
        let mut found = Vec::new();
        let mut at = 0;
        while at <= haystack.len() && found.len() < GREP_MAX_MATCHES_PER_FILE {
            self.active.check()?;
            let Ok(Some(found_match)) = self.matcher.find_at(haystack, at) else {
                break;
            };
            let start = text.floor_char_boundary(found_match.start());
            let end = if found_match.is_empty() {
                start
            } else {
                text.ceil_char_boundary(found_match.end())
            };
            let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
            found.push(ContentMatch {
                file: file.clone(),
                line: text[..start].matches('\n').count() as u64 + 1,
                column: js_len(&text[line_start..start]) + 1,
                content: text[start..end].to_owned(),
                before_start_line: None,
                context: None,
            });
            at = if found_match.is_empty() {
                found_match.end() + 1
            } else {
                found_match.end()
            };
        }
        if found.len() >= GREP_MAX_MATCHES_PER_FILE {
            results.truncated = true;
        }
        if self.request.mode == OutputMode::Files {
            if !found.is_empty() {
                results.add_file(FileCount {
                    file,
                    match_count: found.len(),
                })?;
            }
            return Ok(());
        }
        let lines: Vec<&str> = text
            .split('\n')
            .map(|line| line.trim_suffix('\r'))
            .collect();
        let (before, after) = (self.request.before as u64, self.request.after as u64);
        let mut last_output = 0u64;
        for mut record in found {
            if before > 0 || after > 0 {
                let start = (record.line.saturating_sub(before))
                    .max(1)
                    .max(last_output + 1);
                let after_end = (record.line + after).min(lines.len() as u64);
                let range = |from: u64, to: u64| -> Vec<String> {
                    (from..=to)
                        .filter(|line| *line >= 1)
                        .map(|line| lines[line as usize - 1].to_owned())
                        .collect()
                };
                let before_lines = if start < record.line {
                    range(start, record.line - 1)
                } else {
                    Vec::new()
                };
                let after_lines = if record.line < after_end {
                    range(record.line + 1, after_end)
                } else {
                    Vec::new()
                };
                last_output = last_output.max(after_end);
                if !before_lines.is_empty() || !after_lines.is_empty() {
                    if !before_lines.is_empty() {
                        record.before_start_line = Some(start);
                    }
                    record.context = Some(MatchContext {
                        before: before_lines,
                        after: after_lines,
                    });
                }
            }
            if results.add_match(record)? {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Per-file consumer of line-oriented search events (TS ripgrep JSON consumer).
struct FileSink<'a> {
    request: &'a GrepRequest,
    matcher: &'a RegexMatcher,
    active: &'a Active,
    results: &'a mut Results,
    file: String,
    /// A match or context line was reported (ripgrep's `begin`).
    active_file: bool,
    raw: usize,
    eligible: usize,
    current: Vec<ContentMatch>,
    prefix_committed: bool,
    before: VecDeque<(u64, String)>,
    last: Option<usize>,
    binary: bool,
    failure: Option<SearchError>,
}
impl FileSink<'_> {
    fn fail(&mut self, error: SearchError) -> bool {
        self.failure = Some(error);
        false
    }
    /// Maps a stop decision to the sink's continue flag.
    fn step(&mut self, outcome: Result<bool, SearchError>) -> bool {
        match outcome {
            Ok(stop) => !stop,
            Err(error) => self.fail(error),
        }
    }
    fn commit_current(&mut self) -> Result<bool, SearchError> {
        for record in std::mem::take(&mut self.current) {
            if self.results.add_match(record)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn commit_file(&mut self) -> Result<bool, SearchError> {
        if self.eligible == 0 {
            return Ok(false);
        }
        self.results.add_file(FileCount {
            file: self.file.clone(),
            match_count: self.eligible,
        })
    }
    fn commit(&mut self) -> Result<bool, SearchError> {
        match self.request.mode {
            OutputMode::Content => self.commit_current(),
            OutputMode::Files => self.commit_file(),
        }
    }
    /// The per-file match ceiling was reached: keep the prefix, mark truncation.
    fn commit_prefix(&mut self) -> Result<bool, SearchError> {
        let stopped = self.commit()?;
        self.prefix_committed = true;
        self.results.truncated = true;
        self.last = None;
        self.before.clear();
        Ok(stopped)
    }
    fn end(mut self) -> Result<(), SearchError> {
        if !self.active_file || self.prefix_committed {
            return Ok(());
        }
        if self.binary {
            let file = std::mem::take(&mut self.file);
            self.results.add_skipped(file, "binary")?;
            return Ok(());
        }
        self.commit()?;
        Ok(())
    }
    fn line_text(bytes: &[u8]) -> String {
        let bytes = bytes.trim_suffix(b"\n");
        let bytes = bytes.trim_suffix(b"\r");
        String::from_utf8_lossy(bytes).into_owned()
    }
    fn after_of_last(&self, line: u64) -> Option<usize> {
        self.last.filter(|index| {
            let last = self.current[*index].line;
            line > last && line <= last + self.request.after as u64
        })
    }
    fn on_match(&mut self, line: u64, bytes: &[u8]) -> Result<bool, SearchError> {
        self.raw += 1;
        let text = Self::line_text(bytes);
        if js_len(&text) <= GREP_MAX_LINE_LENGTH {
            self.eligible += 1;
            if self.request.mode == OutputMode::Content {
                if let Some(index) = self.after_of_last(line) {
                    let last = &mut self.current[index];
                    last.context
                        .get_or_insert_with(MatchContext::default)
                        .after
                        .push(text.clone());
                }
                let owned: Vec<(u64, String)> = self
                    .before
                    .drain(..)
                    .filter(|(number, _)| {
                        *number + self.request.before as u64 >= line && *number < line
                    })
                    .collect();
                let start = self
                    .matcher
                    .find(text.as_bytes())
                    .ok()
                    .flatten()
                    .map_or(0, |found| found.start());
                let start = text.floor_char_boundary(start);
                let mut record = ContentMatch {
                    file: self.file.clone(),
                    line,
                    column: js_len(&text[..start]) + 1,
                    content: text,
                    before_start_line: None,
                    context: None,
                };
                if let Some((first, _)) = owned.first() {
                    record.before_start_line = Some(*first);
                    record.context = Some(MatchContext {
                        before: owned.into_iter().map(|(_, text)| text).collect(),
                        after: Vec::new(),
                    });
                }
                self.current.push(record);
                self.last = Some(self.current.len() - 1);
                if self.results.matches.len() + self.current.len() > self.request.limit {
                    return self.commit_current();
                }
            }
        }
        if self.raw >= GREP_MAX_MATCHES_PER_FILE {
            self.commit_prefix()?;
            return Ok(true);
        }
        Ok(false)
    }
    fn on_context(&mut self, line: u64, bytes: &[u8]) {
        if self.request.mode != OutputMode::Content || self.prefix_committed {
            return;
        }
        let text = Self::line_text(bytes);
        if let Some(index) = self.after_of_last(line) {
            self.current[index]
                .context
                .get_or_insert_with(MatchContext::default)
                .after
                .push(text);
        } else {
            self.before.push_back((line, text));
            if self.before.len() > self.request.before {
                self.before.pop_front();
            }
        }
    }
}
impl Sink for FileSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, found: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if let Err(error) = self.active.check() {
            return Ok(self.fail(error));
        }
        self.active_file = true;
        let outcome = self.on_match(found.line_number().unwrap_or(0), found.bytes());
        Ok(self.step(outcome) && !self.results.frozen)
    }

    fn context(&mut self, _: &Searcher, context: &SinkContext<'_>) -> Result<bool, Self::Error> {
        if let Err(error) = self.active.check() {
            return Ok(self.fail(error));
        }
        self.active_file = true;
        self.on_context(context.line_number().unwrap_or(0), context.bytes());
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, Self::Error> {
        self.binary = true;
        Ok(false)
    }
}

fn format_content(matches: &[ContentMatch], line_numbers: bool, truncated: bool) -> String {
    let mut shown: Vec<&str> = Vec::new();
    for record in matches {
        if !shown.contains(&record.file.as_str()) {
            shown.push(&record.file);
        }
    }
    let heading = if truncated {
        format!(
            "Matched at least {} records in {} shown files. More matches may remain; narrow path or include and retry.",
            matches.len(),
            shown.len()
        )
    } else {
        format!(
            "Matched {} records in {} shown files. Not truncated.",
            matches.len(),
            shown.len()
        )
    };
    if matches.is_empty() {
        return heading;
    }
    let render = |number: u64, text: &str| {
        if line_numbers {
            format!("  {number}| {text}")
        } else {
            format!("  {text}")
        }
    };
    let mut lines = Vec::new();
    for file in shown {
        lines.push(file.to_owned());
        for record in matches.iter().filter(|record| record.file == file) {
            let empty = MatchContext::default();
            let context = record.context.as_ref().unwrap_or(&empty);
            let before_start = record
                .before_start_line
                .unwrap_or(record.line - context.before.len() as u64);
            for (index, text) in context.before.iter().enumerate() {
                lines.push(render(before_start + index as u64, text));
            }
            lines.push(render(record.line, &record.content));
            for (index, text) in context.after.iter().enumerate() {
                lines.push(render(record.line + index as u64 + 1, text));
            }
        }
    }
    format!("{heading}\n\n{}", lines.join("\n"))
}

fn format_files(files: &[FileCount], truncated: bool) -> String {
    let heading = if truncated {
        format!(
            "Matched at least {0} files in {0} shown files. More matches may remain; narrow path or include and retry.",
            files.len()
        )
    } else {
        format!(
            "Matched {0} files in {0} shown files. Not truncated.",
            files.len()
        )
    };
    if files.is_empty() {
        return heading;
    }
    let lines: Vec<String> = files
        .iter()
        .map(|file| {
            let plus = if file.match_count == GREP_MAX_MATCHES_PER_FILE {
                "+"
            } else {
                ""
            };
            format!("{}: {}{plus} matches", file.file, file.match_count)
        })
        .collect();
    format!("{heading}\n\n{}", lines.join("\n"))
}
