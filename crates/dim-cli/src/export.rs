use crate::{
    Result,
    args::{Options, SERVER_REQUIREMENT},
    client,
    client::Client,
    session,
};
use dim_agent_types::{
    BranchCreateData, BranchCreateFailedData, ContentBlock, InputBlock, InputMessageData, TextData,
    ToolResultData, ToolUseData,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    process::Stdio,
    str::FromStr,
};

#[derive(Debug, Clone, Copy)]
pub enum Format {
    Markdown,
    Json,
}

impl FromStr for Format {
    type Err = crate::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            _ => Err("Export format must be markdown or json".into()),
        }
    }
}

pub async fn write(
    client: &mut Client,
    session_id: &str,
    format: Format,
    path: Option<&Path>,
) -> Result<()> {
    let mut events = Vec::new();
    let through = session::read_history(client, session_id, 0, |event| {
        events.push(event);
        Ok(())
    })
    .await?;
    let text = match format {
        Format::Json => {
            serde_json::to_string_pretty(
                &json!({"sessionId":session_id,"branchId":"main","through":through,"events":events}),
            )? + "\n"
        }
        Format::Markdown => markdown(session_id, &events)?,
    };
    if let Some(path) = path {
        let mut file = create(path)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        let label = match format {
            Format::Markdown => "Markdown",
            Format::Json => "JSON",
        };
        eprintln!(
            "Exported {label}: {}",
            crate::sanitize::safe(&path.display().to_string())
        );
    } else {
        let mut output = std::io::stdout().lock();
        output.write_all(text.as_bytes())?;
        output.flush()?;
    }
    Ok(())
}

/// Creates an export file: never an existing path, private on Unix.
fn create(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// `dimcode export`: the App Server binary's one-shot Timeline export of
/// the database, without a daemon (Timeline export design §7).
pub async fn timeline(
    options: &Options,
    session_id: &str,
    path: Option<&Path>,
    database: Option<&Path>,
) -> Result<()> {
    let server = client::server(options)?;
    if !server.program.is_file() {
        return Err(if server.embedded {
            format!(
                "App Server binary is missing at {}; build dim-app-server or supply --server PATH",
                server.program.display()
            )
        } else {
            format!(
                "{}; {} is missing",
                SERVER_REQUIREMENT,
                server.program.display()
            )
        }
        .into());
    }
    let database = database
        .map(Path::to_path_buf)
        .unwrap_or_else(|| options.home.join("rust").join("dimcode.sqlite"));
    let mut command = tokio::process::Command::new(&server.program);
    command
        .args(server.subcommand())
        .arg("--export-timeline")
        .arg(session_id)
        .arg("--database")
        .arg(&database)
        .stderr(Stdio::piped());
    let Some(path) = path else {
        return run_export(command.stdout(Stdio::inherit())).await;
    };
    let result = run_export(command.stdout(create(path)?)).await;
    match &result {
        Err(_) => {
            let _ = std::fs::remove_file(path);
        }
        Ok(()) => eprintln!(
            "Exported Timeline (sensitive): {}",
            crate::sanitize::safe(&path.display().to_string())
        ),
    }
    result
}

/// Runs the export and returns its error line as the error.
async fn run_export(command: &mut tokio::process::Command) -> Result<()> {
    let output = command.spawn()?.wait_with_output().await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    Err(match message.strip_prefix("Error: ") {
        Some(message) => message.to_owned(),
        None if message.is_empty() => format!("Timeline export failed ({})", output.status),
        None => message.to_owned(),
    }
    .into())
}

fn fence(output: &mut String, content: &str, language: &str) {
    let length = content
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let delimiter = "`".repeat(3.max(length + 1));
    output.push_str(&format!(
        "\n{delimiter}{language}\n{content}\n{delimiter}\n"
    ));
}

fn markdown(session_id: &str, events: &[Value]) -> Result<String> {
    let mut output = format!("# Session {session_id}\n\nBranch: main\n");
    let mut text_request = None;
    for event in events {
        let kind = (event["type"].as_str(), event["name"].as_str());
        let data = &event["data"];
        match kind {
            (Some("input"), Some("message")) => {
                text_request = None;
                let data = InputMessageData::deserialize(data)?;
                if let Some(user) = data.user {
                    output.push_str("\n\n### User\n\n");
                    for block in user.content {
                        match block {
                            ContentBlock::Text { text } => output.push_str(&text),
                            ContentBlock::Image { source } => {
                                output.push_str(&format!(
                                    "\n[Image attachment: {}]\n",
                                    source.media_type
                                ));
                            }
                        }
                    }
                }
                for block in data.blocks {
                    let InputBlock::Raw { text, .. } = block;
                    output.push_str("\n\n### Runtime input\n");
                    fence(&mut output, &text, "text");
                }
            }
            (Some("step"), Some(name @ ("text" | "thinking"))) => {
                let data = TextData::deserialize(data)?;
                let key = (name.to_owned(), data.request_id);
                if text_request.as_ref() != Some(&key) {
                    output.push_str(if name == "thinking" {
                        "\n\n### Reasoning\n\n"
                    } else {
                        "\n\n### Assistant\n\n"
                    });
                    text_request = Some(key);
                }
                output.push_str(&data.delta);
            }
            (Some("step"), Some("tool-use")) => {
                text_request = None;
                let data = ToolUseData::deserialize(data)?;
                output.push_str(&format!(
                    "\n\n### Tool call: {} ({})\n",
                    data.call.name, data.call.id
                ));
                fence(
                    &mut output,
                    &serde_json::to_string_pretty(&data.call.input)?,
                    "json",
                );
            }
            (Some("step"), Some("tool-result")) => {
                text_request = None;
                let data = ToolResultData::deserialize(data)?;
                for result in data.results {
                    output.push_str(&format!("\n\n### Tool result: {}\n", result.call_id));
                    fence(
                        &mut output,
                        &serde_json::to_string_pretty(&result.result)?,
                        "json",
                    );
                }
            }
            (Some("branch"), Some("create")) => {
                text_request = None;
                let data = BranchCreateData::deserialize(data)?;
                output.push_str(&format!(
                    "\n\nChild branch {} (contents not exported).\n",
                    data.branch
                ));
            }
            (Some("branch"), Some("create-failed")) => {
                text_request = None;
                let data = BranchCreateFailedData::deserialize(data)?;
                let (BranchCreateFailedData::Failed { branch, .. }
                | BranchCreateFailedData::Cancelled { branch, .. }) = data;
                output.push_str(&format!("\n\nChild branch {branch} was not created.\n"));
            }
            _ => {}
        }
    }
    output.push('\n');
    Ok(output)
}
