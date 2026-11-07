//! Submitting a Modality task, waiting for it and fetching its results
//! (Modality A.3, A.5).
//!
//! `modality.task.wait` returns at its own timeout with the record, so the
//! CLI repeats the call while the state is not terminal; an OCR of a long
//! document may run for half an hour, and Ctrl+C is the way out. Progress is
//! drawn on stderr and only when stderr is a terminal, so a pipe stays clean
//! and stdout carries the record (or the id) alone.

use super::{Modality, refusal};
use crate::Result;
use crate::args::ModalityFlags;
use crate::sanitize::{safe, safe_line};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dim_client::Message;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// The longest one wait asks for; a longer task is waited for in several
/// calls (A.3).
pub(crate) const WAIT_MS: u64 = 600_000;

/// The largest chunk one `modality.task.result` call returns (A.3).
const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// Where a command writes: its stdout, its stderr, and the two flags that
/// decide what goes there.
pub(crate) struct Output<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// `--json`: print the task record instead of the command's own lines.
    pub json: bool,
    /// Whether progress may be drawn: only a terminal keeps the stream
    /// readable (A.5).
    pub progress: bool,
}

/// Whether `state` ends a task (A.4).
pub(crate) fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "interrupted")
}

/// Submits one task, then waits for it unless `--no-wait`, and applies the
/// outcome (A.5, D5, D6).
pub(crate) async fn submit(
    modality: &mut Modality<'_>,
    request: Value,
    flags: &ModalityFlags,
    output: &mut Output<'_>,
) -> Result<i32> {
    let record = modality.submit(request).await?;
    let id = task_id(&record)?.to_owned();
    if flags.no_wait {
        // `--json` changes only what is printed: the queued record, or the
        // id alone (D5).
        if output.json {
            line(output.out, &serde_json::to_string(&record)?)?;
        } else {
            line(output.out, &id)?;
        }
        return Ok(0);
    }
    let Some(record) = wait(modality, &id, output).await? else {
        // The first Ctrl+C stops waiting; the task continues (A.5, A8).
        line(output.out, &id)?;
        line(output.err, "Stopped waiting; the task continues.")?;
        return Ok(130);
    };
    let out = flags.out.clone().unwrap_or_else(|| PathBuf::from("."));
    finish(modality, record, Some(&out), output).await
}

/// Waits for `task_id` until it is terminal, one `modality.task.wait` call at
/// a time, and renders the progress the daemon sends. `None` means Ctrl+C
/// stopped the wait.
pub(crate) async fn wait(
    modality: &mut Modality<'_>,
    task_id: &str,
    output: &mut Output<'_>,
) -> Result<Option<Value>> {
    loop {
        let waiting = async {
            let request = modality
                .client()
                .send(
                    "modality.task.wait",
                    json!({"task_id": task_id, "timeout_ms": WAIT_MS}),
                )
                .await?;
            loop {
                match modality.client().next().await? {
                    Message::Response { id, result } if id == request => {
                        return result.map_err(|error| refusal(error.into()));
                    }
                    Message::Notice { method, params }
                        if method == "management.progress"
                            && params["id"].as_u64() == Some(request) =>
                    {
                        render_progress(output, &params["data"])?;
                    }
                    // No other request is in flight while a task is waited
                    // for, so a message for one cannot arrive here.
                    _ => {}
                }
            }
        };
        let record = tokio::select! {
            record = waiting => record?,
            _ = tokio::signal::ctrl_c() => return Ok(None),
        };
        let state = record["task"]["state"]
            .as_str()
            .ok_or("modality.task.wait answered without a task state")?;
        if terminal(state) {
            return Ok(Some(record));
        }
    }
}

/// Draws one progress report: its fraction, its message, or both.
fn render_progress(output: &mut Output<'_>, data: &Value) -> Result<()> {
    if !output.progress {
        return Ok(());
    }
    let text = progress_text(data);
    if !text.is_empty() {
        line(output.err, &text)?;
    }
    Ok(())
}

/// One progress report as text: its fraction, its message, or both.
pub(crate) fn progress_text(data: &Value) -> String {
    let mut text = String::new();
    if let Some(fraction) = data["fraction"].as_f64() {
        text.push_str(&format!("{:.0}%", fraction * 100.0));
    }
    if let Some(message) = data["message"].as_str() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&safe(message));
    }
    text
}

