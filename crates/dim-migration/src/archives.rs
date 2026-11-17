use crate::{
    Error, Result,
    source::{HistoricalRecord, SessionData, Source, fingerprint},
};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Archive {
    version: u32,
    source: &'static str,
    session_id: String,
    meaning: &'static str,
    metadata: Value,
    relations: Vec<Value>,
    pub records: Vec<HistoricalRecord>,
    artifacts: Vec<Artifact>,
    #[serde(skip)]
    pub files: BTreeMap<String, Vec<u8>>,
    #[serde(skip)]
    pub omitted: Vec<String>,
}

#[derive(Serialize)]
struct Artifact {
    source: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable: Option<String>,
}

impl Archive {
    pub async fn read(
        source: &Source,
        session: &SessionData,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let records = source.history(&session.id)?;
        let mut paths = BTreeMap::new();
        let mut omitted = Vec::new();
        let has_workflow = records.iter().any(|record| record.table == "workflow_runs");
        let workflow = if !has_workflow {
            None
        } else {
            match source.json("config.json", cancel).await {
                Ok(config) => match config
                    .as_ref()
                    .and_then(|config| config.get("workflowDataDir"))
                {
                    None => Some(source.home.join("data")),
                    Some(Value::String(path)) if std::path::Path::new(path).is_absolute() => {
                        Some(PathBuf::from(path))
                    }
                    Some(_) => {
                        omitted.push("Workflow artifacts: source data directory is invalid".into());
                        None
                    }
                },
                Err(Error::Object(_)) => {
                    omitted.push("Workflow artifacts: source configuration is invalid".into());
                    None
                }
                Err(error) => return Err(error),
            }
        };
        for record in &records {
            match record.table.as_str() {
                "permission_decisions" if record.data.get("remember") == Some(&Value::from(1)) => {
                    omitted.push(format!("remembered permission {}: no equivalent Session-scoped native grant mapping",record.id));
                }
                "background_tasks" => {
                    if let Some(path) = record.data.get("outputPath").and_then(Value::as_str) {
                        paths.insert(PathBuf::from(path), "background output");
                    }
                }
                "workflow_runs" => {
                    let run = record.data.get("record");
                    if let Some(path) = run
                        .and_then(|run| run.pointer("/source/savedPath"))
                        .and_then(Value::as_str)
                    {
                        paths.insert(PathBuf::from(path), "Workflow source");
                    }
                    if let Some(workflow) = &workflow
                        && matches!(
                            run.and_then(|run| run.pointer("/source/kind"))
                                .and_then(Value::as_str),
                            Some("inline" | "session")
                        )
                    {
                        let directory = workflow
                            .join("workflows/sessions")
                            .join(&session.id)
                            .join(&record.id);
                        paths.insert(directory.join("workflow.ts"), "Workflow source");
                        paths.insert(directory.join("meta.json"), "Workflow metadata");
                    }
                }
                _ => {}
            }
        }
        let mut files = BTreeMap::new();
        let mut artifacts = Vec::new();
        for (path, kind) in paths {
            let provenance = path.trim_prefix(&source.home).to_owned();
            match source.artifact(&path, cancel).await {
                Ok(bytes) => {
                    let id = format!("history:{}", fingerprint(&provenance)?);
                    files.insert(id.clone(), bytes);
                    artifacts.push(Artifact {
                        source: provenance,
                        file: Some(id),
                        path: None,
                        unavailable: None,
                    });
                }
                Err(Error::Object(reason)) => {
                    omitted.push(format!("{kind}: {reason}"));
                    artifacts.push(Artifact {
                        source: provenance,
                        file: None,
                        path: None,
                        unavailable: Some(reason),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        let mut metadata = session.archive.clone();
        metadata["blobs"] = serde_json::to_value(&session.blobs)
            .map_err(|_| Error::Object("Cannot encode historical blob metadata".into()))?;
        metadata["attachments"] = serde_json::to_value(&session.attachments)
            .map_err(|_| Error::Object("Cannot encode historical attachment metadata".into()))?;
        Ok(Self {
            version: 1,
            source: "legacy-session",
            session_id: session.id.clone(),
            meaning: "Imported historical metadata and results. These records do not restore execution, queues, approvals or native usage measurements.",
            metadata,
            relations: session.relations.clone(),
            records,
            artifacts,
            files,
            omitted,
        })
    }

    pub fn encode(mut self, paths: &BTreeMap<String, String>) -> Result<Vec<u8>> {
        self.metadata["importedFiles"] = serde_json::to_value(paths)
            .map_err(|_| Error::Object("Cannot encode historical file mapping".into()))?;
        for artifact in &mut self.artifacts {
            if let Some(id) = &artifact.file {
                artifact.path = Some(
                    paths
                        .get(id)
                        .ok_or_else(|| {
                            Error::Target(
                                "Historical artifact has no committed file receipt".into(),
                            )
                        })?
                        .clone(),
                );
            }
        }
        let bytes = serde_json::to_vec_pretty(&self)
            .map_err(|_| Error::Object("Historical metadata cannot be encoded".into()))?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::Object(
                "Historical metadata exceeds the 16 MiB Session file limit".into(),
            ));
        }
        Ok(bytes)
    }
}
