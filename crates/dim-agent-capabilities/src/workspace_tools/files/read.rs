//! `read` (TS `read.ts`), and its image results.

use super::super::file_reads::{FileReads, TextEncoding, TextFileBinding};
use super::codec::{self, CodecError};
use super::{
    Cancelled, FileTools, NotRegular, blocking, cancelled, failed, field, hex, invalid_input,
    js_len, not_regular, object, open_regular, resolve_path, succeeded,
};
use crate::host::HostError;
use crate::images::{self, ImageKind, ImageRefusal, ImageSize, ScaledImage, Sniffed};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_types::{
    ContentBlock, ImageSource, ImageSourceType, JsonObject, ToolContent, ToolResult,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{ErrorKind, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

/// Maximum lines per read request (policy limit).
const READ_LIMIT_MAX: u64 = 10_000;
/// Maximum UTF-16 units in a single read result content.
const READ_RESULT_MAX_CHARS: usize = 256 * 1024;
/// Maximum UTF-16 units kept inline before the result is written to a file.
const READ_INLINE_MAX_CHARS: usize = 30_000;
/// UTF-16 units of preview returned after the result is written to a file.
const READ_OFFLOAD_PREVIEW_CHARS: usize = 2_000;
/// Bytes inspected for binary detection.
const DETECT_BYTES: usize = 4096;
const CHUNK_BYTES: usize = 64 * 1024;

/// Known binary extensions, refused once the head shows the content is not an
/// image.
const BINARY_EXTENSIONS: &[&str] = &[
    "zip", "gz", "tar", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "bin", "wasm",
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "class", "pyc", "pyo", "o", "obj", "a",
    "lib", "ttf", "otf", "woff", "woff2", "eot", "ico", "sqlite", "db",
];

pub(super) fn definition() -> Value {
    json!({
        "name": "read",
        "description": "Read a file from the filesystem. Text comes back with line numbers; use offset and limit for large files. A PNG, JPEG, WebP or GIF image comes back as an image block with one line giving its path and pixel size.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to read." },
                "offset": { "type": "number", "description": "Starting line number (1-indexed). Default: 1." },
                "limit": { "type": "number", "description": "Maximum number of lines to return. Default: 2000." },
                "encoding": { "type": "string" },
            },
            "required": ["path"],
        },
        "source": "builtin",
        "risk": "read_filesystem",
    })
}

struct Input {
    path: String,
    offset: u64,
    limit: u64,
    encoding: Option<String>,
    /// Whether the caller gave `offset` or `limit`, which images ignore.
    window_given: bool,
}

fn parse(input: &JsonObject) -> Result<Input, ToolResult> {
    let Some(Value::String(path)) = field(input, "path").filter(|path| path.as_str() != Some(""))
    else {
        return Err(invalid_input("read requires a non-empty string path."));
    };
    let offset = match field(input, "offset") {
        None => 1,
        Some(value) => positive_integer(value)
            .ok_or_else(|| invalid_input("read offset must be a positive integer (>= 1)."))?,
    };
    let limit = match field(input, "limit") {
        None => 2000,
        Some(value) => positive_integer(value)
            .ok_or_else(|| invalid_input("read limit must be a positive integer (>= 1)."))?,
    };
    if limit > READ_LIMIT_MAX {
        return Err(invalid_input(format!(
            "read limit must not exceed {READ_LIMIT_MAX}."
        )));
    }
    let encoding = match field(input, "encoding") {
        None => None,
        Some(Value::String(name)) if !name.trim().is_empty() => Some(name.clone()),
        Some(_) => {
            return Err(invalid_input(
                "read encoding must be a non-empty string when provided.",
            ));
        }
    };
    Ok(Input {
        path: path.clone(),
        offset,
        limit,
        encoding,
        window_given: field(input, "offset").is_some() || field(input, "limit").is_some(),
    })
}

/// TS `Number(value)` followed by an integer >= 1 check; numeric strings count.
fn positive_integer(value: &Value) -> Option<u64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (number.fract() == 0.0 && number >= 1.0 && number <= u64::MAX as f64).then_some(number as u64)
}

