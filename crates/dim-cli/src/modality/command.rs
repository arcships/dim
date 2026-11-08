//! `dimcode modality`: the capability table, `set-default`, and the category
//! submissions `image`, `video`, `speech`, `music`, `audio`, `asset` and
//! `motion` (Modality A.5).

use super::task::{self, Output};
use super::{
    Command, Modality, media, model_reference, ocr, operation_key, refusal, row_of, table,
};
use crate::client::{Client, app_error};
use crate::sanitize::safe_line;
use crate::{Result, args::Options};
use serde_json::{Value, json};

pub(crate) async fn run(
    client: &mut Client,
    options: &Options,
    command: &Command,
    output: &mut Output<'_>,
) -> Result<i32> {
    match command {
        Command::List => list(client, options, output).await,
        Command::SetDefault {
            operation,
            model,
            clear,
        } => set_default(client, options, operation, model.as_deref(), *clear, output).await,
        Command::Submit(submit) => media::run(client, submit, options, output).await,
        Command::TaskList => task_list(client, options, output).await,
        Command::TaskStatus { task_id } => task_status(client, options, task_id, output).await,
        Command::TaskWait { task_id } => task_wait(client, options, task_id, output).await,
        Command::TaskCancel { task_id } => task_cancel(client, options, task_id, output).await,
        Command::Ocr { file } => ocr::run(client, file, options, output).await,
    }
}

/// `dimcode modality`: the whole snapshot, as a table or unchanged.
async fn list(client: &mut Client, options: &Options, output: &mut Output<'_>) -> Result<i32> {
    let mut modality = Modality::open(client)?;
    let snapshot = modality.capabilities().await?;
    if options.json {
        writeln!(output.out, "{}", serde_json::to_string(&snapshot)?)?;
    } else {
        write!(output.out, "{}", table(&snapshot))?;
    }
    Ok(0)
}

/// `dimcode modality set-default <category>[.<operation>] <provider>/<model>`
/// or `--clear`: `modality.configure` with the revision read immediately
/// before, retried once after a `modality_conflict` because only another
/// client's save can cause one (A.3, A.4).
async fn set_default(
    client: &mut Client,
    options: &Options,
    key: &str,
    reference: Option<&str>,
    clear: bool,
    output: &mut Output<'_>,
) -> Result<i32> {
    let (category, operation) = operation_key(key);
    if category.is_empty() || operation == Some("") {
        return Err(format!("{key} is not <category>[.<operation>]").into());
    }
    let model = match (reference, clear) {
        (Some(_), true) => return Err("Use --clear without PROVIDER/MODEL".into()),
        (Some(reference), false) => Some(model_reference(reference)?),
        (None, true) => None,
        (None, false) => return Err("Name PROVIDER/MODEL, or use --clear".into()),
    };
    let mut modality = Modality::open(client)?;
    let mut revision = revision_of(&mut modality).await?;
    let mut retried = false;
    loop {
        let mut params = json!({
            "expectedRevision": revision,
            "category": category,
            "model": model.clone().unwrap_or(Value::Null),
        });
        if let Some(operation) = operation {
            params["operation"] = json!(operation);
        }
        let reply = match modality.call("modality.configure", params).await {
            Ok(reply) => reply,
            Err(error) if app_error(&error) == Some("modality_conflict") && !retried => {
                retried = true;
                revision = revision_of(&mut modality).await?;
                continue;
            }
            Err(error) => return Err(refusal(error)),
        };
        if options.json {
            writeln!(output.out, "{}", serde_json::to_string(&reply)?)?;
            return Ok(0);
        }
        match reference {
            Some(reference) => writeln!(output.out, "Set {key} to {reference}.")?,
            None => writeln!(output.out, "Cleared {key}.")?,
        }
        if let Some(row) = row_of(&reply, key) {
            writeln!(output.out, "{row}")?;
        }
        return Ok(0);
    }
}

/// The revision the next `modality.configure` carries.
async fn revision_of(modality: &mut Modality<'_>) -> Result<String> {
    let snapshot = modality.capabilities().await?;
    snapshot["revision"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "modality.capabilities answered without a revision".into())
}

/// The columns `task list` and `task status` share (A.5).
const TASK_HEADER: &str = "ID\tSTATE\tOPERATION\tMODEL\tCREATED";

