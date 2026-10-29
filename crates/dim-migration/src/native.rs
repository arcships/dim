use crate::{
    Error, Outcome, Result,
    source::{SessionData, fingerprint},
};
use dim_agent_capabilities::file_storage::FileStorage;
use dim_agent_runtime::{BranchRecord, SessionConfig, StoredEvent};
use dim_agent_types::*;
use dim_app_server::storage::SqliteStorage;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_HISTORY_RECORD_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn failure(_: impl std::fmt::Display) -> Error {
    Error::Target(
        "Native migration storage operation failed; source and committed objects are preserved"
            .into(),
    )
}

pub(crate) struct Native {
    pub db: Mutex<Connection>,
    pub files: Arc<FileStorage>,
    pub directory: PathBuf,
    _lease: File,
}

impl Native {
    pub async fn open(files: Arc<FileStorage>, directory: &Path) -> Result<Self> {
        if !directory.as_os_str().is_empty() {
            let relative = files.path(0, directory)?;
            files
                .create_directory(&relative, &CancellationToken::new())
                .await?;
        }
        let path = files.root_path(0)?.join(directory).join("dimcode.sqlite");
        let storage = SqliteStorage::open(path.clone()).map_err(|error| {
            Error::Target(format!(
                "Cannot open native target store: {error}; target preserved"
            ))
        })?;
        storage.close().await.map_err(failure)?;
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("sqlite.lock"))
            .map_err(failure)?;
        lease.try_lock().map_err(|_| {
            Error::Target("Target store is in use; close target clients before importing".into())
        })?;
        let (checked, version) = SqliteStorage::open_read_only(path.clone()).map_err(failure)?;
        checked.close().await.map_err(failure)?;
        if version != 22 {
            return Err(Error::Target(
                "Migration requires native storage format 22; target preserved".into(),
            ));
        }
        let db = Connection::open(path).map_err(failure)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
            .map_err(failure)?;
        Ok(Self {
            db: Mutex::new(db),
            files,
            directory: directory.to_owned(),
            _lease: lease,
        })
    }

    pub async fn put_file(
        &self,
        name: &str,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<(String, String)> {
        let container = self.directory.join("dimcode.sqlite.session-files");
        self.files
            .create_directory(&self.files.path(0, &container)?, cancel)
            .await?;
        let id = uuid::Uuid::new_v4().to_string();
        let directory = container.join(&id);
        self.files
            .create_directory(&self.files.path(0, &directory)?, cancel)
            .await?;
        let relative = directory.join(name);
        #[cfg(unix)]
        let permissions = Some(std::os::unix::fs::PermissionsExt::from_mode(0o400));
        #[cfg(not(unix))]
        let permissions = {
            let mut permissions = std::fs::metadata(
                self.files
                    .root_path(0)?
                    .join(&self.directory)
                    .join("dimcode.sqlite"),
            )
            .map_err(failure)?
            .permissions();
            permissions.set_readonly(true);
            Some(permissions)
        };
        let written = self
            .files
            .replace(&self.files.path(0, &relative)?, bytes, permissions, cancel)
            .await;
        if written.is_err() {
            self.files
                .remove(&self.files.path(0, &directory)?, &CancellationToken::new())
                .await?;
        }
        written?;
        Ok((
            id,
            self.files
                .root_path(0)?
                .join(relative)
                .to_string_lossy()
                .into_owned(),
        ))
    }

    pub async fn discard_files(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            let owned: bool = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_file_owners WHERE file_id=?1)",
                    [id],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if !owned {
                self.files
                    .remove(
                        &self.files.path(
                            0,
                            self.directory.join("dimcode.sqlite.session-files").join(id),
                        )?,
                        &CancellationToken::new(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub fn document(&self, source: &str) -> Result<Option<Value>> {
        let text: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT document FROM settings_documents WHERE source=?1",
                [source],
                |r| r.get(0),
            )
            .optional()
            .map_err(failure)?;
        text.map(|text| serde_json::from_str(&text).map_err(failure))
            .transpose()
    }

    pub fn save_document(db: &Connection, source: &str, document: &Value) -> Result<()> {
        db.execute("INSERT INTO settings_documents VALUES (?1,1,?2,'migration') ON CONFLICT(source) DO UPDATE SET version=version+1,document=excluded.document WHERE settings_documents.document!=excluded.document",
            params![source, serde_json::to_string(document).map_err(failure)?]).map_err(failure)?;
        Ok(())
    }

    pub async fn fill(
        &self,
        source: &str,
        fields: Value,
        cancel: &CancellationToken,
    ) -> Result<BTreeMap<String, String>> {
        let key = format!("user/{source}");
        let mut document = self.settings(source, cancel).await?;
        let stored: Option<(i64, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT version,imported_revision FROM settings_documents WHERE source=?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(failure)?;
        let coverage_key = format!("migration/settings/{source}");
        let previous = self.document(&coverage_key)?;
        let mut configured: std::collections::BTreeSet<String> = previous
            .as_ref()
            .and_then(|value| value.get("fields"))
            .map(|value| serde_json::from_value(value.clone()).map_err(failure))
            .transpose()?
            .unwrap_or_default();
        let current = document
            .as_object()
            .ok_or_else(|| Error::Object("Non-object target settings are preserved".into()))?;
        if previous.as_ref().is_none_or(|value| {
            value["version"].as_i64() != stored.as_ref().map(|(version, _)| *version)
        }) {
            match &stored {
                Some((1, origin)) if origin == "missing" => {}
                _ => configured.extend(current.keys().cloned()),
            }
        }
        let fields = fields
            .as_object()
            .ok_or_else(|| Error::Object("Invalid converted settings".into()))?;
        let mut grouped = document.clone();
        for (field, value) in fields {
            if !configured.contains(field) {
                grouped[field] = value.clone();
            }
        }
        let valid_group = validate_settings(source, &grouped).is_ok();
        let mut result = BTreeMap::new();
        for (field, value) in fields {
            let status = if configured.contains(field) {
                if document.get(field) == Some(value) {
                    "unchanged"
                } else {
                    "conflict"
                }
            } else {
                let mut candidate = document.clone();
                candidate[field] = value.clone();
                if valid_group || validate_settings(source, &candidate).is_ok() {
                    document = candidate;
                    configured.insert(field.clone());
                    "imported"
                } else {
                    "rejected"
                }
            };
            result.insert(field.clone(), status.to_owned());
        }
        if valid_group && result.values().any(|status| status == "imported") {
            document = grouped;
        }
        if result.values().any(|s| s == "imported") {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(failure)?;
            Self::save_document(&tx, &key, &document)?;
            let version: i64 = tx
                .query_row(
                    "SELECT version FROM settings_documents WHERE source=?1",
                    [&key],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            Self::save_document(
                &tx,
                &coverage_key,
                &json!({"version":version,"fields":configured}),
            )?;
            tx.commit().map_err(failure)?;
        }
        Ok(result)
    }

    pub async fn settings(&self, source: &str, cancel: &CancellationToken) -> Result<Value> {
        let key = format!("user/{source}");
        let document = match self.document(&key)? {
            Some(value) => value,
            None => match self
                .files
                .read(&self.files.path(0, format!("{source}.json"))?, cancel)
                .await
            {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Object("Invalid target settings are preserved".into()))?,
                Err(dim_agent_capabilities::file_storage::FileStorageError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    json!({})
                }
                Err(e) => return Err(e.into()),
            },
        };
        Ok(document)
    }

    pub async fn plugin(
        &self,
        name: &str,
        enabled: bool,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let receipt = format!("migration/plugin/{name}");
        let previous = self.document(&receipt)?;
        if let Some(previous) = &previous {
            if previous["enabled"] != enabled {
                return Ok(Outcome::Conflict);
            }
            if previous["complete"] == true {
                return Ok(Outcome::AlreadyImported);
            }
        }
        let path = self.files.path(0, "plugins.json")?;
        let mut lease = self.files.lock(&path, cancel).await?;
        let mut record: Value = match lease.read(cancel).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                Error::Object("Invalid target plugin configuration is preserved".into())
            })?,
            Err(dim_agent_capabilities::file_storage::FileStorageError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                json!({"version":1,"names":{}})
            }
            Err(e) => return Err(e.into()),
        };
        if record["version"] != 1
            || !record.is_object()
            || record
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !["version", "installed", "names"].contains(&key.as_str()))
        {
            return Err(Error::Object(
                "Unsupported target plugin configuration is preserved".into(),
            ));
        }
        if record.get("names").is_none() {
            record["names"] = json!({});
        }
        let names = record["names"]
            .as_object_mut()
            .ok_or_else(|| Error::Object("Invalid target plugin choices are preserved".into()))?;
        if previous.is_some() {
            if names.get(name) == Some(&json!({"off":!enabled})) {
                Self::save_document(
                    &self.db.lock().unwrap(),
                    &receipt,
                    &json!({"enabled":enabled,"complete":true}),
                )?;
                return Ok(Outcome::AlreadyImported);
            }
            return Ok(Outcome::Conflict);
        }
        if names.contains_key(name) {
            return Ok(Outcome::Conflict);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Self::save_document(
            &self.db.lock().unwrap(),
            &receipt,
            &json!({"enabled":enabled,"complete":false}),
        )?;
        names.insert(name.into(), json!({"off":!enabled}));
        lease
            .replace(
                serde_json::to_vec_pretty(&record).map_err(failure)?,
                None,
                &CancellationToken::new(),
            )
            .await?;
        Self::save_document(
            &self.db.lock().unwrap(),
            &receipt,
            &json!({"enabled":enabled,"complete":true}),
        )?;
        Ok(Outcome::Imported)
    }

    pub fn session_receipt(&self, id: &str, digest: &str) -> Result<Option<(Outcome, String)>> {
        let key = format!("migration/session/{}", fingerprint(&id)?);
        let Some(receipt) = self.document(&key)? else {
            return Ok(None);
        };
        let session = receipt["session"]
            .as_str()
            .ok_or_else(|| Error::Target("Invalid migration receipt; target preserved".into()))?
            .to_owned();
        let exists: bool = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id=?1)",
                [&session],
                |r| r.get(0),
            )
            .map_err(failure)?;
        Ok(Some((
            if !exists {
                Outcome::Deleted
            } else if receipt["digest"] == digest {
                Outcome::AlreadyImported
            } else {
                Outcome::Conflict
            },
            session,
        )))
    }

    pub fn session(&self, data: &SessionData, digest: &str, files: &[String]) -> Result<String> {
        let session = uuid::Uuid::new_v4().to_string();
        let events = history(data)?;
        for event in &events {
            if serde_json::to_vec(&event.event).map_err(failure)?.len() > MAX_HISTORY_RECORD_BYTES {
                return Err(Error::Object("Converted history record exceeds the native 8 MiB migration limit; Session retained in source".into()));
            }
        }
        let mut connection = self.db.lock().unwrap();
        let tx = connection.transaction().map_err(failure)?;
        let agent: Option<String> = tx
            .query_row("SELECT agent_id FROM agents WHERE is_default=1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(failure)?;
        let agent = match agent {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let profile = AgentProfile {
                    name: "default".into(),
                    description: String::new(),
                    system_prompt: String::new(),
                    tools: None,
                    model: None,
                };
                tx.execute(
                    "INSERT INTO agents VALUES (?1,?2,1)",
                    params![id, serde_json::to_string(&profile).map_err(failure)?],
                )
                .map_err(failure)?;
                id
            }
        };
        let mut config = SessionConfig {
            cwd: data.cwd.clone(),
            tools: None,
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::WorkspaceWrite,
                ..Default::default()
            },
            ..Default::default()
        };
        let profile: String = tx
            .query_row(
                "SELECT profile FROM agents WHERE agent_id=?1",
                [&agent],
                |row| row.get(0),
            )
            .map_err(failure)?;
        let profile: AgentProfile = serde_json::from_str(&profile).map_err(failure)?;
        config.system_prompt = profile.system_prompt;
        config.tools = profile.tools;
        if let Some(model) = &data.model {
            config.model.provider_id = model.provider_id.clone();
            config.model.model_id = model.model_id.clone();
        } else if let Some(model) = profile.model {
            config.model.provider_id = model.provider_id;
            config.model.model_id = model.model_id;
        } else if let Some((provider, model)) = tx
            .query_row(
                "SELECT provider_id,model_id FROM provider_selections WHERE scope='global'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(failure)?
        {
            config.model.provider_id = provider;
            config.model.model_id = model;
        }
        let now = dim_agent_runtime::core::unix_millis();
        tx.execute(
            "INSERT INTO agent_sessions VALUES (?1,?2,?3)",
            params![
                session,
                serde_json::to_string(&config).map_err(failure)?,
                i64::try_from(now).map_err(failure)?
            ],
        )
        .map_err(failure)?;
        tx.execute(
            "INSERT INTO session_owners VALUES (?1,?2,3)",
            params![session, agent],
        )
        .map_err(failure)?;
        let branch = BranchRecord {
            id: "main".into(),
            parent: None,
            branch_point: None,
            create_anchor: None,
        };
        tx.execute(
            "INSERT INTO timeline_branches VALUES (?1,'main',?2,?3)",
            params![
                session,
                serde_json::to_string(&branch).map_err(failure)?,
                i64::try_from(events.len()).map_err(failure)?
            ],
        )
        .map_err(failure)?;
        for record in events {
            let event = record.event;
            let batch_end = record.batch_end;
            let wire = json!({"event":event,"refs":null,"trace":null,"batch_end":batch_end});
            let body = match &event.kind {
                EventKind::Text(data) | EventKind::Thinking(data) => {
                    let mut wire = wire;
                    let fields = wire["event"]["data"].as_object_mut().unwrap();
                    fields.remove("turn_id");
                    fields.remove("request_id");
                    json!({"encoding":"output","turn_id":data.turn_id,"request_id":data.request_id,"started_at":event.create_at,"records":[wire]})
                }
                _ => json!({"encoding":"event","record":wire}),
            };
            tx.execute(
                "INSERT INTO timeline_events VALUES (?1,'main',?2,?2,?3,?4,?5)",
                params![
                    session,
                    i64::try_from(event.sequence).map_err(failure)?,
                    event.kind.key(),
                    body.to_string(),
                    if matches!(event.kind, EventKind::Text(_) | EventKind::Thinking(_)) {
                        None
                    } else {
                        Some(event.id.clone())
                    }
                ],
            )
            .map_err(failure)?;
        }
        if let Some(title) = &data.title {
            tx.execute(
                "INSERT INTO session_metadata VALUES (?1,1,?2,'manual')",
                params![session, title],
            )
            .map_err(failure)?;
        }
        for id in files {
            tx.execute("INSERT INTO session_files VALUES (?1,NULL,NULL)", [id])
                .map_err(failure)?;
            tx.execute(
                "INSERT INTO session_file_owners VALUES (?1,?2)",
                params![session, id],
            )
            .map_err(failure)?;
        }
        Self::save_document(
            &tx,
            &format!("migration/session/{}", fingerprint(&data.id)?),
            &json!({"digest":digest,"session":session}),
        )?;
        tx.commit().map_err(failure)?;
        Ok(session)
    }
}