/// Applies a terminal record: its results are written under `out` when one is
/// given, `--json` prints the record, and the exit code follows D6 —
/// succeeded 0, failed 2, cancelled 130, interrupted 1.
pub(crate) async fn finish(
    modality: &mut Modality<'_>,
    record: Value,
    out: Option<&Path>,
    output: &mut Output<'_>,
) -> Result<i32> {
    let task = record["task"].clone();
    let id = task_id(&record)?.to_owned();
    let state = task["state"]
        .as_str()
        .ok_or("a task record has no state")?
        .to_owned();
    if output.json {
        line(output.out, &serde_json::to_string(&record)?)?;
    }
    Ok(match state.as_str() {
        "succeeded" => match out {
            Some(out) => match write_results(modality, &task, &id, out).await? {
                Written::Stopped => return stopped_transfer(output, &id),
                Written::Files(paths) => {
                    if !output.json {
                        for path in paths {
                            line(output.out, &safe_line(&path.display().to_string()))?;
                        }
                    }
                    0
                }
            },
            None => {
                if !output.json {
                    line(output.out, &format!("Succeeded {id}."))?;
                }
                0
            }
        },
        "failed" => {
            let error = &task["error"];
            let code = error["code"].as_str().unwrap_or("");
            let message = error["message"].as_str().unwrap_or("The task failed");
            line(
                output.err,
                &format!("{id} failed: {}", safe_line(&nonempty(code, message))),
            )?;
            2
        }
        "cancelled" => {
            if !output.json {
                line(output.out, &format!("Cancelled {id}."))?;
            }
            130
        }
        "interrupted" => {
            line(output.err, &format!("{id} was interrupted."))?;
            1
        }
        other => {
            line(output.err, &format!("{id} is {other}."))?;
            1
        }
    })
}

/// `code: message` when a code is stated, the message alone otherwise.
fn nonempty<'a>(code: &'a str, message: &'a str) -> String {
    if code.is_empty() {
        message.to_owned()
    } else {
        format!("{code}: {message}")
    }
}

/// What writing a result set did.
pub(crate) enum Written {
    /// Every file was placed; these are the paths, in the record's order.
    Files(Vec<PathBuf>),
    /// The first Ctrl+C stopped the transfer: the temporary is gone and no
    /// target was touched.
    Stopped,
}

/// What reading a result file into memory did.
pub(crate) enum Read {
    Bytes(Vec<u8>),
    /// The first Ctrl+C stopped the read.
    Stopped,
}

/// A stopped transfer's answer: the task id, the sentence naming the command
/// that fetches the results, and exit 130.
pub(crate) fn stopped_transfer(output: &mut Output<'_>, id: &str) -> Result<i32> {
    line(output.out, id)?;
    line(
        output.err,
        &format!(
            "Stopped; the task keeps its results. Fetch them with: \
             dimcode modality task wait {id} --out <dir>"
        ),
    )?;
    Ok(130)
}

/// Writes every result file under `out`; the first Ctrl+C stops the whole
/// set. No target is replaced, and a file already placed stays when a later
/// one stops the command.
pub(crate) async fn write_results(
    modality: &mut Modality<'_>,
    task: &Value,
    id: &str,
    out: &Path,
) -> Result<Written> {
    let files: Vec<String> = task["result"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| file["name"].as_str().map(str::to_owned))
        .collect();
    if files.is_empty() {
        return Ok(Written::Files(Vec::new()));
    }
    // A result name is one path component: a name that is a directory or an
    // absolute path never decides where a file lands (A.4).
    for name in &files {
        if !one_component(name) {
            return Err(format!("{name} is not a result file name").into());
        }
    }
    check_targets(out, &files, id)?;
    std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
    let mut written = Vec::with_capacity(files.len());
    for name in files {
        let path = out.join(&name);
        let transfer = transfer(modality, id, &name, &path);
        tokio::select! {
            result = transfer => result?,
            _ = tokio::signal::ctrl_c() => return Ok(Written::Stopped),
        }
        written.push(path);
    }
    Ok(Written::Files(written))
}

/// Whether `name` is exactly one normal path component.
pub(super) fn one_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// Refuses to write any of `names` when one of them exists: a second run in
/// the same directory would otherwise replace the first run's results. The
/// task keeps its files, and the message names the command that fetches them
/// into another directory (D1).
pub(super) fn check_targets(out: &Path, names: &[String], id: &str) -> Result<()> {
    let existing: Vec<&str> = names
        .iter()
        .filter(|name| out.join(name.as_str()).symlink_metadata().is_ok())
        .map(String::as_str)
        .collect();
    if existing.is_empty() {
        return Ok(());
    }
    Err(refused(&existing, id))
}

/// The refusal a target that exists, or appeared while a transfer ran, gets.
fn refused(names: &[&str], id: &str) -> crate::Error {
    format!(
        "{} already exists; nothing was written. Fetch the results into another directory: \
         dimcode modality task wait {id} --out <dir>",
        names.join(", ")
    )
    .into()
}

