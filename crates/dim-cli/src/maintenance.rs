//! `dimcode daemon vacuum`: the App Server binary's one-shot `VACUUM` of the
//! database, without a daemon (#600).
use crate::{Result, args::Options, client};
use std::{path::Path, process::Stdio};

pub async fn vacuum(options: &Options, database: Option<&Path>) -> Result<()> {
    let server = client::server(options)?;
    if !server.program.is_file() {
        return Err(format!(
            "App Server binary is missing at {}; build dim-app-server or supply --server PATH",
            server.program.display()
        )
        .into());
    }
    let database = database
        .map(Path::to_path_buf)
        .unwrap_or_else(|| options.home.join("rust").join("dimcode.sqlite"));
    let output = tokio::process::Command::new(&server.program)
        .args(server.subcommand())
        .arg("--vacuum")
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()?
        .wait_with_output()
        .await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    Err(match message.strip_prefix("Error: ") {
        Some(message) => message.to_owned(),
        None if message.is_empty() => format!("Timeline maintenance failed ({})", output.status),
        None => message.to_owned(),
    }
    .into())
}