pub(super) async fn run(
    tools: &FileTools,
    call_id: &str,
    input: &JsonObject,
    reads: &FileReads,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let input = match parse(input) {
        Ok(input) => input,
        Err(result) => return Ok(result),
    };
    let established = reads.established();
    let (cwd, home) = (tools.cwd.clone(), tools.home.clone());
    let outcome = blocking(move || {
        let path = match resolve_path(&cwd, &home, &input.path) {
            Ok(path) => path,
            Err(message) => return Ok((invalid_input(message), None)),
        };
        read_text(&input, path, &cancel)
    })
    .await?
    .map_err(|Cancelled| cancelled())?;
    let (result, binding) = outcome;
    let result = offload(tools, call_id, result).await?;
    if let Some((path, binding)) = binding {
        reads.set(path, binding, established);
    }
    Ok(result)
}

/// TS `offloadReadResultIfNeeded`: a successful text result longer than
/// `READ_INLINE_MAX_CHARS` is written to the Session output directory and
/// replaced by its path and a preview. Rust has no blob store; the file sits
/// next to the exec output files and is named in `outputPath`.
async fn offload(
    tools: &FileTools,
    call_id: &str,
    result: ToolResult,
) -> Result<ToolResult, HostError> {
    let ToolContent::Text(content) = &result.content else {
        return Ok(result);
    };
    if result.is_error == Some(true) || js_len(content) <= READ_INLINE_MAX_CHARS {
        return Ok(result);
    }
    let dir = tools.output_dir.clone();
    let path = dir.join(format!(
        "read-{}-{}.txt",
        file_safe(call_id),
        uuid::Uuid::new_v4()
    ));
    let (text, target) = (content.clone(), path.clone());
    blocking(move || {
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        directory.create(&dir)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&target)?, text.as_bytes())
    })
    .await?
    .map_err(|error| HostError::failed(format!("Failed to write read output: {error}")))?;
    let output_path = path.to_string_lossy().into_owned();
    let preview = js_prefix(content, READ_OFFLOAD_PREVIEW_CHARS);
    let mut structured = result.structured_content.clone().unwrap_or_default();
    structured.insert("offloaded".into(), Value::Bool(true));
    structured.insert("outputPath".into(), Value::String(output_path.clone()));
    Ok(ToolResult {
        content: ToolContent::Text(format!(
            "Read output too large ({} bytes). Full output: {output_path}\n\nPreview (first {READ_OFFLOAD_PREVIEW_CHARS} chars):\n{preview}",
            content.len()
        )),
        structured_content: Some(structured),
        ..result
    })
}

/// The longest prefix of at most `units` UTF-16 units (JS `slice(0, n)`
/// without splitting a surrogate pair).
fn js_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        used += ch.len_utf16();
        if used > units {
            return &text[..index];
        }
    }
    text
}

fn file_safe(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

type Outcome = (ToolResult, Option<(PathBuf, TextFileBinding)>);

fn read_text(
    input: &Input,
    path: PathBuf,
    cancel: &CancellationToken,
) -> Result<Outcome, Cancelled> {
    let done = |result| Ok((result, None));
    let display = &input.path;
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return done(stat_failure(&error, &path, display)),
    };
    Cancelled::check(cancel)?;
    if metadata.is_dir() {
        return done(failed(
            format!("Cannot read directory: {display}"),
            json!({ "code": "is_directory" }),
        ));
    }
    let mut file = match open_regular(&path) {
        Ok(Ok(file)) => file,
        Ok(Err(NotRegular)) => return done(not_regular(display)),
        Err(error) => return done(open_failure(&error, display)),
    };
    let mut head = Vec::with_capacity(DETECT_BYTES);
    if let Err(error) = (&mut file).take(DETECT_BYTES as u64).read_to_end(&mut head) {
        return done(open_failure(&error, display));
    }
    Cancelled::check(cancel)?;
    if let Some(sniffed) = images::sniff(&head) {
        return match sniffed {
            Sniffed::Image(_) => Ok((
                read_image(&mut file, display, metadata.len(), input, cancel)?,
                None,
            )),
            Sniffed::Unsupported(name) => done(unsupported_image(display, name)),
        };
    }
    // The extension is a hint against content that is not an image: an image
    // is recognized from its bytes whatever the file is called.
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());
    if extension.is_some_and(|extension| BINARY_EXTENSIONS.contains(&extension.as_str())) {
        return done(binary(display));
    }
    let resolved = match codec::resolve_encoding(&head, input.encoding.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => return done(error.into_result()),
    };
    if !matches!(
        resolved.encoding,
        TextEncoding::Utf16Le | TextEncoding::Utf16Be
    ) && head.contains(&0)
    {
        return done(binary(display));
    }
    let size = match file.metadata() {
        Ok(opened) => opened.len(),
        Err(error) => return done(open_failure(&error, display)),
    };
    let content_bytes = size.saturating_sub(codec::bom_bytes(resolved.bom).len() as u64);
    if content_bytes > 0 && input.offset > content_bytes {
        return done(offset_out_of_range(input.offset));
    }
    let text = match stream_text(&mut file, &path, &resolved, input, cancel)? {
        Ok(text) => text,
        Err(result) => return done(result),
    };
    Cancelled::check(cancel)?;
    let binding = TextFileBinding {
        raw_revision: text.revision,
        encoding: resolved.encoding,
        encoding_source: resolved.source,
        bom: resolved.bom,
        eol: text.eol,
    };
    let mut structured = json!({
        "path": display,
        "size": metadata.len(),
        "revision": hex(&binding.raw_revision),
        "encoding": codec::encoding_json(&binding.encoding),
        "encodingSource": codec::source_str(binding.encoding_source),
        "bom": codec::bom_str(binding.bom),
        "eol": codec::eol_str(binding.eol),
    });
    if !text.has_content {
        return Ok((succeeded("", structured), Some((path, binding))));
    }
    if input.offset > text.total_lines {
        return done(offset_out_of_range(input.offset));
    }
    structured["kind"] = json!("text");
    let content = render_window(&text.window, input, text.total_lines);
    Ok((succeeded(content, structured), Some((path, binding))))
}

