//! `dimcode ocr`: one `ocr.recognize` host task whose text is printed inline
//! when it fits (Modality A.5, D2).
//!
//! A short text needs no file: the command prints it and writes nothing. The
//! result set is written when the text is longer than the inline rule, when
//! `--output-mode layout` asked for the page images, or when `--out` named a
//! directory — and then only when no target exists, as every Modality
//! command writes.

use super::Modality;
use super::media::{MAX_INLINE_BYTES, file_name, input};
use super::task::{self, Output};
use crate::Result;
use crate::args::Options;
use crate::client::Client;
use crate::sanitize::{safe, safe_line};
use serde_json::{Map, Value, json};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// A text up to this many bytes…
const INLINE_BYTES: u64 = 64 * 1024;
/// …and up to this many UTF-16 units prints inline (A.5).
const INLINE_UNITS: usize = 30_000;
/// How much of a longer text is printed before the file that holds it.
const PREVIEW_CHARS: usize = 2_000;
/// Enough bytes for [`PREVIEW_CHARS`] characters, each at most four bytes.
const PREVIEW_BYTES: usize = 8 * 1024;

/// `dimcode ocr FILE [--output-mode text|layout] [--out DIR]`: submit
/// `ocr.recognize`, then print or write its text (A.5).
pub(crate) async fn run(
    client: &mut Client,
    file: &Path,
    options: &Options,
    output: &mut Output<'_>,
) -> Result<i32> {
    let request = request(file, options.modality.output_mode.as_deref())?;
    let mut modality = Modality::open(client)?;
    let record = modality.submit(request).await?;
    let id = record["task"]["task_id"]
        .as_str()
        .ok_or("modality.task.submit answered without a task")?
        .to_owned();
    if options.modality.no_wait {
        if output.json {
            writeln!(output.out, "{}", serde_json::to_string(&record)?)?;
        } else {
            writeln!(output.out, "{id}")?;
        }
        return Ok(0);
    }
    let Some(record) = task::wait(&mut modality, &id, output).await? else {
        writeln!(output.out, "{id}")?;
        writeln!(output.err, "Stopped waiting; the task continues.")?;
        return Ok(130);
    };
    let state = record["task"]["state"].as_str().unwrap_or_default();
    if state != "succeeded" {
        // Every other terminal state reads the way a submission's does.
        return task::finish(&mut modality, record, None, output).await;
    }
    succeed(&mut modality, &record, options, output).await
}

/// The succeeded task: print its text, write its result set, or both.
async fn succeed(
    modality: &mut Modality<'_>,
    record: &Value,
    options: &Options,
    output: &mut Output<'_>,
) -> Result<i32> {
    let task = &record["task"];
    let id = task["task_id"]
        .as_str()
        .ok_or("a task record has no task id")?;
    let mode = options.modality.output_mode.as_deref().unwrap_or("text");
    let out = options
        .modality
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let has_text = task["result"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|file| file["name"] == "text.txt");
    if mode != "layout" && options.modality.out.is_none() {
        // Text mode without `--out`: a text that fits is the whole answer,
        // and nothing is written (D2).
        match inline_text(modality, id, task).await? {
            Inline::Stopped => return task::stopped_transfer(output, id),
            Inline::Fits(text) => {
                if output.json {
                    writeln!(output.out, "{}", serde_json::to_string(record)?)?;
                } else {
                    print_text(output, &text)?;
                }
                return Ok(0);
            }
            Inline::DoesNotFit => {}
        }
    }
    // The result set is written when the text did not fit, when `--out`
    // asked for a copy, or when layout mode wants the page images.
    let written = match task::write_results(modality, task, id, &out).await? {
        task::Written::Stopped => return task::stopped_transfer(output, id),
        task::Written::Files(paths) => paths,
    };
    if output.json {
        writeln!(output.out, "{}", serde_json::to_string(record)?)?;
        return Ok(0);
    }
    match mode {
        "layout" => print_paths(output, &written)?,
        _ if has_text => {
            let path = out.join("text.txt");
            match inline_text_written(&path)? {
                Some(text) => print_text(output, &text)?,
                None => {
                    print_text(output, &preview(&path)?)?;
                    writeln!(
                        output.out,
                        "Full text: {}",
                        safe_line(&path.display().to_string())
                    )?;
                }
            }
        }
        _ => print_paths(output, &written)?,
    }
    Ok(0)
}

/// Whether the task's `text.txt` fits the inline rule, read from the daemon.
enum Inline {
    Fits(String),
    DoesNotFit,
    /// The first Ctrl+C stopped the read.
    Stopped,
}

/// The task's `text.txt` when it fits the inline rule, read from the daemon.
async fn inline_text(modality: &mut Modality<'_>, id: &str, task: &Value) -> Result<Inline> {
    let Some(file) = task["result"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|file| file["name"] == "text.txt")
    else {
        return Ok(Inline::DoesNotFit);
    };
    if file["bytes"].as_u64().unwrap_or(u64::MAX) > INLINE_BYTES {
        return Ok(Inline::DoesNotFit);
    }
    match task::read_result(modality, id, "text.txt").await? {
        task::Read::Stopped => Ok(Inline::Stopped),
        task::Read::Bytes(bytes) => Ok(match fitting(&bytes) {
            Some(text) => Inline::Fits(text),
            None => Inline::DoesNotFit,
        }),
    }
}

/// The written `text.txt` when it fits the inline rule. The read stops one
/// byte past the inline limit, so a file above it is never read whole.
fn inline_text_written(path: &Path) -> Result<Option<String>> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(INLINE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > INLINE_BYTES {
        return Ok(None);
    }
    Ok(fitting(&bytes))
}

/// The text when it is within both inline limits (A.5).
pub(super) fn fitting(bytes: &[u8]) -> Option<String> {
    if bytes.len() as u64 > INLINE_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(bytes).to_string();
    (text.encode_utf16().count() <= INLINE_UNITS).then_some(text)
}

/// The text with terminal controls removed, with a final newline of its own.
/// Only the printed form is cleaned: the written `text.txt` and the `--json`
/// record keep the exact bytes.
fn print_text(output: &mut Output<'_>, text: &str) -> Result<()> {
    let text = safe(text);
    write!(output.out, "{text}")?;
    if !text.ends_with('\n') {
        writeln!(output.out)?;
    }
    Ok(())
}

fn print_paths(output: &mut Output<'_>, written: &[PathBuf]) -> Result<()> {
    for path in written {
        writeln!(output.out, "{}", safe_line(&path.display().to_string()))?;
    }
    Ok(())
}

/// The first [`PREVIEW_CHARS`] characters of a written `text.txt`.
fn preview(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(PREVIEW_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .chars()
        .take(PREVIEW_CHARS)
        .collect())
}

/// The `modality.task.submit` params of `dimcode ocr` (A.3): one input file,
/// and the output mode only when the command line named one.
fn request(file: &Path, output_mode: Option<&str>) -> Result<Value> {
    let name = file_name(file)?;
    let metadata =
        std::fs::metadata(file).map_err(|error| format!("{}: {error}", file.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", file.display()).into());
    }
    let mut params = Map::new();
    if let Some(mode) = output_mode {
        params.insert("outputMode".to_owned(), json!(mode));
    }
    Ok(json!({
        "category": "ocr",
        "operation": "recognize",
        "params": Value::Object(params),
        "files": [input(name, file, metadata.len() <= MAX_INLINE_BYTES)?],
    }))
}
