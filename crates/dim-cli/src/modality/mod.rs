//! `dimcode modality`, the media commands and `dimcode ocr`: thin clients of
//! the `modality.*` management methods (Modality A.5).
//!
//! One method client serves every surface. A daemon that does not advertise
//! `modality.capabilities` serves no Modality at all: an older daemon never
//! has it, and a reloaded one answers `unknown_method` until it is back
//! (Modality §4), so both read as the same sentence.
//!
//! The commands the daemon owns and this module does not restate: which
//! categories exist, what an operation is unavailable for, and whether a
//! parameter is valid. The table renders the snapshot, and every refusal is
//! the owner's own message.

pub(crate) mod command;
pub(crate) mod media;
pub(crate) mod ocr;
pub(crate) mod task;

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod tests_scripted;

use crate::client::{Client, RpcError, app_error};
use crate::sanitize::safe_line;
use crate::{Result, args::Options};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The management namespace's read; a daemon without it serves no Modality
/// (Modality A.3).
pub(crate) const CAPABILITIES: &str = "modality.capabilities";

/// What a daemon that serves no Modality answers, for a command that needs it.
const NOT_AVAILABLE: &str = "This daemon serves no Modality.";

/// What the user asked a Modality command to do (A.5).
#[derive(Clone, Debug)]
pub(crate) enum Command {
    /// The capability table.
    List,
    /// `set-default <category>[.<operation>] <provider>/<model>`, or `--clear`.
    SetDefault {
        operation: String,
        model: Option<String>,
        clear: bool,
    },
    /// `image|video|speech|music|audio|asset|motion ...`: one submission.
    Submit(media::Submit),
    /// `task list [--state S] [--category C] [--limit N] [--after ID]`.
    TaskList,
    /// `task status ID`.
    TaskStatus { task_id: String },
    /// `task wait ID [--out DIR]`.
    TaskWait { task_id: String },
    /// `task cancel ID`.
    TaskCancel { task_id: String },
    /// `ocr FILE [--output-mode text|layout] [--out DIR]`.
    Ocr { file: PathBuf },
}

/// The `modality` method client.
pub(crate) struct Modality<'a> {
    client: &'a mut Client,
}

impl<'a> Modality<'a> {
    /// The client, or the answer for a daemon that serves no Modality.
    pub(crate) fn open(client: &'a mut Client) -> Result<Self> {
        if !client.supports(&[CAPABILITIES]) {
            return Err(NOT_AVAILABLE.into());
        }
        Ok(Self { client })
    }

    /// One management call. A method the daemon no longer serves — its owner
    /// reloaded — reads as the same sentence, without a retry.
    pub(crate) async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.client.rpc(method, params).await.map_err(|error| {
            if matches!(
                app_error(&error),
                Some("unknown_method" | "service_reloading")
            ) {
                NOT_AVAILABLE.into()
            } else {
                error
            }
        })
    }

    /// `modality.capabilities` `{}`: every category and operation with its
    /// availability, its reasons, its default and its effective model.
    pub(crate) async fn capabilities(&mut self) -> Result<Value> {
        self.call(CAPABILITIES, json!({})).await
    }

    /// `modality.task.submit`: the queued record.
    pub(crate) async fn submit(&mut self, params: Value) -> Result<Value> {
        self.call("modality.task.submit", params)
            .await
            .map_err(refusal)
    }

    /// The connection, for the calls that read their progress as it arrives.
    pub(crate) fn client(&mut self) -> &mut Client {
        self.client
    }
}

/// The owner's message alone: a Modality refusal is display text the client
/// prints as it is (A.3), without the RPC framing.
pub(crate) fn refusal(error: crate::Error) -> crate::Error {
    match error.downcast::<RpcError>() {
        Ok(error) => error.message.into(),
        Err(error) => error,
    }
}

/// `PROVIDER/MODEL` as every CLI model reference is written: the provider is
/// the text before the first `/`, and a model id may hold the rest.
pub(crate) fn model_reference(reference: &str) -> Result<Value> {
    let Some((provider_id, model_id)) = reference.split_once('/') else {
        return Err(format!("{reference} is not PROVIDER/MODEL").into());
    };
    if provider_id.is_empty() || model_id.is_empty() {
        return Err(format!("{reference} is not PROVIDER/MODEL").into());
    }
    Ok(json!({"providerId": provider_id, "modelId": model_id}))
}

/// `<category>[.<operation>]`, split at the first `.`.
pub(crate) fn operation_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('.') {
        Some((category, operation)) => (category, Some(operation)),
        None => (key, None),
    }
}

/// Every operation of the snapshot as one row:
/// `CATEGORY OPERATION AVAILABLE DEFAULT EFFECTIVE REASON`, tab separated, the
/// reason column holding each `dimension: detail` the owner gives.
fn row(category: &Value, operation: &Value) -> String {
    let cell = |value: String| safe_line(&value);
    [
        cell(text(&category["category"])),
        cell(text(&operation["operation"])),
        cell(if operation["available"] == true {
            "yes".to_owned()
        } else {
            "no".to_owned()
        }),
        cell(model(&operation["default"])),
        cell(effective(&operation["effective"])),
        cell(reasons(&operation["reasons"])),
    ]
    .join("\t")
}

/// The header of [`row`].
const HEADER: &str = "CATEGORY\tOPERATION\tAVAILABLE\tDEFAULT\tEFFECTIVE\tREASON";

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("-").to_owned()
}

/// `provider/model`, or `-` for a null model reference.
fn model(value: &Value) -> String {
    match (value["providerId"].as_str(), value["modelId"].as_str()) {
        (Some(provider_id), Some(model_id)) => format!("{provider_id}/{model_id}"),
        _ => "-".to_owned(),
    }
}

/// `provider/model (source)`, or `-` when nothing resolves.
fn effective(value: &Value) -> String {
    if value.is_null() {
        return "-".to_owned();
    }
    format!("{} ({})", model(value), text(&value["source"]))
}

/// Each reason as `dimension: detail`, joined on one line.
fn reasons(value: &Value) -> String {
    let lines: Vec<String> = value
        .as_array()
        .into_iter()
        .flatten()
        .map(|reason| {
            format!(
                "{}: {}",
                text(&reason["dimension"]),
                text(&reason["detail"])
            )
        })
        .collect();
    if lines.is_empty() {
        "-".to_owned()
    } else {
        lines.join("; ")
    }
}

/// The capability table: one line per operation, every category present.
pub(crate) fn table(snapshot: &Value) -> String {
    let mut output = format!("{HEADER}\n");
    for category in snapshot["categories"].as_array().into_iter().flatten() {
        for operation in category["operations"].as_array().into_iter().flatten() {
            output.push_str(&row(category, operation));
            output.push('\n');
        }
    }
    output
}

/// The row of `<category>[.<operation>]` in `snapshot`, when the snapshot has
/// it: what `set-default` reports after a write.
pub(crate) fn row_of(snapshot: &Value, key: &str) -> Option<String> {
    let (category_name, operation_name) = operation_key(key);
    let category = snapshot["categories"]
        .as_array()?
        .iter()
        .find(|category| category["category"].as_str() == Some(category_name))?;
    let operation = category["operations"]
        .as_array()?
        .iter()
        .find(|operation| {
            operation_name.is_none_or(|name| operation["operation"].as_str() == Some(name))
        })?;
    Some(row(category, operation))
}

/// Runs one parsed Modality command and returns the process's exit code.
pub(crate) async fn run(
    client: &mut Client,
    options: &Options,
    command: &Command,
    output: &mut task::Output<'_>,
) -> Result<i32> {
    command::run(client, options, command, output).await
}
