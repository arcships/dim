use crate::{Error, Result};
use dim_agent_capabilities::file_storage::{
    DirectoryEntry, FileLease, FilePath, FileStorage, FileStorageError,
};
use dim_agent_types::{Message, MessageContent, MessagePart, ModelId, Role};
use dim_app_server::protocol::SessionOrigin;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub database: String,
    pub tables: BTreeMap<String, u64>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecordedMessage {
    pub id: String,
    pub created_at: Option<String>,
    pub message: Message,
    pub in_context: bool,
}

pub(crate) struct SessionData {
    pub source_digest: String,
    pub id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub model: Option<ModelId>,
    pub origin: SessionOrigin,
    pub parent_session_id: Option<String>,
    pub records: Vec<RecordedMessage>,
    pub attachments: BTreeMap<String, Vec<Attachment>>,
    pub context_error: Option<String>,
    pub settings: Value,
    pub permission: Value,
    pub mode: Option<String>,
    pub tags: Value,
    pub relations: Vec<Value>,
    pub blobs: Vec<Blob>,
    pub archive: Value,
}

#[derive(Clone, Serialize)]
pub(crate) struct Blob {
    pub id: String,
    pub kind: String,
    pub reference: String,
    pub mime_type: Option<String>,
    pub bytes: u64,
    pub checksum: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attachment {
    pub attachment_id: String,
    pub kind: String,
    pub data: Value,
}

#[derive(Serialize)]
pub(crate) struct HistoricalRecord {
    pub table: String,
    pub id: String,
    pub data: serde_json::Map<String, Value>,
}

#[derive(Serialize)]
pub(crate) struct ScheduledJob {
    pub name: String,
    pub prompt: String,
    pub enabled: bool,
    pub rrule: String,
    pub cwd: String,
    pub model: ModelId,
    pub reasoning_effort: Option<String>,
    pub session_mode: String,
    pub bound_session: Option<String>,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub created_at: String,
}

pub(crate) struct Source {
    pub home: PathBuf,
    pub root: usize,
    pub snapshot: PathBuf,
    pub inventory: Inventory,
    files: Arc<FileStorage>,
    directory: FilePath,
    db: Mutex<Option<Connection>>,
    terminal: Mutex<Option<Connection>>,
    _lease: FileLease,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    end_message_id: String,
    summary: String,
    #[serde(default)]
    raw_items: Vec<Value>,
}

fn json_field(value: Option<String>) -> Result<Value> {
    value
        .map(|value| {
            serde_json::from_str(&value)
                .map_err(|_| Error::Object("Legacy JSON field is invalid".into()))
        })
        .transpose()
        .map(|value| value.unwrap_or(Value::Null))
}

fn object_error(error: rusqlite::Error) -> Error {
    match error {
        rusqlite::Error::SqliteFailure(ref failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::SystemIoFailure
                    | rusqlite::ErrorCode::DatabaseCorrupt
                    | rusqlite::ErrorCode::NotADatabase
                    | rusqlite::ErrorCode::CannotOpen
            ) =>
        {
            Error::Source("Legacy database cannot be read consistently".into())
        }
        _ => Error::Object("Legacy record does not match its storage schema".into()),
    }
}

pub fn validate_source_home(source: &Path, target: &Path) -> Result<PathBuf> {
    let source = std::fs::canonicalize(source)
        .map_err(|_| Error::Source("Legacy home is not accessible".into()))?;
    let mut ancestor = target.to_owned();
    let mut missing = Vec::new();
    let mut target = loop {
        match std::fs::canonicalize(&ancestor) {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| Error::Target("Target home cannot be resolved".into()))?
                        .to_owned(),
                );
                if !ancestor.pop() {
                    return Err(Error::Target("Target home cannot be resolved".into()));
                }
                if ancestor.as_os_str().is_empty() {
                    ancestor = std::env::current_dir()
                        .map_err(|_| Error::Target("Target home cannot be resolved".into()))?;
                }
            }
            Err(_) => return Err(Error::Target("Target home is not accessible".into())),
        }
    };
    for name in missing.into_iter().rev() {
        target.push(name);
    }
    if target == source || target.starts_with(&source) {
        return Err(Error::Source(
            "The target home must be outside the legacy home".into(),
        ));
    }
    Ok(source)
}

