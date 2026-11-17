use crate::{
    Result,
    args::{Options, SERVER_REQUIREMENT},
    client,
    client::Client,
    session::{self, HistorySink},
};
use dim_agent_types::{
    BranchCreateData, BranchCreateFailedData, ContentBlock, InputBlock, InputMessageData, TextData,
    ToolResultData, ToolUseData, markup,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    process::Stdio,
    str::FromStr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Json,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
        }
    }
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
    let Some(path) = path else {
        let mut output = BufWriter::new(std::io::stdout().lock());
        return stream(client, session_id, format, &mut output).await;
    };
    let mut file = BufWriter::new(create(path)?);
    let result = stream(client, session_id, format, &mut file).await;
    drop(file);
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

/// Writes the export to `out` as the read's pages arrive, so it holds one page
/// instead of the whole history.
async fn stream(
    client: &mut Client,
    session_id: &str,
    format: Format,
    out: &mut impl Write,
) -> Result<()> {
    match format {
        Format::Json => {
            let mut json = Json::new(out, session_id);
            session::read_history(client, session_id, 0, &mut json).await?;
            json.end()
        }
        Format::Markdown => {
            let mut markdown = Markdown::new(out, session_id)?;
            session::read_history(client, session_id, 0, &mut markdown).await?;
            markdown.end()
        }
    }
}

/// Writes the JSON document as the pages arrive. `sessionId`, `branchId` and
/// `through` precede `events`, so the document header is written when the read
/// reports its watermark, before the first page's events.
struct Json<'a, W: Write> {
    out: &'a mut W,
    session_id: &'a str,
    /// Whether no event has been written yet: the first takes no separator,
    /// and an array that stays empty closes on its opening line.
    first: bool,
}

impl<'a, W: Write> Json<'a, W> {
    fn new(out: &'a mut W, session_id: &'a str) -> Self {
        Self {
            out,
            session_id,
            first: true,
        }
    }
    fn end(&mut self) -> Result<()> {
        self.out
            .write_all(if self.first { b"]\n}\n" } else { b"\n  ]\n}\n" })?;
        self.out.flush()?;
        Ok(())
    }
}

impl<W: Write> HistorySink for Json<'_, W> {
    fn watermark(&mut self, through: u64) -> Result<()> {
        write!(
            self.out,
            "{{\n  \"sessionId\": {},\n  \"branchId\": \"main\",\n  \"through\": {through},\n  \"events\": [",
            serde_json::to_string(self.session_id)?
        )?;
        Ok(())
    }
    fn event(&mut self, event: Value) -> Result<()> {
        self.out
            .write_all(if self.first { b"\n" } else { b",\n" })?;
        self.first = false;
        for (index, line) in serde_json::to_string_pretty(&event)?
            .split('\n')
            .enumerate()
        {
            if index > 0 {
                self.out.write_all(b"\n")?;
            }
            self.out.write_all(b"    ")?;
            self.out.write_all(line.as_bytes())?;
        }
        Ok(())
    }
}

/// Writes the Markdown document as the pages arrive.
struct Markdown<'a, W: Write> {
    out: &'a mut W,
    /// The `text` or `thinking` run already opened, by name and request, so
    /// consecutive deltas of one run share its heading.
    text_request: Option<(String, String)>,
}

impl<'a, W: Write> Markdown<'a, W> {
    fn new(out: &'a mut W, session_id: &str) -> Result<Self> {
        write!(out, "# Session {session_id}\n\nBranch: main\n")?;
        Ok(Self {
            out,
            text_request: None,
        })
    }
    fn end(&mut self) -> Result<()> {
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        Ok(())
    }
    fn fence(&mut self, content: &str, language: &str) -> Result<()> {
        let length = content
            .split(|ch| ch != '`')
            .map(str::len)
            .max()
            .unwrap_or(0);
        let delimiter = "`".repeat(3.max(length + 1));
        write!(
            self.out,
            "\n{delimiter}{language}\n{content}\n{delimiter}\n"
        )?;
        Ok(())
    }
}

impl<W: Write> HistorySink for Markdown<'_, W> {
    fn event(&mut self, event: Value) -> Result<()> {
        let kind = (event["type"].as_str(), event["name"].as_str());
        let data = &event["data"];
        match kind {
            (Some("input"), Some("message")) => {
                self.text_request = None;
                let data = InputMessageData::deserialize(data)?;
                if let Some(user) = data.user {
                    self.out.write_all(b"\n\n### User\n\n")?;
                    for (index, block) in user.content.iter().enumerate() {
                        match block {
                            // Only a hook's context leaves the section; a loaded
                            // Skill's block stays here, as it always has.
                            ContentBlock::Text { text }
                                if markup::runtime_element(index, text) == Some("dim-hooks") => {}
                            ContentBlock::Text { text } => self.out.write_all(text.as_bytes())?,
                            ContentBlock::Image { source } => {
                                write!(self.out, "\n[Image attachment: {}]\n", source.media_type)?
                            }
                        }
                    }
                    for body in crate::projection::hook_contexts(&user.content) {
                        self.out.write_all(b"\n\n### Hook context\n")?;
                        self.fence(body.trim(), "text")?;
                    }
                }
                for block in data.blocks {
                    let InputBlock::Raw { text, .. } = block;
                    self.out.write_all(b"\n\n### Runtime input\n")?;
                    self.fence(&text, "text")?;
                }
            }
            (Some("step"), Some(name @ ("text" | "thinking"))) => {
                let data = TextData::deserialize(data)?;
                let key = (name.to_owned(), data.request_id);
                if self.text_request.as_ref() != Some(&key) {
                    self.out.write_all(if name == "thinking" {
                        b"\n\n### Reasoning\n\n"
                    } else {
                        b"\n\n### Assistant\n\n"
                    })?;
                    self.text_request = Some(key);
                }
                self.out.write_all(data.delta.as_bytes())?;
            }
            (Some("step"), Some("tool-use")) => {
                self.text_request = None;
                let data = ToolUseData::deserialize(data)?;
                write!(
                    self.out,
                    "\n\n### Tool call: {} ({})\n",
                    data.call.name, data.call.id
                )?;
                let input = serde_json::to_string_pretty(&data.call.input)?;
                self.fence(&input, "json")?;
            }
            (Some("step"), Some("tool-result")) => {
                self.text_request = None;
                let data = ToolResultData::deserialize(data)?;
                for result in data.results {
                    write!(self.out, "\n\n### Tool result: {}\n", result.call_id)?;
                    let value = serde_json::to_string_pretty(&result.result)?;
                    self.fence(&value, "json")?;
                }
            }
            (Some("branch"), Some("create")) => {
                self.text_request = None;
                let data = BranchCreateData::deserialize(data)?;
                write!(
                    self.out,
                    "\n\nChild branch {} (contents not exported).\n",
                    data.branch
                )?;
            }
            (Some("branch"), Some("create-failed")) => {
                self.text_request = None;
                let data = BranchCreateFailedData::deserialize(data)?;
                let (BranchCreateFailedData::Failed { branch, .. }
                | BranchCreateFailedData::Cancelled { branch, .. }) = data;
                write!(self.out, "\n\nChild branch {branch} was not created.\n")?;
            }
            _ => {}
        }
        Ok(())
    }
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