struct StreamedText {
    /// Lines `offset..` of the window, without line ends; collection stops
    /// once the read budget is exceeded and each line is cut at the budget.
    window: Vec<String>,
    has_content: bool,
    revision: [u8; 32],
    total_lines: u64,
    eol: super::super::file_reads::Eol,
}

/// Hashes every byte and decodes strictly, keeping only the lines of the
/// requested window within the read budget: lines before `offset` and after
/// the window are counted, not kept.
fn stream_text(
    file: &mut std::fs::File,
    path: &Path,
    resolved: &codec::ResolvedEncoding,
    input: &Input,
    cancel: &CancellationToken,
) -> Result<Result<StreamedText, ToolResult>, Cancelled> {
    let first = input.offset;
    let last_line = input.offset.saturating_add(input.limit - 1);
    let read_error = |error: std::io::Error| {
        failed(
            format!("Failed to read file: {error}"),
            json!({ "code": "read_error" }),
        )
    };
    if let Err(error) = file.seek(SeekFrom::Start(0)) {
        return Ok(Err(read_error(error)));
    }
    let mut decoder = codec::encoding_rs_of(&resolved.encoding).new_decoder_without_bom_handling();
    let mut hash = Sha256::new();
    let mut window: Vec<String> = Vec::new();
    let (mut line_no, mut current, mut current_units, mut kept_units) =
        (1u64, String::new(), 0usize, 0usize);
    let mut decoded = String::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    let mut skip = codec::bom_bytes(resolved.bom).len();
    let (mut lines, mut crlf, mut lone_lf) = (0u64, 0u64, 0u64);
    let (mut has_content, mut last_is_newline, mut previous_cr) = (false, false, false);
    let mut collecting = true;
    let finish_line =
        |line: &mut String, units: &mut usize, kept: &mut usize, window: &mut Vec<String>| {
            let text = std::mem::take(line);
            let text = text.strip_suffix('\r').map(str::to_owned).unwrap_or(text);
            *kept += *units + 1;
            *units = 0;
            window.push(text);
            *kept > READ_RESULT_MAX_CHARS
        };
    loop {
        Cancelled::check(cancel)?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Ok(Err(read_error(error))),
        };
        let last = read == 0;
        let chunk = &buffer[..read];
        hash.update(chunk);
        let body = &chunk[skip.min(chunk.len())..];
        skip = skip.saturating_sub(chunk.len());
        decoded.clear();
        if codec::decode_chunk(&mut decoder, body, last, &mut decoded).is_err() {
            return Ok(Err(invalid_bytes(&resolved.encoding).into_result()));
        }
        if decoded.contains('\0') {
            return Ok(Err(binary(&path.display().to_string())));
        }
        if !decoded.is_empty() {
            has_content = true;
            last_is_newline = decoded.ends_with('\n');
        }
        for ch in decoded.chars() {
            let in_window = collecting && line_no >= first && line_no <= last_line;
            if ch == '\n' {
                if previous_cr {
                    crlf += 1;
                } else {
                    lone_lf += 1;
                }
                lines += 1;
                if in_window
                    && finish_line(
                        &mut current,
                        &mut current_units,
                        &mut kept_units,
                        &mut window,
                    )
                {
                    collecting = false;
                }
                line_no += 1;
                if line_no > last_line {
                    collecting = false;
                }
            } else if in_window && current_units <= READ_RESULT_MAX_CHARS {
                current.push(ch);
                current_units += ch.len_utf16();
            }
            previous_cr = ch == '\r';
        }
        if last {
            break;
        }
    }
    if collecting && has_content && !last_is_newline && line_no >= first && line_no <= last_line {
        finish_line(
            &mut current,
            &mut current_units,
            &mut kept_units,
            &mut window,
        );
    }
    let total_lines = match (has_content, last_is_newline) {
        (false, _) => 0,
        (true, true) => lines,
        (true, false) => lines + 1,
    };
    let eol = match (crlf, lone_lf) {
        (0, 0) => super::super::file_reads::Eol::None,
        (_, 0) => super::super::file_reads::Eol::Crlf,
        (0, _) => super::super::file_reads::Eol::Lf,
        _ => super::super::file_reads::Eol::Mixed,
    };
    Ok(Ok(StreamedText {
        window,
        has_content,
        revision: hash.finalize().into(),
        total_lines,
        eol,
    }))
}