fn excluded_from_context(metadata: &Value) -> bool {
    let retry = metadata
        .pointer("/_dimTranscript/kind")
        .and_then(Value::as_str)
        == Some("provider_retry")
        && matches!(
            metadata
                .pointer("/_dimTranscript/visibility")
                .and_then(Value::as_str),
            Some("visible" | "hidden" | "summary")
        )
        && matches!(
            metadata
                .pointer("/_dimTranscript/owner")
                .and_then(Value::as_str),
            Some("user" | "runtime" | "system")
        );
    let shell = metadata.get("shellCommand");
    let hidden = metadata.get("source").and_then(Value::as_str) == Some("user_shell_command")
        && shell.is_some_and(|shell| {
            let outcome = shell.get("outcome");
            let valid_outcome =
                outcome.is_some_and(
                    |outcome| match outcome.get("kind").and_then(Value::as_str) {
                        Some("exited") => outcome.get("exitCode").is_some_and(Value::is_number),
                        Some("signaled") => outcome.get("signal").is_some_and(Value::is_string),
                        Some("interrupted") => true,
                        Some("failed") => outcome.get("message").is_some_and(Value::is_string),
                        _ => false,
                    },
                );
            shell.get("command").is_some_and(Value::is_string)
                && shell.get("cwd").is_some_and(Value::is_string)
                && shell
                    .get("output")
                    .and_then(Value::as_array)
                    .is_some_and(|lines| lines.iter().all(Value::is_string))
                && shell.get("hiddenLines").is_some_and(Value::is_number)
                && valid_outcome
                && shell
                    .get("outputAt")
                    .is_none_or(|value| matches!(value.as_str(), Some("start" | "end")))
                && shell.get("truncated").is_none_or(Value::is_boolean)
                && shell.get("fullOutputPath").is_none_or(Value::is_string)
                && shell.get("excludeFromContext") == Some(&Value::Bool(true))
        });
    retry || hidden
}