pub(crate) fn history(data: &SessionData) -> Result<Vec<StoredEvent>> {
    if let Some(error) = &data.context_error {
        return Err(Error::Deferred(format!(
            "Effective continuation cannot be represented: {error}; Session retained in source"
        )));
    }
    let now = dim_agent_runtime::core::unix_millis();
    let mut events = Vec::new();
    fn push(
        events: &mut Vec<StoredEvent>,
        kind: EventKind,
        id: Option<String>,
        now: u64,
    ) -> String {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let sequence = events.len() as u64 + 1;
        events.push(StoredEvent {
            event: Arc::new(AgentEvent {
                id: id.clone(),
                sequence,
                create_at: now,
                tags: Some(vec!["migration:legacy-import".into()]),
                kind,
            }),
            trace: None,
            batch_end: sequence,
        });
        id
    }
    push(&mut events,EventKind::Extension(ExtensionEvent::new("migration","imported",json!({"version":1,"source":"legacy-session","sourceSessionId":data.id,"meaning":"Imported history; historical execution is not resumed"})).map_err(Error::Object)?),None,now);
    let marker = format!(
        "Imported history from legacy Session {}. These messages and tool results were imported; no historical model request or tool was executed by this import.",
        data.id
    );
    for record in &data.records {
        let first = events.len();
        let timestamp = record
            .created_at
            .as_deref()
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| Error::Object("Legacy message timestamp is invalid".into()))?;
        let now = timestamp
            .map(|time| {
                u64::try_from(time.timestamp_millis()).map_err(|_| {
                    Error::Object("Legacy message timestamp is before the Unix epoch".into())
                })
            })
            .transpose()?
            .unwrap_or(now);
        let request = format!("import:{}", record.id);
        let parts = match &record.message.content {
            MessageContent::Parts(parts) => parts.clone(),
            MessageContent::Text(text) => vec![MessagePart::Text {
                provider_metadata: None,
                text: text.clone(),
                start_time: None,
                end_time: None,
            }],
        };
        match record.message.role {
            Role::User => {
                let mut content = Vec::new();
                for part in parts {
                    match part {
                        MessagePart::Text { text, .. } => content.push(ContentBlock::Text { text }),
                        MessagePart::Image { source } => {
                            content.push(ContentBlock::Image { source })
                        }
                        _ => {
                            return Err(Error::Deferred(
                                "Legacy user content has no equivalent native input representation"
                                    .into(),
                            ));
                        }
                    }
                }
                let id = push(
                    &mut events,
                    EventKind::InputMessage(InputMessageData {
                        user: Some(UserInput { content }),
                        mode: Some(InputMode::Append),
                        ..Default::default()
                    }),
                    None,
                    now,
                );
                push(
                    &mut events,
                    EventKind::InputApplied(InputAppliedData {
                        input_id: id.clone(),
                        mode: Some(InputMode::Append),
                    }),
                    None,
                    now,
                );
                push(
                    &mut events,
                    EventKind::InputConsumed(InputConsumedData { input_id: id }),
                    None,
                    now,
                );
            }
            Role::Assistant => {
                for part in &parts {
                    let kind = match part {
                        MessagePart::Text { text, .. } => Some(EventKind::Text(TextData {
                            turn_id: request.clone(),
                            request_id: request.clone(),
                            delta: text.clone(),
                        })),
                        MessagePart::Thinking { thinking, .. } => {
                            Some(EventKind::Thinking(ThinkingData {
                                turn_id: request.clone(),
                                request_id: request.clone(),
                                delta: thinking.clone(),
                            }))
                        }
                        MessagePart::ToolUse {
                            id, name, input, ..
                        } => {
                            push(
                                &mut events,
                                EventKind::ToolUseStart(ToolUseStartData {
                                    turn_id: request.clone(),
                                    request_id: request.clone(),
                                    call_id: id.clone(),
                                    name: name.clone(),
                                }),
                                None,
                                now,
                            );
                            Some(EventKind::ToolUse(ToolUseData {
                                turn_id: request.clone(),
                                request_id: request.clone(),
                                call: ToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                },
                            }))
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        push(&mut events, kind, None, now);
                    }
                }
                let finish_reason = if parts
                    .iter()
                    .any(|part| matches!(part, MessagePart::ToolUse { .. }))
                {
                    FinishReason::ToolUse
                } else {
                    FinishReason::EndTurn
                };
                push(
                    &mut events,
                    EventKind::ModelContent(ModelContentData {
                        turn_id: request.clone(),
                        request_id: request.clone(),
                        content: parts,
                    }),
                    None,
                    now,
                );
                push(
                    &mut events,
                    EventKind::ModelOutputEnd(ModelOutputEndData {
                        turn_id: request.clone(),
                        request_id: request.clone(),
                        outcome: ModelOutputOutcome::Completed { finish_reason },
                    }),
                    None,
                    now,
                );
            }
            Role::ToolResult => {
                let mut results = Vec::new();
                for part in parts {
                    if let MessagePart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        structured_content,
                    } = part
                    {
                        results.push(ToolCallResult {
                            call_id: tool_use_id,
                            result: ToolResult {
                                content,
                                is_error,
                                structured_content,
                                blob_refs: None,
                                background_task: None,
                                context_messages: None,
                                effects: None,
                            },
                        });
                    } else {
                        return Err(Error::Deferred(
                            "Legacy tool result has incompatible parts".into(),
                        ));
                    }
                }
                push(
                    &mut events,
                    EventKind::ToolResult(ToolResultData {
                        turn_id: request.clone(),
                        request_id: request,
                        call_event_id: None,
                        results,
                    }),
                    None,
                    now,
                );
            }
        }
        let batch_end = events.len() as u64;
        for record_event in &mut events[first..] {
            record_event.batch_end = batch_end;
            let tags = Arc::get_mut(&mut record_event.event)
                .unwrap()
                .tags
                .as_mut()
                .unwrap();
            tags.push(format!("legacy-message:{}", record.id));
            if !record.in_context {
                tags.push("migration:context-excluded".into());
            }
        }
    }
    let through = events.len() as u64;
    let command = through + 1;
    push(
        &mut events,
        EventKind::ContextCompact(ContextCompactData {}),
        None,
        now,
    );
    push(
        &mut events,
        EventKind::ContextCompactionStart(ContextCompactionStartData {
            turn_id: None,
            request_id: None,
            command: Some(command),
            through,
            trigger: CompactionTrigger::Manual,
            strategy: "migration".into(),
        }),
        None,
        now,
    );
    let cut = uuid::Uuid::new_v4().to_string();
    let mut replacement = vec![Message::text(Role::User, marker)];
    replacement.extend(
        data.records
            .iter()
            .filter(|record| record.in_context)
            .map(|record| record.message.clone()),
    );
    push(&mut events, EventKind::ContextCompaction(ContextCompactionData{turn_id:None,request_id:None,command:Some(command),start:Some(command+1),through,outcome:ContextCompactionResult::Completed{retain_from:cut.clone(),replacement,strategy:"migration".into(),context_baseline:Vec::new(),replacement_parts:None,pinned_inputs:None,digest_entries:None,diagnostics:Box::new(CompactionDiagnosticsRecord{reason:CompactionReason::Manual,trigger:CompactionTrigger::Manual,retain_from:Some(cut.clone()),retain_limit:None,estimated_input_tokens:0.0,estimated_output_tokens:None,entries_replaced:data.records.len() as u64,model_call_count:0,duration_ms:0,model_usage:None,insufficient_space:None,strategy:CompactionStrategyRecord{kind:"migration".into(),details:serde_json::from_value(json!({"source":"legacy-effective-context","tokenEstimateUnavailable":true})).map_err(failure)?}})}}),Some(cut),now);
    Ok(events)
}