fn offset_out_of_range(offset: u64) -> ToolResult {
    failed(
        format!("Read offset {offset} exceeds file length."),
        json!({ "code": "offset_out_of_range" }),
    )
}

fn invalid_bytes(encoding: &TextEncoding) -> CodecError {
    match encoding {
        TextEncoding::Legacy { name, .. } => CodecError::new(
            "text_encoding_required",
            format!("File bytes are not valid {name} text."),
        ),
        _ => CodecError::new(
            "text_encoding_required",
            "File bytes are not valid for the selected text encoding. Specify the correct encoding and read again.",
        ),
    }
}

/// Numbered lines of the requested window, cut to the safe budget.
fn render_window(window: &[String], input: &Input, total_lines: u64) -> String {
    let start = input.offset;
    let end = start.saturating_add(input.limit - 1).min(total_lines);
    let records: Vec<String> = window
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}\t{line}", start + index as u64))
        .collect();
    let mut shown = records.len();
    let mut budget = 0;
    for (index, record) in records.iter().enumerate() {
        budget += js_len(record) + usize::from(index > 0);
        if budget > READ_RESULT_MAX_CHARS {
            if index == 0 {
                return format!(
                    "Line {start} exceeds the safe read budget and is truncated. The rest of this line is not included; a line-based offset cannot retrieve it. Use exec to inspect it in smaller chunks.\n\n{}",
                    js_prefix(record, READ_RESULT_MAX_CHARS)
                );
            }
            shown = index;
            break;
        }
    }
    let truncated = (shown as u64) < end - start + 1;
    let mut content = records[..shown].join("\n");
    if end < total_lines || truncated {
        let actual_end = if truncated {
            start + shown as u64 - 1
        } else {
            end
        };
        content.push_str(&format!(
            "\n\nFile is larger than the safe read budget for one response. Showing lines {start}-{actual_end} of {total_lines}. Continue with read({{ path: \"{}\", offset: {} }}).",
            input.path,
            actual_end + 1
        ));
    }
    content
}

/// An image file's result: the image block and one line naming the path, the
/// pixel size and, when the image was re-encoded, the size sent.
fn read_image(
    file: &mut std::fs::File,
    display: &str,
    size: u64,
    input: &Input,
    cancel: &CancellationToken,
) -> Result<ToolResult, Cancelled> {
    if let Err(error) = file.seek(SeekFrom::Start(0)) {
        return Ok(open_failure(&error, display));
    }
    Cancelled::check(cancel)?;
    // The source is the file itself: `scale_image` reads the header, runs the
    // pixel guard and only then decodes, so the file is never all in memory.
    let source = std::io::BufReader::new(&mut *file);
    Ok(match images::scale_image(source) {
        Ok(image) => image_result(display, size, input, image),
        Err(ImageRefusal::NotAnImage) => binary(display),
        Err(ImageRefusal::UnsupportedFormat(name)) => unsupported_image(display, name),
        Err(ImageRefusal::TooManyPixels(dimensions)) => image_too_large(display, dimensions),
        Err(ImageRefusal::Invalid { kind, reason }) => invalid_image(display, kind, &reason),
    })
}

