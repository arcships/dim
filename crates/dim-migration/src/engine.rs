use crate::{
    Error, Migration, ObjectResult, Outcome, Result, TaskReport, TaskState, TaskStatus,
    native::Native,
    source::{RecordedMessage, SessionData, Source, fingerprint},
};
use base64::Engine;
use dim_agent_types::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) async fn publish(owner: &Migration, task: &TaskState) -> Result<()> {
    owner.ledger.save(task).await?;
    owner.progress.send_modify(|progress| match progress {
        Some(progress) if progress.task.id == task.id => progress.task = task.clone(),
        progress => {
            *progress = Some(crate::Progress {
                task: task.clone(),
                recent: Default::default(),
            })
        }
    });
    Ok(())
}

pub(crate) async fn check(owner: &Migration, id: &str) -> Result<()> {
    if owner.stopping.is_cancelled() || owner.ledger.cancelled(id).await? {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) async fn document(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    name: &str,
) -> Result<Option<Value>> {
    match source.json(name, &owner.stopping).await {
        Err(Error::Object(reason)) => {
            owner
                .ledger
                .object(
                    &task.id,
                    &ObjectResult {
                        kind: "file".into(),
                        id: name.into(),
                        outcome: Outcome::Failed,
                        target_id: None,
                        reason: Some(reason),
                        omitted: Vec::new(),
                    },
                )
                .await?;
            Ok(None)
        }
        result => result,
    }
}

pub(crate) async fn run(
    owner: Arc<Migration>,
    source_home: PathBuf,
    retry: Option<String>,
) -> Result<TaskReport> {
    let _previous = if let Some(id) = &retry {
        let lease = owner.ledger.lock(id).await?;
        let mut previous = owner.ledger.report(id).await?.task;
        if previous.status == TaskStatus::Running {
            previous.status = TaskStatus::Interrupted;
            previous.error = Some(
                "Previous process ended before final settlement; retry reconciles owner receipts"
                    .into(),
            );
            owner.ledger.save(&previous).await?;
        }
        Some(lease)
    } else {
        None
    };
    let id = uuid::Uuid::new_v4().to_string();
    owner.ledger.create(&id).await?;
    let _lease = owner.ledger.lock(&id).await?;
    let mut task = TaskState {
        format: 1,
        id: id.clone(),
        retry_of: retry,
        source_home,
        status: TaskStatus::Running,
        stage: "scan".into(),
        processed: 0,
        total: None,
        current: None,
        error: None,
    };
    publish(&owner, &task).await?;
    let outcome = execute(&owner, &mut task).await;
    match outcome {
        Ok(()) => {
            let report = owner.ledger.report(&id).await?;
            task.processed = report.objects.len() as u64;
            task.total = Some(task.processed);
            task.status = if report.objects.iter().any(|item| {
                matches!(
                    item.outcome,
                    Outcome::Conflict
                        | Outcome::Deferred
                        | Outcome::Unsupported
                        | Outcome::Failed
                        | Outcome::Cancelled
                ) || !item.omitted.is_empty()
            }) {
                TaskStatus::Partial
            } else {
                TaskStatus::Completed
            };
            task.stage = "complete".into();
        }
        Err(Error::Cancelled) => {
            task.status = TaskStatus::Cancelled;
            task.error = Some(
                "Stopped after accepted work settled; completed objects remain imported".into(),
            );
        }
        Err(error) => {
            task.status = TaskStatus::Failed;
            task.error = Some(error.to_string());
        }
    }
    task.current = None;
    publish(&owner, &task).await?;
    owner.ledger.report(&id).await
}

async fn execute(owner: &Migration, task: &mut TaskState) -> Result<()> {
    let target = Native::open(owner.ledger.files.clone(), &owner.native_directory).await?;
    let source = Source::open(
        owner.ledger.files.clone(),
        task.source_home.clone(),
        0,
        &owner.stopping,
    )
    .await?;
    let result = execute_source(owner, task, &source, &target).await;
    let cleanup = source.close(&CancellationToken::new()).await;
    result.and(cleanup)
}

async fn execute_source(
    owner: &Migration,
    task: &mut TaskState,
    source: &Source,
    target: &Native,
) -> Result<()> {
    for stage in ["providers", "settings", "accounts"] {
        check(owner, &task.id).await?;
        task.stage = stage.into();
        task.processed = 0;
        task.total = None;
        task.current = None;
        publish(owner, task).await?;
        let result = match stage {
            "providers" => crate::providers::import(owner, task, source, target).await,
            "settings" => crate::settings::import(owner, task, source, target).await,
            _ => crate::accounts::import(owner, task, source, target).await,
        };
        match result {
            Err(Error::Object(reason)) | Err(Error::Unsupported(reason)) => {
                owner
                    .ledger
                    .object(
                        &task.id,
                        &ObjectResult {
                            kind: stage.into(),
                            id: stage.into(),
                            outcome: Outcome::Failed,
                            target_id: None,
                            reason: Some(reason),
                            omitted: Vec::new(),
                        },
                    )
                    .await?
            }
            result => result?,
        }
    }
    task.stage = "sessions".into();
    let ids = source.session_ids()?;
    task.total = Some(ids.len() as u64);
    task.processed = 0;
    let mut imported = BTreeMap::new();
    for id in ids {
        check(owner, &task.id).await?;
        task.current = Some(id.clone());
        publish(owner, task).await?;
        let (result, parent) = match source.session(&id) {
            Ok(mut data) => {
                let parent = data.parent_session_id.take();
                (session(owner, task, source, target, data).await, parent)
            }
            Err(e) => (Err(e), None),
        };
        let result = match result {
            Ok(result) => result,
            Err(Error::Object(reason)) => ObjectResult {
                kind: "session".into(),
                id,
                outcome: Outcome::Failed,
                target_id: None,
                reason: Some(reason),
                omitted: Vec::new(),
            },
            Err(Error::Deferred(reason)) => ObjectResult {
                kind: "session".into(),
                id,
                outcome: Outcome::Deferred,
                target_id: None,
                reason: Some(reason),
                omitted: Vec::new(),
            },
            Err(e) => return Err(e),
        };
        if matches!(result.outcome, Outcome::Imported | Outcome::AlreadyImported)
            && let Some(target_id) = &result.target_id
        {
            imported.insert(result.id.clone(), (target_id.clone(), parent));
        }
        owner.ledger.object(&task.id, &result).await?;
        task.processed += 1;
        publish(owner, task).await?;
    }
    check(owner, &task.id).await?;
    target.link_session_parents(imported.values().filter_map(|(child_id, parent)| {
        let (parent, _) = imported.get(parent.as_ref()?)?;
        Some((child_id.as_str(), parent.as_str()))
    }))?;
    task.stage = "scheduler".into();
    task.processed = 0;
    task.total = None;
    task.current = None;
    publish(owner, task).await?;
    crate::scheduler::import(owner, task, source, target).await?;
    task.stage = "inventory".into();
    task.processed = 0;
    task.total = None;
    task.current = None;
    publish(owner, task).await?;
    crate::inventory::inspect(owner, task, source).await?;
    Ok(())
}

async fn session(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    target: &Native,
    mut data: SessionData,
) -> Result<ObjectResult> {
    if let Some(error) = &data.context_error {
        return Err(Error::Deferred(format!(
            "Effective continuation cannot be represented: {error}; source preserved"
        )));
    }
    let mut archive = crate::archives::Archive::read(source, &data, &owner.stopping).await?;
    let historical: Vec<_> = archive
        .records
        .iter()
        .map(|record| (record.table.clone(), record.id.clone()))
        .collect();
    let mut raw_files = std::mem::take(&mut archive.files);
    for blob in &data.blobs {
        if raw_files
            .insert(blob.id.clone(), source.blob(blob, &owner.stopping).await?)
            .is_some()
        {
            return Err(Error::Object("Attachment identity collision".into()));
        }
    }
    let digests: BTreeMap<_, _> = raw_files
        .iter()
        .map(|(id, bytes)| (id, format!("{:x}", Sha256::digest(bytes))))
        .collect();
    let digest = fingerprint(&(&data.source_digest, &archive, &digests))?;
    let result = |outcome, target_id, omitted| ObjectResult {
        kind: "session".into(),
        id: data.id.clone(),
        outcome,
        target_id: Some(target_id),
        reason: None,
        omitted,
    };
    if let Some((outcome, session)) = target.session_receipt(&data.id, &digest)? {
        if outcome == Outcome::AlreadyImported {
            record_history(owner, task, &historical, &session, outcome).await?;
        }
        return Ok(result(outcome, session, archive.omitted));
    }
    let mut omitted = archive.omitted.clone();
    if data.tags.as_array().is_some_and(|tags| !tags.is_empty()) {
        omitted.push("tags".into());
    }
    if data
        .permission
        .as_object()
        .is_some_and(|fields| !fields.is_empty())
    {
        omitted.push("session permission fields without an equivalent mapping".into());
    }
    if let Some(settings) = data.settings.as_object() {
        omitted.extend(
            settings
                .keys()
                .filter(|k| k.as_str() != "__revertState__")
                .map(|k| format!("session.settings.{k}")),
        );
    }
    if data
        .mode
        .as_deref()
        .is_some_and(|mode| mode != "agent" && mode != "plan")
    {
        omitted.push("legacy Session mode".into());
    }
    let mut files = Vec::new();
    let result=async {
    let mut paths=BTreeMap::new();
    for (id,bytes) in &raw_files {
        check(owner,&task.id).await?;
        let (file,path)=target.put_file("data",bytes.clone(),&owner.stopping).await?;
        paths.insert(id.clone(),path);files.push(file);
    }
    let metadata=archive.encode(&paths)?;
    let (file,metadata_path)=target.put_file("legacy-history.json",metadata,&owner.stopping).await?;
    files.push(file);
    let mut images = Vec::new();
    for (record_index, record) in data.records.iter_mut().enumerate() {
        let MessageContent::Parts(parts) = &mut record.message.content else {
            continue;
        };
        if let Some(attachments) = data.attachments.get(&record.id) {
            for attachment in attachments {
                if attachment.kind != "blob" {
                    omitted.push(format!("attachment.kind.{}", attachment.kind));
                    continue;
                }
                let id = attachment.data["blobId"]
                    .as_str()
                    .ok_or_else(|| Error::Object("Attachment has no blob identity".into()))?;
                let path = paths
                    .get(id)
                    .ok_or_else(|| Error::Object("Attachment references a missing blob".into()))?;
                if attachment.data["role"].as_str() != Some("tool_output") {
                    let blob = data
                        .blobs
                        .iter()
                        .find(|blob| blob.id == id)
                        .ok_or_else(|| Error::Object("Missing blob metadata".into()))?;
                    let media = attachment.data["mediaType"]
                        .as_str()
                        .or(blob.mime_type.as_deref());
                    if let Some(media) = media.filter(|media| media.starts_with("image/")) {
                        images.push((record_index, parts.len(), id.to_owned(), media.to_owned()));
                    }
                    parts.push(MessagePart::Text {
                        provider_metadata: None,
                        text: format!("Attached imported blob {id}: {path}"),
                        start_time: None,
                        end_time: None,
                    });
                }
            }
        }
        for part in parts {
            match part {
                MessagePart::Text { text, .. } | MessagePart::Thinking { thinking: text, .. } => {
                    remap(text, &data.blobs, &paths, source)
                }
                MessagePart::ToolResult { content, .. } => match content {
                    ToolContent::Text(text) => remap(text, &data.blobs, &paths, source),
                    ToolContent::Blocks(blocks) => {
                        for block in blocks {
                            if let ContentBlock::Text { text } = block {
                                remap(text, &data.blobs, &paths, source);
                            }
                        }
                    }
                },
                _ => {}
            }
        }
    }
    let mut note_id="migration-metadata".to_owned();
    while data.records.iter().any(|record|record.id==note_id){note_id.push('-');}
    data.records.push(RecordedMessage{id:note_id,created_at:None,message:Message::text(Role::User,format!("Imported legacy historical metadata and available artifacts: {}. Execution is not resumed.",metadata_path)),in_context:false});
    if !images.is_empty() {
        let mut message_budgets = BTreeMap::new();
        let mut context_budget = None;
        for stored in crate::native::history(&data)? {
            let bytes = serde_json::to_vec(&stored.event).map_err(crate::native::failure)?.len();
            let remaining = crate::native::MAX_HISTORY_RECORD_BYTES.saturating_sub(bytes);
            if matches!(stored.event.kind, EventKind::ContextCompaction(_)) {
                context_budget = Some(remaining);
            }
            for tag in stored.event.tags.as_ref().unwrap() {
                if let Some(id) = tag.strip_prefix("legacy-message:") {
                    message_budgets.entry(id.to_owned())
                        .and_modify(|budget: &mut usize| *budget = (*budget).min(remaining))
                        .or_insert(remaining);
                }
            }
        }
        let mut context_budget = context_budget.unwrap();
        for (record_index, part_index, id, media_type) in images {
            let record = &mut data.records[record_index];
            let MessageContent::Parts(parts) = &mut record.message.content else { unreachable!() };
            let image = MessagePart::Image { source: ImageSource {
                source_type: ImageSourceType::Base64,
                media_type,
                data: Some(base64::engine::general_purpose::STANDARD.encode(&raw_files[&id])),
                url: None,
            }};
            let growth = serde_json::to_vec(&image).map_err(crate::native::failure)?.len()
                .saturating_sub(serde_json::to_vec(&parts[part_index]).map_err(crate::native::failure)?.len());
            let budget = message_budgets.get_mut(&record.id).unwrap();
            if growth <= *budget && (!record.in_context || growth <= context_budget) {
                parts[part_index] = image;
                *budget -= growth;
                if record.in_context { context_budget -= growth; }
            } else {
                omitted.push(format!("attachment.inlineImage.{id}"));
            }
        }
    }
    check(owner, &task.id).await?;
    let session = target.session(&data, &digest, &files)?;
    record_history(owner, task, &historical, &session, Outcome::Imported).await?;
    Ok(ObjectResult {
        kind: "session".into(),
        id: data.id,
        outcome: Outcome::Imported,
        target_id: Some(session),
        reason: None,
        omitted,
    })
    }.await;
    if result.is_err() {
        target.discard_files(&files).await?;
    }
    result
}

async fn record_history(
    owner: &Migration,
    task: &TaskState,
    records: &[(String, String)],
    session: &str,
    outcome: Outcome,
) -> Result<()> {
    for (table, id) in records {
        owner.ledger.object(&task.id, &ObjectResult { kind: "historical_record".into(), id: format!("{table}:{id}"), outcome, target_id: Some(session.into()), reason: Some("Preserved as readable historical data; execution and authority are not restored".into()), omitted: Vec::new() }).await?;
    }
    Ok(())
}

fn remap(
    text: &mut String,
    blobs: &[crate::source::Blob],
    paths: &BTreeMap<String, String>,
    source: &Source,
) {
    for blob in blobs {
        if let Some(target) = paths.get(&blob.id) {
            let path = if std::path::Path::new(&blob.reference).is_absolute() {
                PathBuf::from(&blob.reference)
            } else {
                source.home.join("data").join(&blob.reference)
            };
            if let Some(path) = path.to_str() {
                *text = text.replace(path, target);
            }
        }
    }
}
