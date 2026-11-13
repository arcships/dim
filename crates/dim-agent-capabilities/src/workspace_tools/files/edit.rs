//! `edit` (TS `edit.ts`): exact replacement with EOL and Unicode fallbacks.

use super::super::file_reads::{Eol, FileReads, TextFileBinding};
use super::codec;
use super::write::write_target;
use super::{
    Access, Cancelled, FileTools, NotRegular, PathError, Target, TargetError, blocking, cancelled,
    failed, field, hex, invalid_input, js_len, not_regular, refused, resolve_path, revision, wrote,
};
use crate::host::HostError;
use dim_agent_types::{JsonObject, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::{
    io::{ErrorKind, Read},
    path::Path,
};
use tokio_util::sync::CancellationToken;

/// Max combined UTF-16 length of oldText + newText over every replacement.
const EDIT_INPUT_MAX_CHARS: usize = 10_000;
/// TS limit: min(1 GiB, V8 `MAX_STRING_LENGTH`).
const EDIT_TARGET_MAX_BYTES: u64 = 536_870_888;
/// Read size per cancellation check.
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// Context lines around each change in the display diff.
const EDIT_DIFF_CONTEXT: usize = 3;

pub(super) fn definition() -> Value {
    json!({
        "name": "edit",
        "description": "Edit an existing file by replacing exact text matches. Read the file first, then derive oldText and newText from the latest read result. Use this tool for ordinary incremental text edits instead of shell commands or Python. Pass every change to one file in one call as edits; they apply in order, each against the text the previous one produced, and nothing is written if any of them fails.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to edit." },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The replacements to apply, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Text to find and replace (exact match)." },
                            "newText": { "type": "string", "description": "Replacement text." },
                            "replaceAll": { "type": "boolean", "description": "Replace every match in this replacement instead of requiring exactly one (default: false)." },
                        },
                        "required": ["oldText", "newText"],
                    },
                },
            },
            "required": ["path", "edits"],
        },
        "source": "builtin",
        "risk": "write_filesystem",
        "fileChanges": {
            "preExecutePaths": { "selector": "path" },
            "postExecutePaths": "path",
        },
    })
}

struct Replacement {
    old_text: String,
    new_text: String,
    replace_all: bool,
}

struct Input {
    path: String,
    edits: Vec<Replacement>,
}

