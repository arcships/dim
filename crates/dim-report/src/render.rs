use crate::evidence::{CapturedError, Missing, Snapshot, TimelineEvidence};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_capabilities::methods::{RpcError, reply};
use dim_app_server::protocol::{PROTOCOL_VERSION, SERVER_VERSION};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    io::{Cursor, Write},
};
use url::Url;
use zip::{ZipWriter, write::SimpleFileOptions};

const ZIP_LIMIT: usize = 64 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Reviewed {
    #[serde(flatten)]
    evidence: Evidence,
    unavailable: Option<Missing>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    captured_error: Option<CapturedError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeline: Option<TimelineEvidence>,
    process: ProcessDiagnostics,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessDiagnostics {
    product_version: &'static str,
    protocol_version: u32,
    platform: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum InstructionsStatus {
    None,
    Applied,
    PartiallyApplied,
    Unsupported,
}

pub(super) fn review(
    snapshot: &Snapshot,
    redact: bool,
    instructions: &str,
) -> (Reviewed, InstructionsStatus) {
    let (mut timeline, mut code, mut correlation) = (true, true, true);
    let (mut recognized, mut unsupported) = (false, false);
    for instruction in instructions
        .split([',', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match instruction.to_ascii_lowercase().as_str() {
            "omit timeline" => timeline = false,
            "keep timeline" => timeline = true,
            "omit error codes" => code = false,
            "keep error codes" => code = true,
            "omit correlation ids" => correlation = false,
            "keep correlation ids" if !redact => correlation = true,
            _ => {
                unsupported = true;
                continue;
            }
        }
        recognized = true;
    }
    let status = match (recognized, unsupported) {
        (false, false) => InstructionsStatus::None,
        (true, false) => InstructionsStatus::Applied,
        (false, true) => InstructionsStatus::Unsupported,
        (true, true) => InstructionsStatus::PartiallyApplied,
    };
    let mut captured_error = snapshot.error.clone();
    if !code && let Some(error) = &mut captured_error {
        error.code = None;
    }
    let (timeline, unavailable) = match &snapshot.timeline {
        Err(reason) => (None, Some(*reason)),
        Ok(_) if !timeline => (None, Some(Missing::Omitted)),
        Ok(evidence) => {
            let mut evidence = evidence.clone();
            if (redact || !correlation)
                && let TimelineEvidence::Failure { correlation, .. } = &mut evidence
            {
                *correlation = None;
            }
            (Some(evidence), None)
        }
    };
    (
        Reviewed {
            evidence: Evidence {
                captured_error,
                timeline,
                process: ProcessDiagnostics {
                    product_version: SERVER_VERSION,
                    protocol_version: PROTOCOL_VERSION,
                    platform: std::env::consts::OS,
                },
            },
            unavailable,
        },
        status,
    )
}

impl Reviewed {
    pub(super) fn generate(self) -> Result<Value, RpcError> {
        let (title, version, mut body) = match &self.evidence.captured_error {
            Some(error) => (
                error.summary,
                1,
                format!(
                    "## Error\n\n{}.\n\n- Observed at {}\n",
                    error.summary,
                    error.observed_at.to_rfc3339()
                ),
            ),
            None => ("Session report", 2, "## Session\n\n".to_owned()),
        };
        write!(
            body,
            "- Dim {} (protocol {})\n- Platform {}\n",
            self.evidence.process.product_version,
            self.evidence.process.protocol_version,
            self.evidence.process.platform,
        )
        .expect("writing to a string");
        if let Some(code) = self
            .evidence
            .captured_error
            .as_ref()
            .and_then(|error| error.code)
        {
            writeln!(body, "- Error code `{code}`").expect("writing to a string");
        }
        if let Some(timeline) = &self.evidence.timeline {
            let (summary, truncated) = match timeline {
                TimelineEvidence::Failure {
                    nearby, truncated, ..
                } => (
                    format!(
                        "Timeline failure and {} nearby records in diagnostics",
                        nearby.len()
                    ),
                    truncated,
                ),
                TimelineEvidence::Recent { recent, truncated } => (
                    format!(
                        "{} recent session activity records in diagnostics",
                        recent.len()
                    ),
                    truncated,
                ),
            };
            writeln!(
                body,
                "- {summary}{}",
                if *truncated { " (bounded excerpt)" } else { "" },
            )
            .expect("writing to a string");
        }
        if let Some(reason) = self.unavailable {
            let message = match reason {
                Missing::NoReference => "No Timeline failure reference was supplied.",
                Missing::FailureUnavailable => "The referenced Timeline failure was unavailable.",
                Missing::ReadFailed => "The Timeline evidence could not be read.",
                Missing::Omitted => "Timeline evidence was omitted from this report.",
            };
            writeln!(body, "\n{message}").expect("writing to a string");
        }
        let report = format!("# {title}\n\n{body}");
        let diagnostics = serde_json::to_string_pretty(&json!({
            "version": version,
            "evidence": self.evidence,
            "unavailable": self.unavailable,
        }))
        .map_err(|_| failed())?;
        let files = [("report.md", report), ("diagnostics.json", diagnostics)];
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in &files {
            zip.start_file(*name, SimpleFileOptions::default())
                .map_err(|_| failed())?;
            zip.write_all(content.as_bytes()).map_err(|_| failed())?;
        }
        let archive = zip.finish().map_err(|_| failed())?.into_inner();
        if archive.len() > ZIP_LIMIT {
            return Err(failed());
        }
        let mut github =
            Url::parse("https://github.com/arcships/dim/issues/new").expect("constant URL");
        github
            .query_pairs_mut()
            .append_pair("title", title)
            .append_pair("body", &body);
        reply(json!({
            "title": title,
            "body": body,
            "files": std::collections::BTreeMap::from(files),
            "archive": {"filename": "dim-report.zip", "base64": STANDARD.encode(archive)},
            "sharing": {
                "githubIssueUrl": github.as_str(),
                "discordUrl": "https://discord.gg/ds35K3TadP",
                "description": body,
            },
        }))
    }
}

fn failed() -> RpcError {
    RpcError::service(
        "report_generation_failed",
        "Could not build the report archive",
    )
}