fn validate_settings(source: &str, value: &Value) -> Result<()> {
    let invalid = || {
        Error::Object(
            "Converted settings have no valid native representation; target preserved".into(),
        )
    };
    let fields = value.as_object().ok_or_else(invalid)?;
    match source {
        "mcp" => {
            use dim_app_server::settings::BusinessDocument;
            let document: dim_app_server::mcp::config::McpSettings =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            document.validate().map_err(|_| invalid())?;
        }
        "collaboration" => {
            let valid = match value["mode"].as_str() {
                Some("agent" | "inherit") => fields.len() == 1,
                Some("fixed") => {
                    fields.len() == 2
                        && value["model"].as_object().is_some_and(|model| {
                            model.len() == 2
                                && ["providerId", "modelId"].iter().all(|key| {
                                    model
                                        .get(*key)
                                        .and_then(Value::as_str)
                                        .is_some_and(|value| !value.is_empty())
                                })
                        })
                }
                _ => false,
            };
            if !valid {
                return Err(invalid());
            }
        }
        "permission" => {
            if fields.len() != 1 {
                return Err(invalid());
            }
            let rules: Vec<CommandRule> =
                serde_json::from_value(value["commandRules"].clone()).map_err(|_| invalid())?;
            if rules.iter().any(|rule| {
                rule.command.split_whitespace().next().is_none()
                    || rule.command.contains(COMMAND_SEPARATORS)
            }) {
                return Err(invalid());
            }
        }
        "skills" => {
            if !value["disabledSkillIds"].as_array().is_some_and(|ids| {
                ids.iter()
                    .all(|id| id.as_str().is_some_and(|id| !id.trim().is_empty()))
            }) {
                return Err(invalid());
            }
            if let Some(roots) = value.get("pluginSkills")
                && !roots.as_object().is_some_and(|roots| {
                    roots.iter().all(|(id, root)| {
                        !id.trim().is_empty()
                            && root
                                .as_str()
                                .is_some_and(|path| Path::new(path).is_absolute())
                    })
                })
            {
                return Err(invalid());
            }
        }
        "web" => {
            let service = &value["searchService"];
            if service["kind"] != "brave"
                || !service["apiKey"]
                    .as_str()
                    .is_some_and(|key| !key.is_empty())
                || service.get("endpoint").is_some_and(|url| {
                    !url.as_str().is_some_and(|raw| {
                        reqwest::Url::parse(raw).is_ok_and(|url| {
                            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                        })
                    })
                })
            {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