impl Source {
    pub async fn open(
        files: Arc<FileStorage>,
        home: PathBuf,
        target_root: usize,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let target = files.root_path(target_root)?;
        let home = validate_source_home(&home, &target)?;
        let lease = files
            .lock(&files.path(target_root, "migration/snapshot-run")?, cancel)
            .await?;
        for entry in files
            .list(&files.path(target_root, "migration/snapshots")?, cancel)
            .await?
        {
            files
                .remove(
                    &files.path(
                        target_root,
                        Path::new("migration/snapshots").join(entry.name),
                    )?,
                    cancel,
                )
                .await?;
        }
        let root = files.register_root(home.clone(), cancel).await?;
        let names: Vec<_> = ["dimcode.sqlite", "dimglobal.sqlite"]
            .into_iter()
            .filter(|name| std::fs::symlink_metadata(home.join(name)).is_ok())
            .collect();
        if names.len() != 1 {
            return Err(Error::Source(
                "Legacy home must contain exactly one dimcode.sqlite or dimglobal.sqlite".into(),
            ));
        }
        let name = names[0];
        let relative = PathBuf::from("migration/snapshots").join(uuid::Uuid::new_v4().to_string());
        let directory = files.path(target_root, &relative)?;
        files.create_directory(&directory, cancel).await?;
        let result = async {
            let copied = files.path(target_root, relative.join("legacy.sqlite"))?;
            copy_snapshot(&files, &files.path(root, name)?, &copied, cancel).await?;
            let wal = files.path(root, format!("{name}-wal"))?;
            match copy_snapshot(
                &files,
                &wal,
                &files.path(target_root, relative.join("legacy.sqlite-wal"))?,
                cancel,
            )
            .await
            {
                Ok(_) => {}
                Err(FileStorageError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let snapshot = target.join(&relative).join("legacy.sqlite");
            let path = snapshot.clone();
            let (db, mut tables) = tokio::task::spawn_blocking(move || -> Result<_> {
                let (connection, tables) = read_database(path)?;
                for (table, columns) in [
                    (
                        "sessions",
                        &[
                            "sessionId",
                            "cwd",
                            "title",
                            "status",
                            "tags",
                            "createdAt",
                            "updatedAt",
                            "version",
                        ][..],
                    ),
                    (
                        "messages",
                        &[
                            "messageId",
                            "sessionId",
                            "role",
                            "parts",
                            "attachments",
                            "metadata",
                            "orderKey",
                            "createdAt",
                        ][..],
                    ),
                    (
                        "session_states",
                        &[
                            "sessionId",
                            "selectedProviderId",
                            "selectedModelId",
                            "settings",
                            "permissionSettings",
                            "currentMode",
                        ][..],
                    ),
                    (
                        "compaction_states",
                        &["sessionId", "cursor", "compactionSegments"][..],
                    ),
                ] {
                    if !tables.contains_key(table) {
                        return Err(Error::Source(format!(
                            "Legacy database lacks required table {table}"
                        )));
                    }
                    let actual: Vec<String> = connection
                        .prepare(&format!("PRAGMA table_info({table})"))
                        .map_err(object_error)?
                        .query_map([], |row| row.get(1))
                        .map_err(object_error)?
                        .collect::<rusqlite::Result<_>>()
                        .map_err(object_error)?;
                    if columns
                        .iter()
                        .any(|column| !actual.iter().any(|actual| actual == column))
                    {
                        return Err(Error::Source(format!(
                            "Legacy table {table} has an unsupported schema"
                        )));
                    }
                }
                Ok((connection, tables))
            })
            .await
            .map_err(|_| Error::Source("Legacy snapshot reader stopped".into()))??;
            let terminal_path = snapshot.with_file_name("terminal.sqlite");
            let mut issues = Vec::new();
            let terminal = match copy_snapshot(
                &files,
                &files.path(root, "schedule-tui.sqlite")?,
                &files.path(target_root, relative.join("terminal.sqlite"))?,
                cancel,
            )
            .await
            {
                Err(FileStorageError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    match copy_snapshot(
                        &files,
                        &files.path(root, "schedule-tui.sqlite-wal")?,
                        &files.path(target_root, relative.join("terminal.sqlite-wal"))?,
                        cancel,
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(FileStorageError::Io(error))
                            if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    match tokio::task::spawn_blocking(move || read_database(terminal_path))
                        .await
                        .map_err(|_| {
                            Error::Source("Terminal schedule snapshot reader stopped".into())
                        })? {
                        Ok((db, inventory)) => {
                            tables.extend(
                                inventory
                                    .into_iter()
                                    .map(|(name, count)| (format!("terminal.{name}"), count)),
                            );
                            Some(db)
                        }
                        Err(error) => {
                            issues.push(format!("schedule-tui.sqlite: {error}"));
                            None
                        }
                    }
                }
            };
            Ok(Self {
                home,
                root,
                snapshot,
                inventory: Inventory {
                    database: name.into(),
                    tables,
                    issues,
                },
                files: files.clone(),
                directory: directory.clone(),
                db: Mutex::new(Some(db)),
                terminal: Mutex::new(terminal),
                _lease: lease,
            })
        }
        .await;
        if result.is_err() {
            files.remove(&directory, &CancellationToken::new()).await?;
        }
        result
    }

    pub fn session_ids(&self) -> Result<Vec<String>> {
        let db = self.db.lock().unwrap();
        let db = db
            .as_ref()
            .ok_or_else(|| Error::Source("Legacy snapshot is closed".into()))?;
        db.prepare("SELECT sessionId FROM sessions ORDER BY sessionId")
            .map_err(object_error)?
            .query_map([], |row| row.get(0))
            .map_err(object_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(object_error)
    }

    pub fn job_ids(&self, terminal: bool) -> Result<Vec<String>> {
        let table = if terminal {
            "terminal.scheduled_jobs"
        } else {
            "scheduled_jobs"
        };
        if !self.inventory.tables.contains_key(table) {
            return Ok(Vec::new());
        }
        let locked = if terminal {
            self.terminal.lock().unwrap()
        } else {
            self.db.lock().unwrap()
        };
        let db = locked
            .as_ref()
            .ok_or_else(|| Error::Source("Legacy schedule snapshot is closed".into()))?;
        db.prepare("SELECT jobId FROM scheduled_jobs ORDER BY jobId")
            .map_err(object_error)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(object_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(object_error)
    }

    pub fn job(&self, terminal: bool, id: &str) -> Result<ScheduledJob> {
        let locked = if terminal {
            self.terminal.lock().unwrap()
        } else {
            self.db.lock().unwrap()
        };
        let db = locked
            .as_ref()
            .ok_or_else(|| Error::Source("Legacy schedule snapshot is closed".into()))?;
        let (name,prompt,status,rrule,cwd,provider,model,effort,mode,bound,next,last,created)=db.query_row(
            "SELECT name,prompt,status,rrule,cwd,providerId,modelId,reasoningEffort,sessionMode,boundSessionId,nextRunAt,lastRunAt,createdAt FROM scheduled_jobs WHERE jobId=?1",[id],
            |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,String>(8)?,row.get::<_,Option<String>>(9)?,row.get::<_,Option<String>>(10)?,row.get::<_,Option<String>>(11)?,row.get::<_,String>(12)?)),
        ).map_err(object_error)?;
        let enabled = match status.as_str() {
            "active" => true,
            "paused" => false,
            _ => return Err(Error::Object("Legacy job status is unsupported".into())),
        };
        if !Path::new(&cwd).is_absolute()
            || id.is_empty()
            || name.is_empty()
            || provider.is_empty()
            || model.is_empty()
            || !["new", "continue"].contains(&mode.as_str())
        {
            return Err(Error::Object("Legacy job configuration is invalid".into()));
        }
        Ok(ScheduledJob {
            name,
            prompt,
            enabled,
            rrule,
            cwd,
            model: ModelId {
                provider_id: provider,
                model_id: model,
            },
            reasoning_effort: effort,
            session_mode: mode,
            bound_session: bound,
            next_run_at: next,
            last_run_at: last,
            created_at: created,
        })
    }

    pub fn session(&self, id: &str) -> Result<SessionData> {
        let db = self.db.lock().unwrap();
        let db = db
            .as_ref()
            .ok_or_else(|| Error::Source("Legacy snapshot is closed".into()))?;
        let (cwd, title, status, tags, created_at, updated_at): (String, Option<String>, String, Option<String>, String, String) = db
            .query_row(
                "SELECT cwd, title, status, tags, createdAt, updatedAt FROM sessions WHERE sessionId=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .map_err(object_error)?;
        if status != "active" && status != "archived" {
            return Err(Error::Object("Legacy Session status is unsupported".into()));
        }
        if status == "archived" {
            return Err(Error::Deferred("This client does not support archived Sessions; the entire Session remains in the legacy home".into()));
        }
        if !Path::new(&cwd).is_absolute() {
            return Err(Error::Object("Legacy Session cwd is not absolute".into()));
        }
        let state = db.query_row("SELECT selectedProviderId, selectedModelId, settings, permissionSettings, currentMode FROM session_states WHERE sessionId=?1", [id], |row| Ok((row.get::<_,Option<String>>(0)?, row.get::<_,Option<String>>(1)?, row.get::<_,Option<String>>(2)?, row.get::<_,Option<String>>(3)?, row.get::<_,Option<String>>(4)?))).optional().map_err(object_error)?;
        let (model, settings, permission, mode) = match state {
            Some((provider, model, settings, permission, mode)) => (
                match (provider, model) {
                    (Some(provider_id), Some(model_id)) => Some(ModelId {
                        provider_id,
                        model_id,
                    }),
                    (None, None) => None,
                    _ => {
                        return Err(Error::Object(
                            "Legacy Session has an incomplete model reference".into(),
                        ));
                    }
                },
                json_field(settings)?,
                json_field(permission)?,
                mode,
            ),
            None => (None, Value::Null, Value::Null, None),
        };
        let revert = settings
            .get("__revertState__")
            .and_then(|value| value.get("anchorOrderKey"))
            .and_then(Value::as_str);
        if settings.get("__revertState__").is_some() && revert.is_none() {
            return Err(Error::Object(
                "Legacy revert state has no valid boundary".into(),
            ));
        }
        let rows = db.prepare("SELECT messageId, role, parts, metadata, orderKey, createdAt, attachments FROM messages WHERE sessionId=?1 ORDER BY orderKey, messageId").map_err(object_error)?
            .query_map([id], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,Option<String>>(3)?, row.get::<_,String>(4)?, row.get::<_,String>(5)?, row.get::<_,Option<String>>(6)?))).map_err(object_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(object_error)?;
        let compaction: Option<(Option<String>, String)> = db
            .query_row(
                "SELECT cursor, compactionSegments FROM compaction_states WHERE sessionId=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(object_error)?;
        let body_digest = fingerprint(&(&rows, &compaction))?;
        let archive = serde_json::json!({"createdAt":created_at,"updatedAt":updated_at,"model":model,"revertAnchor":revert,
            "compaction":compaction,"messageMetadata":rows.iter().filter(|row|!matches!(row.3.as_deref(),None|Some("{}"|"null"))).map(|row|serde_json::json!({"messageId":row.0,"metadata":row.3})).collect::<Vec<_>>()});
        let mut context_error = None;
        let mut cursor = None;
        let mut summaries = Vec::new();
        if let Some((anchor, segments)) = compaction {
            let segments: Vec<Segment> = serde_json::from_str(&segments)
                .map_err(|_| Error::Object("Legacy compaction state is invalid".into()))?;
            if segments.iter().any(|segment| {
                !segment.raw_items.is_empty() || segment.summary == "[remote-compacted]"
            }) {
                return Err(Error::Deferred(
                    "The current Provider input cannot replay legacy remote compaction items"
                        .into(),
                ));
            }
            if let Some(anchor) = anchor {
                match rows.iter().find(|row| row.0 == anchor) {
                    Some(row) if revert.is_none_or(|cut| row.4.as_str() < cut) => {
                        cursor = Some((row.4.clone(), row.0.clone()))
                    }
                    _ => {
                        context_error = Some(
                            "Legacy compaction cursor is absent or beyond the reverted boundary"
                                .into(),
                        )
                    }
                }
            }
            for segment in segments {
                let covered = rows.iter().find(|row| row.0 == segment.end_message_id);
                if covered.is_none_or(|row| revert.is_some_and(|cut| row.4.as_str() >= cut)) {
                    context_error = Some(
                        "Legacy summary coverage cannot be retained at the reverted boundary"
                            .into(),
                    );
                }
                if !segment.summary.is_empty() {
                    summaries.push(segment.summary);
                }
            }
        }
        let mut records = Vec::new();
        if !summaries.is_empty() {
            let mut summary_id = "legacy-summary".to_owned();
            while rows.iter().any(|row| row.0 == summary_id) {
                summary_id.push('-');
            }
            records.push(RecordedMessage {
                id: summary_id,
                created_at: None,
                message: Message::text(
                    Role::User,
                    format!(
                        "Imported legacy conversation summary:\n{}",
                        summaries.join("\n\n")
                    ),
                ),
                in_context: true,
            });
        }
        let mut attachments = BTreeMap::new();
        let mut eligible = std::collections::HashSet::new();
        for (message_id, role, parts, metadata, order, time, attached) in rows {
            let role: Role = serde_json::from_value(Value::String(role))
                .map_err(|_| Error::Object("Legacy message role is unsupported".into()))?;
            let parts = serde_json::from_str(&parts)
                .map_err(|_| Error::Object("Legacy message parts are unsupported".into()))?;
            let metadata = json_field(metadata)?;
            let excluded = excluded_from_context(&metadata);
            if !excluded && revert.is_none_or(|cut| order.as_str() < cut) {
                eligible.insert(message_id.clone());
            }
            let in_context = !excluded
                && revert.is_none_or(|cut| order.as_str() < cut)
                && cursor
                    .as_ref()
                    .is_none_or(|(key, id)| (&order, &message_id) > (key, id));
            if let Some(attached) = attached {
                let attached: Vec<Attachment> = serde_json::from_str(&attached)
                    .map_err(|_| Error::Object("Legacy message attachments are invalid".into()))?;
                attachments.insert(message_id.clone(), attached);
            }
            records.push(RecordedMessage {
                id: message_id,
                created_at: Some(time),
                message: Message {
                    role,
                    content: MessageContent::Parts(parts),
                },
                in_context,
            });
        }
        if cursor.is_some() && context_error.is_none() {
            let mut required = std::collections::HashSet::new();
            let mut calls = std::collections::HashSet::new();
            for record in records.iter().filter(|record| record.in_context) {
                if let MessageContent::Parts(parts) = &record.message.content {
                    for part in parts {
                        match part {
                            MessagePart::ToolUse { id, .. } => {
                                calls.insert(id.clone());
                            }
                            MessagePart::ToolResult { tool_use_id, .. }
                                if !calls.contains(tool_use_id) =>
                            {
                                required.insert(tool_use_id.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }
            let mut boundary = None;
            let window_start = records
                .iter()
                .position(|record| record.in_context && record.created_at.is_some())
                .unwrap_or(records.len());
            for (index, record) in records.iter().enumerate().take(window_start).rev() {
                if required.is_empty() {
                    break;
                }
                if let MessageContent::Parts(parts) = &record.message.content {
                    for part in parts {
                        if let MessagePart::ToolUse { id, .. } = part
                            && required.remove(id)
                        {
                            boundary = Some(index);
                        }
                    }
                    if boundary == Some(index) {
                        for part in parts {
                            if let MessagePart::ToolResult { tool_use_id, .. } = part {
                                required.insert(tool_use_id.clone());
                            }
                        }
                    }
                }
            }
            if let Some(boundary) = boundary {
                for record in &mut records[boundary..] {
                    record.in_context = eligible.contains(&record.id);
                }
            }
        }
        if context_error.is_some() {
            for record in &mut records {
                record.in_context = false;
            }
        }
        if context_error.is_none() {
            validate_context(&records)?;
        }
        let mut relations = Vec::new();
        if self.inventory.tables.contains_key("session_relations") {
            relations = db.prepare("SELECT sourceSessionId, targetSessionId, relationType, typeData FROM session_relations WHERE sourceSessionId=?1 OR targetSessionId=?1 ORDER BY relationId").map_err(object_error)?
                .query_map([id], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,Option<String>>(3)?))).map_err(object_error)?
                .map(|row| { let (source, target, kind, data) = row.map_err(object_error)?; Ok(serde_json::json!({"sourceSessionId":source,"targetSessionId":target,"kind":kind,"data":json_field(data)?})) }).collect::<Result<_>>()?;
        }
        let metadata = &settings["metadata"];
        let mut incoming = relations
            .iter()
            .filter(|relation| relation["targetSessionId"].as_str() == Some(id));
        let mut subagent_parents = incoming
            .clone()
            .filter(|relation| relation["kind"] == "subagent")
            .filter_map(|relation| relation["sourceSessionId"].as_str());
        let first_subagent_parent = subagent_parents.next();
        let relation_parent =
            first_subagent_parent.filter(|first| subagent_parents.all(|parent| parent == *first));
        let parent_session_id = metadata["parentSessionId"]
            .as_str()
            .filter(|parent| !parent.is_empty())
            .or(relation_parent);
        let origin = if parent_session_id.is_some()
            || first_subagent_parent.is_some()
            || settings["agentProfile"] == "reviewer"
            || [
                "__approvalReviewer",
                "__subagentOrigin",
                "__workflowAgentOrigin",
            ]
            .iter()
            .any(|marker| metadata[*marker].as_bool() == Some(true))
        {
            SessionOrigin::Internal
        } else if metadata["__sessionSource"] == "acp"
            || (metadata.get("__sessionSource").is_none()
                && metadata["acp"]["additionalDirectories"].is_array()
                && metadata["acp"]["mcpServers"].is_array()
                && incoming.next().is_none())
        {
            SessionOrigin::Acp
        } else {
            SessionOrigin::Unknown
        };
        let parent_session_id = parent_session_id
            .filter(|parent| !parent.is_empty() && *parent != id)
            .map(str::to_owned);
        let mut blobs = Vec::new();
        if self.inventory.tables.contains_key("session_blobs") {
            blobs = db.prepare("SELECT blobId, kind, ref, mimeType, sizeBytes, checksum FROM session_blobs WHERE sessionId=?1 ORDER BY blobId").map_err(object_error)?
                .query_map([id], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,Option<String>>(3)?, row.get::<_,i64>(4)?, row.get::<_,Option<String>>(5)?))).map_err(object_error)?
                .map(|row| { let (id, kind, reference, mime_type, bytes, checksum) = row.map_err(object_error)?; Ok(Blob { id, kind, reference, mime_type, bytes: u64::try_from(bytes).map_err(|_| Error::Object("Legacy blob size is invalid".into()))?, checksum }) }).collect::<Result<_>>()?;
        }
        let source_digest = fingerprint(&(
            id,
            &cwd,
            &title,
            &status,
            &model,
            revert,
            body_digest,
            &relations,
            &blobs,
        ))?;
        Ok(SessionData {
            source_digest,
            id: id.into(),
            cwd,
            title,
            model,
            origin,
            parent_session_id,
            records,
            attachments,
            context_error,
            settings,
            permission,
            mode,
            tags: json_field(tags)?,
            relations,
            blobs,
            archive,
        })
    }

    pub fn history(&self, id: &str) -> Result<Vec<HistoricalRecord>> {
        use rusqlite::types::ValueRef;
        let mut records = Vec::new();
        for terminal in [false, true] {
            let locked = if terminal {
                self.terminal.lock().unwrap()
            } else {
                self.db.lock().unwrap()
            };
            let Some(db) = locked.as_ref() else { continue };
            for (table, identity) in [
                ("background_tasks", "taskId"),
                ("workflow_runs", "workflowRunId"),
                ("queue_items", "queueItemId"),
                ("permission_decisions", "decisionId"),
                ("usage_ledger", "ledgerId"),
                ("usage_run_stats", "runId"),
                ("usage_skill_events", "eventId"),
                ("scheduled_job_executions", "executionId"),
            ] {
                let name = if terminal {
                    format!("terminal.{table}")
                } else {
                    table.into()
                };
                if !self.inventory.tables.contains_key(&name) {
                    continue;
                }
                let mut query = db
                    .prepare(&format!(
                        "SELECT * FROM {table} WHERE sessionId=?1 ORDER BY {identity}"
                    ))
                    .map_err(object_error)?;
                let names = query
                    .column_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let mut rows = query.query([id]).map_err(object_error)?;
                while let Some(row) = rows.next().map_err(object_error)? {
                    let mut data = serde_json::Map::new();
                    for (index, column) in names.iter().enumerate() {
                        let value = match row.get_ref(index).map_err(object_error)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(value) => Value::from(value),
                            ValueRef::Real(value) => serde_json::Number::from_f64(value)
                                .map(Value::Number)
                                .ok_or_else(|| {
                                    Error::Object(
                                        "Legacy history contains a non-finite number".into(),
                                    )
                                })?,
                            ValueRef::Text(bytes) => {
                                let text = std::str::from_utf8(bytes).map_err(|_| {
                                    Error::Object("Legacy history text is not UTF-8".into())
                                })?;
                                if [
                                    "record",
                                    "payload",
                                    "metadata",
                                    "completion",
                                    "usage",
                                    "cost",
                                    "pricing",
                                ]
                                .contains(&column.as_str())
                                {
                                    serde_json::from_str(text)
                                        .unwrap_or_else(|_| Value::String(text.into()))
                                } else {
                                    Value::String(text.into())
                                }
                            }
                            ValueRef::Blob(_) => {
                                return Err(Error::Object(
                                    "Legacy history contains an unsupported binary column".into(),
                                ));
                            }
                        };
                        data.insert(column.clone(), value);
                    }
                    let id = data
                        .get(identity)
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            Error::Object("Legacy historical record lacks an identity".into())
                        })?
                        .to_owned();
                    records.push(HistoricalRecord {
                        table: name.clone(),
                        id,
                        data,
                    });
                }
            }
        }
        Ok(records)
    }

    pub async fn artifact(&self, path: &Path, cancel: &CancellationToken) -> Result<Vec<u8>> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.home)
                .map_err(|_| {
                    Error::Object(
                        "Historical artifact is outside the authorized source home".into(),
                    )
                })?
                .to_owned()
        } else {
            path.to_owned()
        };
        let path = self
            .files
            .path(self.root, relative)
            .map_err(|_| Error::Object("Historical artifact has an unsafe source path".into()))?;
        match self
            .files
            .read_limited(&path, 16 * 1024 * 1024, cancel)
            .await
        {
            Ok(bytes) => Ok(bytes),
            Err(FileStorageError::Cancelled) => Err(Error::Cancelled),
            Err(FileStorageError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::InvalidData
                ) =>
            {
                Err(Error::Object(
                    "Historical artifact is missing, unreadable or exceeds 16 MiB".into(),
                ))
            }
            Err(FileStorageError::InvalidPath(_)) => Err(Error::Object(
                "Historical artifact is not a regular authorized file".into(),
            )),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn json(&self, name: &str, cancel: &CancellationToken) -> Result<Option<Value>> {
        let path = self.files.path(self.root, name)?;
        match self
            .files
            .read_limited(&path, 8 * 1024 * 1024, cancel)
            .await
        {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Object(format!("Legacy {name} is not valid JSON")))?;
                if !value.is_object() {
                    return Err(Error::Object(format!("Legacy {name} must be an object")));
                }
                Ok(Some(value))
            }
            Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(FileStorageError::InvalidPath(_)) => Err(Error::Object(format!(
                "Legacy {name} is not a regular authorized file"
            ))),
            Err(FileStorageError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidData
                ) =>
            {
                Err(Error::Object(format!(
                    "Legacy {name} is unreadable or exceeds its input limit"
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn entries(
        &self,
        path: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<DirectoryEntry>> {
        match self
            .files
            .list(&self.files.path(self.root, path)?, cancel)
            .await
        {
            Ok(entries) => Ok(entries),
            Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            }
            Err(FileStorageError::InvalidPath(_)) => Err(Error::Object(
                "Legacy directory contains an unauthorized path".into(),
            )),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn blob(&self, blob: &Blob, cancel: &CancellationToken) -> Result<Vec<u8>> {
        if !["text", "binary", "file", "stream", "remote"].contains(&blob.kind.as_str()) {
            return Err(Error::Object("Legacy blob kind is unsupported".into()));
        }
        if blob.bytes > 16 * 1024 * 1024 {
            return Err(Error::Object(
                "Legacy attachment exceeds the Session file limit of 16 MiB".into(),
            ));
        }
        let embedded = if self.inventory.tables.contains_key("blob_data") {
            let locked = self.db.lock().unwrap();
            let db = locked
                .as_ref()
                .ok_or_else(|| Error::Source("Legacy snapshot is closed".into()))?;
            let length: Option<i64> = db
                .query_row(
                    "SELECT length(data) FROM blob_data WHERE blobId=?1",
                    [&blob.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(object_error)?;
            match length {
                Some(length) if u64::try_from(length).ok() != Some(blob.bytes) => {
                    return Err(Error::Object(
                        "Embedded legacy attachment does not match its recorded size".into(),
                    ));
                }
                Some(_) => Some(
                    db.query_row(
                        "SELECT data FROM blob_data WHERE blobId=?1",
                        [&blob.id],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .map_err(object_error)?,
                ),
                None => None,
            }
        } else {
            None
        };
        let bytes = match embedded {
            Some(bytes) => bytes,
            None => {
                if blob.kind == "remote" {
                    return Err(Error::Object(
                        "Remote attachment bytes are unavailable".into(),
                    ));
                }
                let reference = Path::new(&blob.reference);
                let relative = if reference.is_absolute() {
                    reference
                        .strip_prefix(&self.home)
                        .map_err(|_| {
                            Error::Object("Legacy attachment is outside the source home".into())
                        })?
                        .to_owned()
                } else {
                    Path::new("data").join(reference)
                };
                self.files
                    .read_limited(
                        &self.files.path(self.root, relative).map_err(|_| {
                            Error::Object("Legacy attachment is outside its authorized root".into())
                        })?,
                        16 * 1024 * 1024,
                        cancel,
                    )
                    .await.map_err(|error| match error {FileStorageError::Cancelled=>Error::Cancelled,FileStorageError::Closed=>Error::Source("Legacy attachment storage is closed".into()),_=>Error::Object("Legacy attachment is missing, unreadable, or outside its authorized root".into())})?
            }
        };
        if bytes.len() as u64 != blob.bytes {
            return Err(Error::Object(
                "Legacy attachment bytes do not match their recorded size".into(),
            ));
        }
        if blob
            .checksum
            .as_ref()
            .is_some_and(|expected| expected != &format!("{:x}", Sha256::digest(&bytes)))
        {
            return Err(Error::Object(
                "Legacy attachment checksum does not match its bytes".into(),
            ));
        }
        Ok(bytes)
    }

    pub async fn close(self, cancel: &CancellationToken) -> Result<()> {
        drop(self.db.lock().unwrap().take());
        drop(self.terminal.lock().unwrap().take());
        self.files.remove(&self.directory, cancel).await?;
        Ok(())
    }
}

pub(crate) fn fingerprint(value: &impl Serialize) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(value)
                .map_err(|_| Error::Object("Source identity cannot be encoded".into()))?
        )
    ))
}

fn read_database(path: PathBuf) -> Result<(Connection, BTreeMap<String, u64>)> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Error::Source("Legacy snapshot cannot be opened".into()))?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN")
        .map_err(object_error)?;
    let mut tables = BTreeMap::new();
    let names = connection.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(object_error)?
        .query_map([], |row| row.get::<_, String>(0)).map_err(object_error)?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(object_error)?;
    for name in names {
        if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::Source(
                "Legacy database contains an unsupported table identity".into(),
            ));
        }
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |row| {
                row.get(0)
            })
            .map_err(object_error)?;
        tables.insert(
            name,
            u64::try_from(count).map_err(|_| Error::Source("Invalid legacy table count".into()))?,
        );
    }

    Ok((connection, tables))
}

async fn copy_snapshot(
    files: &Arc<FileStorage>,
    source: &FilePath,
    destination: &FilePath,
    cancel: &CancellationToken,
) -> std::result::Result<(), FileStorageError> {
    let bytes = files.read(source, cancel).await?;
    files.replace(destination, bytes, None, cancel).await
}

fn validate_context(records: &[RecordedMessage]) -> Result<()> {
    let invalid = || {
        Error::Deferred("Legacy effective context has incompatible roles or incomplete tool pairs; Session retained in source".into())
    };
    let mut calls = std::collections::HashSet::new();
    let mut open = std::collections::HashSet::new();
    for record in records.iter().filter(|record| record.in_context) {
        if record.message.role != Role::ToolResult && !open.is_empty() {
            return Err(invalid());
        }
        match &record.message.content {
            MessageContent::Text(_) if record.message.role == Role::ToolResult => {
                return Err(invalid());
            }
            MessageContent::Parts(parts) => {
                if record.message.role == Role::ToolResult && parts.is_empty() {
                    return Err(invalid());
                }
                for part in parts {
                    match part {
                        MessagePart::ToolUse { id, .. } => {
                            if record.message.role != Role::Assistant
                                || id.is_empty()
                                || !calls.insert(id)
                            {
                                return Err(invalid());
                            }
                            open.insert(id);
                        }
                        MessagePart::ToolResult { tool_use_id, .. } => {
                            if record.message.role != Role::ToolResult || !open.remove(tool_use_id)
                            {
                                return Err(invalid());
                            }
                        }
                        _ if record.message.role == Role::ToolResult => return Err(invalid()),
                        MessagePart::Thinking { .. } if record.message.role != Role::Assistant => {
                            return Err(invalid());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if !open.is_empty() {
        return Err(invalid());
    }
    Ok(())
}