/// `dimcode modality task list [--state S] [--category C] [--limit N]
/// [--after ID]`: one row per task, newest first, and the page hint on
/// stderr so stdout stays parseable (A.5, A.9).
async fn task_list(client: &mut Client, options: &Options, output: &mut Output<'_>) -> Result<i32> {
    let mut modality = Modality::open(client)?;
    let mut params = serde_json::Map::new();
    let flags = &options.modality;
    if let Some(state) = &flags.state {
        params.insert("state".to_owned(), json!(state));
    }
    if let Some(category) = &flags.category {
        params.insert("category".to_owned(), json!(category));
    }
    if let Some(limit) = flags.limit {
        params.insert("limit".to_owned(), json!(limit));
    }
    if let Some(after) = &flags.after {
        params.insert("after".to_owned(), json!(after));
    }
    let reply = modality
        .call("modality.task.list", Value::Object(params))
        .await
        .map_err(refusal)?;
    if options.json {
        writeln!(output.out, "{}", serde_json::to_string(&reply)?)?;
        return Ok(0);
    }
    writeln!(output.out, "{TASK_HEADER}")?;
    for task in reply["tasks"].as_array().into_iter().flatten() {
        writeln!(output.out, "{}", task_row(task))?;
    }
    if let Some(next) = reply["next"].as_str() {
        writeln!(output.err, "More: --after {next}")?;
    }
    Ok(0)
}

/// `dimcode modality task status ID`: the task's row, then its progress, its
/// error and one line per result file (A.5).
async fn task_status(
    client: &mut Client,
    options: &Options,
    task_id: &str,
    output: &mut Output<'_>,
) -> Result<i32> {
    let mut modality = Modality::open(client)?;
    let record = modality
        .call("modality.task.status", json!({"task_id": task_id}))
        .await
        .map_err(refusal)?;
    if options.json {
        writeln!(output.out, "{}", serde_json::to_string(&record)?)?;
        return Ok(0);
    }
    let task = &record["task"];
    writeln!(output.out, "{TASK_HEADER}")?;
    writeln!(output.out, "{}", task_row(task))?;
    if let Some(progress) = task["progress"].as_object() {
        let text = task::progress_text(&Value::Object(progress.clone()));
        if !text.is_empty() {
            writeln!(output.out, "Progress: {text}")?;
        }
    }
    if let Some(error) = task["error"].as_object() {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("The task failed");
        let text = if code.is_empty() {
            message.to_owned()
        } else {
            format!("{code}: {message}")
        };
        writeln!(output.out, "Error: {}", safe_line(&text))?;
    }
    for file in task["result"]["files"].as_array().into_iter().flatten() {
        writeln!(
            output.out,
            "Result: {}\t{}\t{}",
            safe_line(file["name"].as_str().unwrap_or("-")),
            safe_line(file["media_type"].as_str().unwrap_or("-")),
            file["bytes"].as_u64().unwrap_or(0),
        )?;
    }
    Ok(0)
}

/// `dimcode modality task wait ID [--out DIR]`: the same wait a submission
/// does, writing results only when `--out` names a directory (A.5).
async fn task_wait(
    client: &mut Client,
    options: &Options,
    task_id: &str,
    output: &mut Output<'_>,
) -> Result<i32> {
    let mut modality = Modality::open(client)?;
    let Some(record) = task::wait(&mut modality, task_id, output).await? else {
        writeln!(output.out, "{task_id}")?;
        writeln!(output.err, "Stopped waiting; the task continues.")?;
        return Ok(130);
    };
    task::finish(
        &mut modality,
        record,
        options.modality.out.as_deref(),
        output,
    )
    .await
}

/// `dimcode modality task cancel ID`: a terminal task is returned unchanged
/// and says so (A.3).
async fn task_cancel(
    client: &mut Client,
    options: &Options,
    task_id: &str,
    output: &mut Output<'_>,
) -> Result<i32> {
    let mut modality = Modality::open(client)?;
    let record = modality
        .call("modality.task.cancel", json!({"task_id": task_id}))
        .await
        .map_err(refusal)?;
    if options.json {
        writeln!(output.out, "{}", serde_json::to_string(&record)?)?;
        return Ok(0);
    }
    let state = record["task"]["state"].as_str().unwrap_or("unknown");
    if state == "cancelled" {
        writeln!(output.out, "Cancelled {task_id}.")?;
    } else {
        writeln!(output.out, "{state} {task_id} (unchanged).")?;
    }
    Ok(0)
}

/// One task as the two task tables print it.
fn task_row(task: &Value) -> String {
    [
        safe_line(task["task_id"].as_str().unwrap_or("-")),
        safe_line(task["state"].as_str().unwrap_or("-")),
        safe_line(&format!(
            "{}.{}",
            task["category"].as_str().unwrap_or("-"),
            task["operation"].as_str().unwrap_or("-")
        )),
        safe_line(&executor_model(&task["executor"])),
        safe_line(task["created_at"].as_str().unwrap_or("-")),
    ]
    .join("\t")
}

/// `provider/model` of the executor: the provider model, or the host target.
fn executor_model(executor: &Value) -> String {
    let model = match executor["kind"].as_str() {
        Some("host") => &executor["target"],
        _ => &executor["provider"],
    };
    match (model["providerId"].as_str(), model["modelId"].as_str()) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        _ => "-".to_owned(),
    }
}
