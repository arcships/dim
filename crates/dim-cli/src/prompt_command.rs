//! `dimcode prompt ...`: the operator's overrides of the built-in policy texts
//! through the App Server's `settings` methods ([prompt
//! overrides](../docs/design/appserver/prompt-overrides.md)).
//!
//! The daemon owns the document and each capability reads its own field. This
//! client presents one field as plain text: an empty draft writes nothing, and
//! `reset` removes the override. Every write uses the revision it read.
use crate::model::session::{PromptField, PromptSetting};
use crate::{Result, args::Options, client::Client, management::editor};
use serde_json::{Map, Value, json};
use std::io::{IsTerminal, Read};

const SOURCE: &str = "prompts";

#[derive(Clone, Debug)]
pub enum Command {
    List,
    Show { field: PromptField },
    Edit { field: PromptField },
    Set { field: PromptField, path: String },
    Reset { field: PromptField },
}

pub fn field(name: &str) -> Result<PromptField> {
    PromptField::parse(name)
        .ok_or_else(|| format!("Use base, summary or skills, not {name}").into())
}

/// The `skills` text replaces the protocol sentences that name the marker and
/// call shape; the tool and the list are not changed.
fn warn(field: PromptField) {
    if field == PromptField::Skills {
        eprintln!(
            "skills replaces the whole block, including the marker and call shape; \
             the skill tool and dim/skills/list stay"
        );
    }
}

pub async fn run(client: &mut Client, options: &Options, command: Command) -> Result<i32> {
    if !client.supports(&["settings.read", "settings.update"]) {
        return Err("This daemon does not provide prompt overrides".into());
    }
    match command {
        Command::List => list(client).await,
        Command::Show { field } => show(client, field).await,
        Command::Edit { field } => edit(client, options, field).await,
        Command::Set { field, path } => set(client, field, &path).await,
        Command::Reset { field } => reset(client, field).await,
    }
}

async fn read(client: &mut Client) -> Result<PromptSetting> {
    client
        .typed("settings.read", json!({"source": SOURCE, "scope": "user"}))
        .await
}

fn document(snapshot: &PromptSetting) -> Map<String, Value> {
    snapshot.document.as_object().cloned().unwrap_or_default()
}

async fn write(
    client: &mut Client,
    snapshot: &PromptSetting,
    field: PromptField,
    value: Option<&str>,
) -> Result<PromptSetting> {
    let mut document = document(snapshot);
    match value {
        Some(value) => {
            document.insert(field.key().into(), Value::String(value.to_owned()));
        }
        None => {
            document.remove(field.key());
        }
    }
    client
        .typed(
            "settings.update",
            json!({
                "source": SOURCE,
                "scope": "user",
                "expectedRevision": snapshot.revision,
                "document": document,
            }),
        )
        .await
}

async fn list(client: &mut Client) -> Result<i32> {
    let snapshot = read(client).await?;
    println!("prompts (user scope, revision {})", snapshot.revision);
    for field in PromptField::ALL {
        match snapshot.text(field) {
            Some(text) => println!("  {:<8} custom ({} B)", field.key(), text.len()),
            None => println!("  {:<8} built-in", field.key()),
        }
    }
    Ok(0)
}

async fn show(client: &mut Client, field: PromptField) -> Result<i32> {
    let snapshot = read(client).await?;
    match snapshot.text(field) {
        Some(text) => {
            println!("{text}");
            Ok(0)
        }
        None => Err(format!("{}: built-in (no override)", field.key()).into()),
    }
}

async fn edit(client: &mut Client, options: &Options, field: PromptField) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(format!(
            "prompt edit needs a terminal; prompt set {} PATH writes a file, or - its input",
            field.key()
        )
        .into());
    }
    let snapshot = read(client).await?;
    let current = snapshot.text(field).unwrap_or_default().to_owned();
    warn(field);
    let draft = editor::Draft::open_text(&options.home, field.key(), "md", &current)
        .map_err(|error| error.to_string())?;
    let kept = draft.path().display().to_string();
    let mut command = editor::command(draft.path());
    let program = command.get_program().to_string_lossy().into_owned();
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            draft.remove();
            return Err(format!("{program}: {error}").into());
        }
    };
    if !status.success() {
        return Err(format!(
            "{} Nothing was written. Your text is kept in {kept}.",
            editor::exited(status)
        )
        .into());
    }
    let text = draft
        .text()
        .map_err(|error| format!("{error} Your text is kept in {kept}."))?;
    if text.trim().is_empty() {
        draft.remove();
        return Err(format!(
            "An empty draft writes nothing; use `dimcode prompt reset {}` to remove the override.",
            field.key()
        )
        .into());
    }
    if text == current {
        draft.remove();
        println!("{}: unchanged", field.key());
        return Ok(0);
    }
    match write(client, &snapshot, field, Some(&text)).await {
        Ok(_) => {
            draft.remove();
            println!("{}: saved; applies to new Sessions", field.key());
            Ok(0)
        }
        Err(error) => Err(format!("{error} Your text is kept in {kept}.").into()),
    }
}

async fn set(client: &mut Client, field: PromptField, path: &str) -> Result<i32> {
    let mut body = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut body)?;
    } else {
        body = std::fs::read_to_string(path)?;
    }
    if body.trim().is_empty() {
        return Err(format!(
            "An empty text writes nothing; use `dimcode prompt reset {}` to remove the override.",
            field.key()
        )
        .into());
    }
    let snapshot = read(client).await?;
    if snapshot.text(field) == Some(body.as_str()) {
        println!("{}: unchanged", field.key());
        return Ok(0);
    }
    warn(field);
    write(client, &snapshot, field, Some(&body)).await?;
    println!("{}: saved; applies to new Sessions", field.key());
    Ok(0)
}

async fn reset(client: &mut Client, field: PromptField) -> Result<i32> {
    let snapshot = read(client).await?;
    if snapshot.text(field).is_none() {
        println!("{}: built-in", field.key());
        return Ok(0);
    }
    write(client, &snapshot, field, None).await?;
    println!(
        "{}: reset to the built-in text; applies to new Sessions",
        field.key()
    );
    Ok(0)
}