fn image_result(display: &str, size: u64, input: &Input, image: ScaledImage) -> ToolResult {
    let mut line = format!(
        "Read image: {display} ({}x{} px).",
        image.original.width, image.original.height
    );
    if image.animated {
        line.push_str(&format!(
            " Animated {}: only the first frame was sent.",
            image.kind.name()
        ));
    }
    if image.scaled {
        line.push_str(&format!(
            " Sent {}x{} px as {} ({} bytes).",
            image.sent.width,
            image.sent.height,
            image.media_type,
            image.bytes.len()
        ));
    }
    if input.window_given {
        line.push_str(" Offset and limit do not apply to images.");
    }
    let mut structured = json!({
        "kind": "image",
        "path": display,
        "size": size,
        "format": image.kind.name(),
        "mediaType": image.media_type,
        "width": image.original.width,
        "height": image.original.height,
    });
    if image.scaled {
        structured["sentWidth"] = json!(image.sent.width);
        structured["sentHeight"] = json!(image.sent.height);
        structured["sentBytes"] = json!(image.bytes.len());
    }
    if image.animated {
        structured["animated"] = json!(true);
    }
    let source = ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: image.media_type.to_string(),
        data: Some(STANDARD.encode(&image.bytes)),
        url: None,
    };
    ToolResult {
        content: ToolContent::Blocks(vec![
            ContentBlock::Text { text: line },
            ContentBlock::Image { source },
        ]),
        structured_content: Some(object(structured)),
        is_error: None,
        blob_refs: None,
        background_task: None,
        context_messages: None,
        effects: None,
    }
}

fn unsupported_image(display: &str, name: &str) -> ToolResult {
    failed(
        format!("{name} images are not supported by read: {display}."),
        json!({ "code": "binary_unsupported" }),
    )
}

fn image_too_large(display: &str, size: ImageSize) -> ToolResult {
    failed(
        format!(
            "Image is too large for read: {display} is {}x{} pixels. Make a smaller copy and read that.",
            size.width, size.height
        ),
        json!({ "code": "binary_unsupported" }),
    )
}

fn invalid_image(display: &str, kind: ImageKind, reason: &str) -> ToolResult {
    failed(
        format!(
            "read could not decode the {} image {display}: {reason}",
            kind.name()
        ),
        json!({ "code": "binary_unsupported" }),
    )
}

fn binary(display: &str) -> ToolResult {
    failed(
        format!("Binary file is not supported by read: {display}."),
        json!({ "code": "binary_unsupported" }),
    )
}

fn stat_failure(error: &std::io::Error, path: &Path, display: &str) -> ToolResult {
    match error.kind() {
        ErrorKind::NotFound => {
            let suggestions = similar_files(path);
            let message = if suggestions.is_empty() {
                format!("File not found: {display}")
            } else {
                format!(
                    "File not found: {display}\nDid you mean: {}",
                    suggestions.join(", ")
                )
            };
            failed(message, json!({ "code": "file_not_found" }))
        }
        ErrorKind::PermissionDenied => failed(
            format!("Permission denied: {display}"),
            json!({ "code": "permission_denied" }),
        ),
        _ if is_symlink_loop(error) => failed(
            format!("Symlink loop detected: {display}"),
            json!({ "code": "symlink_loop" }),
        ),
        _ => failed(
            format!("Failed to read file: {error}"),
            json!({ "code": "read_error" }),
        ),
    }
}

fn open_failure(error: &std::io::Error, display: &str) -> ToolResult {
    if error.kind() == ErrorKind::PermissionDenied {
        return failed(
            format!("Permission denied: {display}"),
            json!({ "code": "permission_denied" }),
        );
    }
    failed(
        format!("Failed to read file: {error}"),
        json!({ "code": "read_error" }),
    )
}

fn is_symlink_loop(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(libc::ELOOP);
    #[cfg(not(unix))]
    return {
        let _ = error;
        false
    };
}

/// Case-insensitive siblings of a missing file, at most three.
fn similar_files(path: &Path) -> Vec<String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let name = name.to_string_lossy();
    let lower = name.to_lowercase();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| entry.to_lowercase() == lower && *entry != name)
        .take(3)
        .collect()
}
