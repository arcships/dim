mod permissions;
mod skills;
mod web;

use crate::native::Native;
use crate::{Error, Migration, ObjectResult, Outcome, Result, TaskState, source::Source};
use dim_agent_capabilities::file_storage::FileStorageError;
use serde_json::{Map, Value, json};

async fn record(
    owner: &Migration,
    task: &TaskState,
    id: String,
    outcome: Outcome,
    reason: Option<String>,
) -> Result<()> {
    owner
        .ledger
        .object(
            &task.id,
            &ObjectResult {
                kind: "setting".into(),
                id,
                outcome,
                target_id: None,
                reason,
                omitted: Vec::new(),
            },
        )
        .await
}

async fn fill(
    owner: &Migration,
    task: &TaskState,
    client: &Native,
    source: &str,
    fields: Value,
) -> Result<()> {
    let result = client.fill(source, fields, &owner.stopping).await;
    match result {
        Ok(results) => {
            for (field, status) in results {
                let outcome = match status.as_str() {
                    "imported" => Outcome::Imported,
                    "unchanged" => Outcome::AlreadyImported,
                    "conflict" => Outcome::Conflict,
                    "rejected" => Outcome::Unsupported,
                    _ => return Err(Error::Target("Unknown Settings field outcome".into())),
                };
                record(owner,task,format!("{source}.{field}"),outcome,if matches!(outcome,Outcome::Conflict|Outcome::Unsupported){Some("Target configuration is preserved; incompatible source data remains in the legacy home".into())}else{None}).await?;
            }
        }
        Err(Error::Object(reason)) => {
            record(
                owner,
                task,
                source.into(),
                Outcome::Unsupported,
                Some(reason),
            )
            .await?
        }
        Err(Error::Unsupported(reason)) => {
            record(
                owner,
                task,
                source.into(),
                Outcome::Unsupported,
                Some(reason),
            )
            .await?
        }
        Err(Error::Deferred(reason)) => {
            record(owner, task, source.into(), Outcome::Deferred, Some(reason)).await?
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

async fn file_fill(
    owner: &Migration,
    task: &TaskState,
    target: &Native,
    name: &str,
    fields: Map<String, Value>,
) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let key = format!("migration/file/{name}");
    let coverage = target.document(&key)?;
    let mut configured: std::collections::BTreeSet<String> = coverage
        .as_ref()
        .and_then(|value| value.get("fields"))
        .map(|value| serde_json::from_value(value.clone()).map_err(crate::native::failure))
        .transpose()?
        .unwrap_or_default();
    let path = owner.ledger.files.path(0, name)?;
    let mut lease = owner.ledger.files.lock(&path, &owner.stopping).await?;
    let mut current: Value = match lease.read(&owner.stopping).await {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(document) => document,
            Err(_) => {
                return record(
                    owner,
                    task,
                    name.into(),
                    Outcome::Conflict,
                    Some("Invalid target configuration is preserved".into()),
                )
                .await;
            }
        },
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({})
        }
        Err(error) => return Err(error.into()),
    };
    let Some(members) = current.as_object_mut() else {
        return record(
            owner,
            task,
            name.into(),
            Outcome::Conflict,
            Some("Non-object target configuration is preserved".into()),
        )
        .await;
    };
    let existed = !members.is_empty();
    configured.extend(members.keys().cloned());
    let mut changed = false;
    let mut results = Vec::new();
    for (field, value) in fields {
        let outcome = match members.get(&field) {
            Some(existing) if existing == &value => Outcome::AlreadyImported,
            Some(_) => Outcome::Conflict,
            None if configured.contains(&field) => {
                if coverage
                    .as_ref()
                    .is_some_and(|value| value["complete"] == true)
                {
                    Outcome::Deleted
                } else {
                    Outcome::Conflict
                }
            }
            None if name == "exec-shell.json" && existed => Outcome::Conflict,
            None => {
                members.insert(field.clone(), value);
                configured.insert(field.clone());
                changed = true;
                Outcome::Imported
            }
        };
        results.push((field, outcome));
    }
    if changed {
        let bytes = serde_json::to_vec_pretty(&current)
            .map_err(|_| Error::Target("Cannot encode client settings".into()))?;
        #[cfg(unix)]
        let permissions = Some(std::os::unix::fs::PermissionsExt::from_mode(0o600));
        #[cfg(not(unix))]
        let permissions = None;
        crate::engine::check(owner, &task.id).await?;
        Native::save_document(
            &target.db.lock().unwrap(),
            &key,
            &json!({"fields":configured,"complete":false}),
        )?;
        lease
            .replace(
                bytes,
                permissions,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await?;
    }
    if changed
        || coverage
            .as_ref()
            .is_none_or(|value| value["complete"] == true)
    {
        Native::save_document(
            &target.db.lock().unwrap(),
            &key,
            &json!({"fields":configured,"complete":true}),
        )?;
    }
    for (field, outcome) in results {
        record(owner, task, format!("{name}.{field}"), outcome, None).await?;
    }
    Ok(())
}

pub(crate) async fn import(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    client: &Native,
) -> Result<()> {
    let cli = crate::engine::document(owner, task, source, "config.json")
        .await?
        .unwrap_or_else(|| json!({}));
    let desktop = crate::engine::document(owner, task, source, "desktop-config.json").await?;
    let mut cli_fields = match cli {
        Value::Object(fields) => fields,
        _ => {
            record(
                owner,
                task,
                "config.json".into(),
                Outcome::Failed,
                Some("Legacy CLI settings must be an object".into()),
            )
            .await?;
            Map::new()
        }
    };
    if let Some(desktop) = desktop {
        let settings = desktop.get("settings").and_then(Value::as_object);
        if let Some(settings) = settings.filter(|_| desktop.get("version") == Some(&json!(7))) {
            for (key, value) in settings {
                if [
                    "locale",
                    "subagentModel",
                    "assistantModel",
                    "subagentMaxConcurrency",
                    "customInstructions",
                    "defaultToolApprovalMode",
                    "sandboxMode",
                    "sandboxNetworkMode",
                    "sandboxAllowedDomains",
                ]
                .contains(&key.as_str())
                {
                    cli_fields.insert(key.clone(), value.clone());
                } else {
                    record(
                        owner,
                        task,
                        format!("desktop.{key}"),
                        Outcome::Deferred,
                        Some("Desktop-specific configuration remains in the legacy home".into()),
                    )
                    .await?;
                }
            }
        } else {
            record(
                owner,
                task,
                "desktop-config.json".into(),
                Outcome::Unsupported,
                Some("Legacy Desktop configuration does not match the current schema".into()),
            )
            .await?;
        }
    }
    let mut preferences = Map::new();
    for (key, value) in cli_fields.iter() {
        let compatible = match key.as_str() {
            "locale" => {
                value.is_null()
                    || value.as_str().is_some_and(|value| {
                        [
                            "en", "zh-CN", "zh-Hant", "ja", "ko", "es", "fr", "de", "pt-BR",
                            "pt-PT", "id",
                        ]
                        .contains(&value)
                    })
            }
            "themeId" => value
                .as_str()
                .is_some_and(|value| owner.themes.iter().any(|theme| theme == value)),
            "tuiMode" => value
                .as_str()
                .is_some_and(|value| ["fullscreen", "scrollback"].contains(&value)),
            "tuiMouseMode" => value
                .as_str()
                .is_some_and(|value| ["on", "off"].contains(&value)),
            "tuiNotifyOnComplete" | "tuiToolDetailsExpanded" | "showAcpSessions" => {
                value.is_boolean()
            }
            _ => false,
        };
        if compatible {
            preferences.insert(key.clone(), value.clone());
        } else if key != "subagentModel" {
            record(
                owner,
                task,
                format!("config.{key}"),
                Outcome::Unsupported,
                Some("No equivalent target field mapping; source preserved".into()),
            )
            .await?;
        }
    }
    file_fill(owner, task, client, "config.json", preferences).await?;
    if let Some(model) = cli_fields.get("subagentModel") {
        let policy = if model.is_null() {
            Some(json!({"mode":"inherit"}))
        } else if model.get("mode").and_then(Value::as_str) == Some("agent-decides") {
            None
        } else if let (Some(provider), Some(model)) = (
            model.get("providerId").and_then(Value::as_str),
            model.get("modelId").and_then(Value::as_str),
        ) {
            Some(json!({"mode":"fixed","model":{"providerId":provider,"modelId":model}}))
        } else {
            None
        };
        match policy {
            Some(policy) => {
                fill(owner, task, client, "collaboration", policy).await?;
                if let Some(fields) = model.as_object() {
                    for key in fields
                        .keys()
                        .filter(|key| !matches!(key.as_str(), "providerId" | "modelId"))
                    {
                        record(
                            owner,
                            task,
                            format!("config.subagentModel.{key}"),
                            Outcome::Unsupported,
                            Some("No equivalent Collaboration model policy field".into()),
                        )
                        .await?;
                    }
                }
            }
            None => {
                record(
                    owner,
                    task,
                    "config.subagentModel".into(),
                    Outcome::Unsupported,
                    Some("Legacy model-decides policy has no equivalent target policy".into()),
                )
                .await?
            }
        }
    }
    if let Some(skills) = crate::engine::document(owner, task, source, "skills.json").await? {
        skills::import(owner, task, client, skills).await?;
    }
    web::import(owner, task, source, client).await?;
    if let Some(mcp) = crate::engine::document(owner, task, source, "mcp.json").await?
        && let Some(servers) = mcp.get("mcpServers")
    {
        match servers.as_object() {
            Some(entries) => {
                let mut valid = Map::new();
                for (id, value) in entries {
                    match dim_app_server::mcp::config::decode_entry(id, value) {
                        Ok(_) => {
                            valid.insert(id.clone(), value.clone());
                        }
                        // The legacy home keeps this entry; the native store
                        // receives only entries that decode.
                        Err(error) => {
                            record(
                                owner,
                                task,
                                format!("mcpServers.{id}"),
                                Outcome::Unsupported,
                                Some(error.to_string()),
                            )
                            .await?
                        }
                    }
                }
                if !valid.is_empty() {
                    fill(owner, task, client, "mcp", json!({"mcpServers": valid})).await?;
                }
            }
            None => fill(owner, task, client, "mcp", json!({"mcpServers": servers})).await?,
        }
    }
    if let Some(shell) = crate::engine::document(owner, task, source, "exec-shell.json").await? {
        let valid = shell.get("version") == Some(&json!(1))
            && shell.as_object().is_some_and(|fields| {
                fields
                    .keys()
                    .all(|key| ["version", "executablePath"].contains(&key.as_str()))
            })
            && shell.get("executablePath").is_none_or(|path| {
                path.as_str()
                    .is_some_and(|path| std::path::Path::new(path).is_absolute())
            });
        if valid {
            file_fill(
                owner,
                task,
                client,
                "exec-shell.json",
                shell.as_object().unwrap().clone(),
            )
            .await?;
        } else {
            record(
                owner,
                task,
                "exec-shell".into(),
                Outcome::Unsupported,
                Some("Legacy shell selection is incompatible".into()),
            )
            .await?;
        }
    }
    if let Some(permission) =
        crate::engine::document(owner, task, source, "permissions.json").await?
    {
        permissions::import(owner, task, client, permission).await?;
    }
    Ok(())
}