/// The temporary a transfer writes. Dropping it before the file is placed
/// removes it, so a stopped or failed transfer leaves nothing behind; after
/// the file is placed the same drop removes only the second link.
struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Reads one result file in chunks into a temporary sibling and places it
/// under the target name.
async fn transfer(
    modality: &mut Modality<'_>,
    task_id: &str,
    name: &str,
    path: &Path,
) -> Result<()> {
    let partial = Partial(partial_path(path));
    let mut file = std::fs::File::create_new(&partial.0)
        .map_err(|error| format!("{}: {error}", partial.0.display()))?;
    let mut offset = 0;
    loop {
        let chunk = modality
            .call(
                "modality.task.result",
                json!({
                    "task_id": task_id,
                    "name": name,
                    "offset": offset,
                    "length": CHUNK_BYTES,
                }),
            )
            .await?;
        let data = STANDARD
            .decode(chunk["data_base64"].as_str().unwrap_or_default())
            .map_err(|_| format!("{name}: the daemon sent invalid base64"))?;
        if data.is_empty() && chunk["eof"].as_bool() != Some(true) {
            return Err(format!("{name}: the daemon returned no data before the end").into());
        }
        file.write_all(&data)?;
        offset += data.len() as u64;
        if chunk["eof"].as_bool() == Some(true) {
            break;
        }
    }
    file.flush()?;
    drop(file);
    place(&partial.0, path, name, task_id)
}

/// Places the transferred file under its target name without replacing
/// anything. A hard link is tried first because it publishes the complete
/// file at once. The copy is the fallback where hard links are unsupported,
/// and it never replaces a file; during that copy the target may briefly be
/// visible incomplete, which is the cost on such file systems.
fn place(partial: &Path, path: &Path, name: &str, id: &str) -> Result<()> {
    match std::fs::hard_link(partial, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(refused(&[name], id))
        }
        Err(_) => copy_in_place(partial, path, name, id),
    }
}

/// The fallback where hard links are unsupported: the target is created with
/// `create_new` (`O_EXCL` on Unix, `CREATE_NEW` on Windows), so an existing
/// file is refused rather than replaced, and the temporary's bytes are copied
/// into it. A copy that fails removes the target this call created, which it
/// owns.
pub(super) fn copy_in_place(partial: &Path, path: &Path, name: &str, id: &str) -> Result<()> {
    let mut target = match std::fs::File::create_new(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(refused(&[name], id));
        }
        Err(error) => return Err(format!("{}: {error}", path.display()).into()),
    };
    let copied = std::fs::File::open(partial)
        .and_then(|mut source| std::io::copy(&mut source, &mut target).map(|_| ()))
        .and_then(|()| target.flush());
    if let Err(error) = copied {
        drop(target);
        let _ = std::fs::remove_file(path);
        return Err(format!("{}: {error}", path.display()).into());
    }
    Ok(())
}

/// The sibling a transfer writes before it renames it into place.
fn partial_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("result");
    path.with_file_name(format!(".{name}.partial-{}", uuid::Uuid::new_v4()))
}

fn task_id(record: &Value) -> Result<&str> {
    record["task"]["task_id"]
        .as_str()
        .ok_or_else(|| "a task record has no task id".into())
}

fn line(writer: &mut dyn Write, text: &str) -> Result<()> {
    writeln!(writer, "{text}")?;
    writer.flush()?;
    Ok(())
}

/// Reads one whole result file into memory, which the first Ctrl+C stops;
/// only for a file already known to be small (A.3).
pub(crate) async fn read_result(
    modality: &mut Modality<'_>,
    task_id: &str,
    name: &str,
) -> Result<Read> {
    let reading = async {
        let mut bytes = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = modality
                .call(
                    "modality.task.result",
                    json!({
                        "task_id": task_id,
                        "name": name,
                        "offset": offset,
                        "length": CHUNK_BYTES,
                    }),
                )
                .await?;
            let data = STANDARD
                .decode(chunk["data_base64"].as_str().unwrap_or_default())
                .map_err(|_| format!("{name}: the daemon sent invalid base64"))?;
            if data.is_empty() && chunk["eof"].as_bool() != Some(true) {
                return Err(format!("{name}: the daemon returned no data before the end").into());
            }
            bytes.extend_from_slice(&data);
            offset += data.len() as u64;
            if chunk["eof"].as_bool() == Some(true) {
                return Ok::<Vec<u8>, crate::Error>(bytes);
            }
        }
    };
    tokio::select! {
        bytes = reading => Ok(Read::Bytes(bytes?)),
        _ = tokio::signal::ctrl_c() => Ok(Read::Stopped),
    }
}