/// Reads the replacement list. A recorded call that carries the top-level
/// `oldText` and `newText` instead is read as one replacement; a call that
/// carries both shapes, or an empty list, is refused.
fn parse(input: &JsonObject) -> Result<Input, ToolResult> {
    let Some(Value::String(path)) = field(input, "path").filter(|path| path.as_str() != Some(""))
    else {
        return Err(invalid_input("edit requires a non-empty string path."));
    };
    let edits = match field(input, "edits") {
        Some(value) => {
            if field(input, "oldText").is_some() || field(input, "newText").is_some() {
                return Err(invalid_input(
                    "edit takes a list of replacements: pass edits: [{oldText, newText}], without oldText and newText beside it.",
                ));
            }
            let Some(items) = value.as_array().filter(|items| !items.is_empty()) else {
                return Err(invalid_input(
                    "edit requires a non-empty edits array: pass edits: [{oldText, newText}].",
                ));
            };
            items
                .iter()
                .enumerate()
                .map(|(index, item)| match item.as_object() {
                    Some(item) => replacement(item).map_err(|result| for_index(result, index)),
                    None => Err(for_index(
                        invalid_input("a replacement must be an object with oldText and newText."),
                        index,
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        None => vec![replacement(input).map_err(|result| for_index(result, 0))?],
    };
    Ok(Input {
        path: path.clone(),
        edits,
    })
}

/// One replacement read from `fields`; a failure names its entry through
/// [`for_index`] at the caller, so a parse failure and an apply failure carry
/// the same field.
fn replacement(fields: &JsonObject) -> Result<Replacement, ToolResult> {
    let Some(Value::String(old_text)) = field(fields, "oldText") else {
        return Err(invalid_input("edit requires a string oldText."));
    };
    if old_text.is_empty() {
        return Err(invalid_input("oldText must not be empty."));
    }
    let Some(Value::String(new_text)) = field(fields, "newText") else {
        return Err(invalid_input("edit requires a string newText."));
    };
    Ok(Replacement {
        old_text: old_text.clone(),
        new_text: new_text.clone(),
        replace_all: field(fields, "replaceAll") == Some(&Value::Bool(true)),
    })
}

pub(super) async fn run(
    tools: &FileTools,
    input: &JsonObject,
    reads: &FileReads,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let input = match parse(input) {
        Ok(input) => input,
        Err(result) => return Ok(result),
    };
    let established = reads.established();
    let chars = input
        .edits
        .iter()
        .map(|replacement| js_len(&replacement.old_text) + js_len(&replacement.new_text))
        .sum::<usize>();
    if chars > EDIT_INPUT_MAX_CHARS {
        return Ok(failed(
            format!(
                "edit input too large: the replacements' oldText and newText total {chars} chars, limit is {EDIT_INPUT_MAX_CHARS} chars."
            ),
            json!({ "code": "input_too_large", "chars": chars, "limit": EDIT_INPUT_MAX_CHARS }),
        ));
    }
    if let Some(index) = input
        .edits
        .iter()
        .position(|replacement| replacement.old_text == replacement.new_text)
    {
        return Ok(for_index(
            failed(
                "oldText and newText are identical; no change needed.",
                json!({ "code": "no_change" }),
            ),
            index,
        ));
    }
    let (cwd, home, requested, boundary) = (
        tools.cwd.clone(),
        tools.home.clone(),
        input.path.clone(),
        tools.boundary.clone(),
    );
    let path = match blocking(move || {
        resolve_path(&cwd, &home, boundary.as_ref(), Access::Write, &requested)
    })
    .await?
    {
        Ok(path) => path,
        Err(PathError::Refused(path)) => return Ok(refused(&path)),
        Err(PathError::Message(message)) => return Ok(invalid_input(message)),
    };
    #[cfg(test)]
    if let Some(hook) = &tools.before_lock {
        hook(&path);
    }
    let _lock = tools.locks.lock(&path, &cancel).await?;
    let binding = reads.get(&path);
    let (target, bounded) = (path.clone(), tools.boundary.is_some());
    #[cfg(test)]
    let hook = tools.before_commit.clone();
    let (_lock, written) = blocking(move || {
        let written = edit_locked(
            &target,
            bounded,
            &input,
            binding,
            &cancel,
            #[cfg(test)]
            hook,
        );
        (_lock, written)
    })
    .await?;
    let (result, binding) = written.map_err(|Cancelled| cancelled())?;
    if let Some(binding) = binding {
        reads.set(path, binding, established);
    }
    Ok(result)
}

fn edit_locked(
    path: &Path,
    bounded: bool,
    input: &Input,
    binding: Option<TextFileBinding>,
    cancel: &CancellationToken,
    #[cfg(test)] hook: Option<super::CommitHook>,
) -> Result<(ToolResult, Option<TextFileBinding>), Cancelled> {
    let done = |result| Ok((result, None));
    let display = &input.path;
    let stat_failure = |error: std::io::Error| match error.kind() {
        ErrorKind::NotFound => failed(
            format!("File not found: {display}"),
            json!({ "code": "file_not_found" }),
        ),
        ErrorKind::PermissionDenied => failed(
            format!("Permission denied: {display}"),
            json!({ "code": "permission_denied" }),
        ),
        _ => failed(
            format!("Failed to stat file: {error}"),
            json!({ "code": "stat_error" }),
        ),
    };
    Cancelled::check(cancel)?;
    let target = match Target::for_mutation(path, bounded, false) {
        Ok(target) => target,
        Err(TargetError::Refused) => return done(refused(path)),
        Err(TargetError::Io(error)) => return done(stat_failure(error)),
    };
    let size = match target.stat() {
        Ok(Some(stat)) if stat.directory => {
            return done(failed(
                format!("Cannot edit directory: {display}"),
                json!({ "code": "is_directory" }),
            ));
        }
        Ok(Some(stat)) => stat.len,
        Ok(None) => {
            return done(failed(
                format!("File not found: {display}"),
                json!({ "code": "file_not_found" }),
            ));
        }
        Err(TargetError::Refused) => return done(refused(path)),
        Err(TargetError::Io(error)) => return done(stat_failure(error)),
    };
    Cancelled::check(cancel)?;
    if size > EDIT_TARGET_MAX_BYTES {
        return done(too_large(size));
    }
    let read_failure = |error: std::io::Error| {
        if error.kind() == ErrorKind::PermissionDenied {
            failed(
                format!("Permission denied: {display}"),
                json!({ "code": "permission_denied" }),
            )
        } else {
            failed(
                format!("Failed to read file: {error}"),
                json!({ "code": "read_error" }),
            )
        }
    };
    let file = match target.open_regular() {
        Ok(Ok(file)) => file,
        Ok(Err(NotRegular)) => return done(not_regular(display)),
        Err(TargetError::Refused) => return done(refused(path)),
        Err(TargetError::Io(error)) => return done(read_failure(error)),
    };
    let Some(binding) = binding else {
        return done(failed(
            "Read the file before editing it so its encoding and line endings can be preserved.",
            json!({ "code": "text_binding_required" }),
        ));
    };
    let mut bytes = Vec::with_capacity(size as usize);
    let mut limited = file.take(EDIT_TARGET_MAX_BYTES + 1);
    let mut chunk = vec![0; READ_CHUNK_BYTES];
    loop {
        Cancelled::check(cancel)?;
        match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return done(read_failure(error)),
        }
    }
    if bytes.len() as u64 > EDIT_TARGET_MAX_BYTES {
        return done(too_large(bytes.len() as u64));
    }
    if revision(&bytes) != binding.raw_revision {
        return done(failed(
            "File was modified after it was last read. Use the read tool again before editing.",
            json!({ "code": "file_modified_since_read" }),
        ));
    }
    let before = match codec::decode_text(&bytes, &binding) {
        Ok(text) => text,
        Err(error) => return done(error.into_result()),
    };
    let eol = match binding.eol {
        Eol::None => Eol::Lf,
        eol => eol,
    };
    let (after, count) = match apply(&before, &input.edits, eol, cancel)? {
        Ok(replaced) => replaced,
        Err((index, result)) => return done(for_index(result, index)),
    };
    Cancelled::check(cancel)?;
    let diff = unified_diff(&to_lf(&before), &to_lf(&after));
    let written = match codec::encode_text(&after, &binding) {
        Ok(bytes) => bytes,
        Err(error) => return done(error.into_result()),
    };
    Cancelled::check(cancel)?;
    if let Err(result) = write_target(
        &target,
        &written,
        display,
        Some(limited.get_ref()),
        &binding.raw_revision,
        cancel,
        #[cfg(test)]
        hook,
    )? {
        return done(result);
    }
    let raw_revision = revision(&written);
    let result = wrote(
        format!("Edited {display}"),
        json!({
            "path": display,
            "replacementCount": count,
            "revision": hex(&raw_revision),
            "diff": diff,
        }),
        display,
    );
    let binding = TextFileBinding {
        raw_revision,
        eol: codec::detect_eol(&after),
        ..binding
    };
    Ok((result, Some(binding)))
}

/// `size` is a lower bound when the file grew while being read.
fn too_large(size: u64) -> ToolResult {
    failed(
        format!(
            "edit target file too large: file is {size} bytes, limit is {EDIT_TARGET_MAX_BYTES} bytes."
        ),
        json!({ "code": "edit_target_file_too_large", "bytes": size, "limit": EDIT_TARGET_MAX_BYTES }),
    )
}

fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn to_crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn multiple_matches(matches: usize) -> ToolResult {
    failed(
        "oldText matched more than once; set replaceAll to true to replace all matches.",
        json!({ "code": "multiple_matches", "matches": matches }),
    )
}

/// A replacement's failure names where it sits: `index` in the structured
/// result, and `edits[<index>]:` before the message the model reads.
fn for_index(mut result: ToolResult, index: usize) -> ToolResult {
    if let ToolContent::Text(text) = &mut result.content {
        *text = format!("edits[{index}]: {text}");
    }
    result
        .structured_content
        .as_mut()
        .expect("a replacement failure carries structured content")
        .insert("index".into(), json!(index));
    result
}

/// The first failing entry: its 0-based index and the refusal it produced.
type EntryFailure = (usize, ToolResult);

/// Applies every replacement in order, each against the text the previous one
/// produced, and returns the text and the total number of replacements. The
/// first failure names its index and leaves the caller nothing to write.
///
/// Cancellation is observed between entries, before the next one runs, so a
/// cancelled call stops at an entry boundary and writes nothing.
fn apply(
    file: &str,
    edits: &[Replacement],
    eol: Eol,
    cancel: &CancellationToken,
) -> Result<Result<(String, usize), EntryFailure>, Cancelled> {
    let mut text = file.to_owned();
    let mut count = 0;
    for (index, replacement) in edits.iter().enumerate() {
        Cancelled::check(cancel)?;
        match replace(&text, replacement, eol) {
            Ok((next, replaced)) => {
                text = next;
                count += replaced;
            }
            Err(result) => return Ok(Err((index, result))),
        }
    }
    Ok(Ok((text, count)))
}

/// Refuses an entry whose result would pass [`EDIT_TARGET_MAX_BYTES`] before
/// the result is built. [`EDIT_INPUT_MAX_CHARS`] bounds the submitted texts,
/// not what they produce: a `replaceAll` of one byte by many multiplies the
/// whole text at every entry, so a small input can ask for a terabyte.
///
/// Counted on the decoded text in LF space, the same space the match count
/// comes from.
fn bound_growth(
    file: &str,
    old_text: &str,
    new_text: &str,
    replaced: usize,
) -> Result<(), ToolResult> {
    let size = (file.len() as u64)
        .saturating_sub((old_text.len() as u64).saturating_mul(replaced as u64))
        .saturating_add((new_text.len() as u64).saturating_mul(replaced as u64));
    if size > EDIT_TARGET_MAX_BYTES {
        return Err(failed(
            format!(
                "the replacements would grow the text to {size} bytes, limit is {EDIT_TARGET_MAX_BYTES} bytes."
            ),
            json!({ "code": "edit_target_file_too_large", "bytes": size, "limit": EDIT_TARGET_MAX_BYTES }),
        ));
    }
    Ok(())
}

/// Exact match in LF space first, then the Unicode/trailing-whitespace
/// fallback. `eol` is never `None` here.
///
/// `newText` is always inserted literally. TS single replacement goes through
/// JS `String.replace`, which expands `$&`, `$$`, `` $` `` and `$'` in the
/// replacement, while its `replaceAll` and fuzzy paths do not; that is a JS
/// API accident, not a tool contract, and it corrupts code that contains
/// `$$` or `$&` (shell, PHP, regex docs), so Rust keeps every path literal.
fn replace(file: &str, replacement: &Replacement, eol: Eol) -> Result<(String, usize), ToolResult> {
    let lf_file = to_lf(file);
    let lf_old = to_lf(&replacement.old_text);
    let lf_new = to_lf(&replacement.new_text);
    let exact = lf_file.matches(lf_old.as_str()).count();
    if exact > 0 {
        if !replacement.replace_all && exact > 1 {
            return Err(multiple_matches(exact));
        }
        let replaced = if replacement.replace_all { exact } else { 1 };
        bound_growth(&lf_file, &lf_old, &lf_new, replaced)?;
        let limit = if replacement.replace_all {
            usize::MAX
        } else {
            1
        };
        let content = match eol {
            Eol::Mixed => {
                replace_lf_matches_in_raw(file, &lf_old, &lf_new, replacement.replace_all)
            }
            Eol::Crlf => to_crlf(&lf_file.replacen(lf_old.as_str(), &lf_new, limit)),
            _ => lf_file.replacen(lf_old.as_str(), &lf_new, limit),
        };
        return Ok((content, replaced));
    }

    let normalized_file = normalize(&lf_file);
    let normalized_old = normalize(&lf_old);
    let matches = if normalized_old.is_empty() {
        0
    } else {
        normalized_file.matches(normalized_old.as_str()).count()
    };
    if matches > 0 {
        if !replacement.replace_all && matches > 1 {
            return Err(multiple_matches(matches));
        }
        let replaced = if replacement.replace_all { matches } else { 1 };
        bound_growth(&lf_file, &normalized_old, &lf_new, replaced)?;
        let content = replace_by_normalized_positions(
            file,
            &normalized_file,
            &normalized_old,
            &lf_new,
            replacement.replace_all,
            eol,
        );
        return Ok((content, replaced));
    }
    Err(failed(
        "oldText was not found in the target file.",
        json!({ "code": "oldtext_not_found" }),
    ))
}

/// Smart quotes, Unicode dashes and special spaces map to ASCII one char to
/// one char, so positions within a line carry over by char index.
fn normalize_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' => ' ',
        c => c,
    }
}

/// Unicode normalization plus per-line trailing space/tab trim.
fn normalize(lf_text: &str) -> String {
    lf_text
        .split('\n')
        .map(|line| {
            line.chars()
                .map(normalize_char)
                .collect::<String>()
                .trim_end_matches([' ', '\t'])
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn match_positions(haystack: &str, needle: &str, all: bool) -> Vec<usize> {
    let positions = haystack.match_indices(needle).map(|(index, _)| index);
    if all {
        positions.collect()
    } else {
        positions.take(1).collect()
    }
}

fn replace_by_normalized_positions(
    file: &str,
    normalized_file: &str,
    normalized_old: &str,
    new_text: &str,
    replace_all: bool,
    eol: Eol,
) -> String {
    let lf_file = to_lf(file);
    let lf_lines: Vec<&str> = lf_file.split('\n').collect();
    let norm_lines: Vec<&str> = normalized_file.split('\n').collect();
    let starts = |lines: &[&str]| {
        let mut offset = 0;
        lines
            .iter()
            .map(|line| {
                let start = offset;
                offset += line.len() + 1;
                start
            })
            .collect::<Vec<_>>()
    };
    let (lf_starts, norm_starts) = (starts(&lf_lines), starts(&norm_lines));
    let to_lf_offset = |position: usize| {
        let line = norm_starts.partition_point(|start| *start <= position) - 1;
        let within = position - norm_starts[line];
        let (norm_line, lf_line) = (norm_lines[line], lf_lines[line]);
        if within >= norm_line.len() {
            return lf_starts[line] + lf_line.len() + (within - norm_line.len());
        }
        let chars = norm_line[..within].chars().count();
        lf_starts[line]
            + lf_line
                .char_indices()
                .nth(chars)
                .map_or(lf_line.len(), |(index, _)| index)
    };
    let view = matches!(eol, Eol::Mixed).then(|| LfView::new(file));
    let mut result = if view.is_some() {
        file.to_owned()
    } else {
        lf_file.clone()
    };
    for position in match_positions(normalized_file, normalized_old, replace_all)
        .into_iter()
        .rev()
    {
        let lf_start = to_lf_offset(position);
        let lf_end = to_lf_offset(position + normalized_old.len());
        match &view {
            Some(view) => {
                let (raw_start, raw_end) = (view.boundaries[lf_start], view.boundaries[lf_end]);
                let local = apply_local_eol(new_text, file, raw_start, raw_end);
                result.replace_range(raw_start..raw_end, &local);
            }
            None => result.replace_range(lf_start..lf_end, new_text),
        }
    }
    if matches!(eol, Eol::Crlf) {
        to_crlf(&result)
    } else {
        result
    }
}

/// Mixed-EOL files are edited in the raw text so untouched line endings keep
/// their bytes.
fn replace_lf_matches_in_raw(raw: &str, lf_old: &str, lf_new: &str, replace_all: bool) -> String {
    let view = LfView::new(raw);
    let mut result = raw.to_owned();
    for position in match_positions(&view.text, lf_old, replace_all)
        .into_iter()
        .rev()
    {
        let (raw_start, raw_end) = (
            view.boundaries[position],
            view.boundaries[position + lf_old.len()],
        );
        let replacement = apply_local_eol(lf_new, raw, raw_start, raw_end);
        result.replace_range(raw_start..raw_end, &replacement);
    }
    result
}

/// LF view of raw text with each LF byte offset mapped to its raw offset.
struct LfView {
    text: String,
    boundaries: Vec<usize>,
}
impl LfView {
    fn new(raw: &str) -> Self {
        let bytes = raw.as_bytes();
        let mut text = Vec::with_capacity(bytes.len());
        let mut boundaries = vec![0];
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                text.push(b'\n');
                index += 2;
            } else {
                text.push(bytes[index]);
                index += 1;
            }
            boundaries.push(index);
        }
        Self {
            text: String::from_utf8(text).expect("CRLF folding keeps UTF-8 valid"),
            boundaries,
        }
    }
}

/// New line breaks follow the matched region's style, else the nearest one.
fn apply_local_eol(replacement: &str, raw: &str, start: usize, end: usize) -> String {
    match detect_local_eol(&raw[start..end]).or_else(|| nearest_eol(raw, start, end)) {
        Some(Eol::Crlf) => to_crlf(&to_lf(replacement)),
        Some(_) => to_lf(replacement),
        None => replacement.to_owned(),
    }
}

fn eol_at(bytes: &[u8], newline: usize) -> Eol {
    if newline > 0 && bytes[newline - 1] == b'\r' {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

fn detect_local_eol(text: &str) -> Option<Eol> {
    let newline = text.find('\n')?;
    Some(eol_at(text.as_bytes(), newline))
}

fn nearest_eol(text: &str, start: usize, end: usize) -> Option<Eol> {
    let bytes = text.as_bytes();
    let before_end = (start.saturating_sub(1) + 1).min(bytes.len());
    let previous = bytes[..before_end].iter().rposition(|byte| *byte == b'\n');
    let next = bytes
        .get(end..)
        .and_then(|rest| rest.iter().position(|byte| *byte == b'\n'))
        .map(|index| index + end);
    let chosen = match (previous, next) {
        (None, None) => return None,
        (None, Some(next)) => next,
        (Some(previous), None) => previous,
        (Some(previous), Some(next)) => {
            if start as isize - previous as isize <= (next - end) as isize {
                previous
            } else {
                next
            }
        }
    };
    Some(eol_at(bytes, chosen))
}

/// Display-only unified diff hunks in LF space.
pub(super) fn unified_diff(before: &str, after: &str) -> String {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut lines = Vec::new();
    for hunk in diff
        .unified_diff()
        .context_radius(EDIT_DIFF_CONTEXT)
        .iter_hunks()
    {
        let ops = hunk.ops();
        let (Some(first), Some(last)) = (ops.first(), ops.last()) else {
            continue;
        };
        let range = |start: usize, end: usize| {
            let len = end - start;
            format!("{},{len}", if len == 0 { start } else { start + 1 })
        };
        lines.push(format!(
            "@@ -{} +{} @@",
            range(first.old_range().start, last.old_range().end),
            range(first.new_range().start, last.new_range().end),
        ));
        for change in hunk.iter_changes() {
            let sign = match change.tag() {
                similar::ChangeTag::Delete => '-',
                similar::ChangeTag::Insert => '+',
                similar::ChangeTag::Equal => ' ',
            };
            let value = change.value();
            lines.push(format!(
                "{sign}{}",
                value.strip_suffix('\n').unwrap_or(value)
            ));
        }
    }
    lines.join("\n")
}
