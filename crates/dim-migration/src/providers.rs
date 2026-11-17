use crate::{
    Error, Migration, ObjectResult, Outcome, Result as MigrationResult, TaskState, native::Native,
    source::Source,
};
use dim_app_server::{
    model_catalog::BundledCatalog,
    provider::{
        LEGACY_METADATA_FIELDS, LEGACY_PROVIDER_FIELDS, LegacyRow, StaticProviderRecord,
        convert_legacy_provider, convert_legacy_selection, legacy_omitted_fields,
        read_legacy_source, read_stored_static, write_stored_static,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

fn covered_fields(source: &LegacyRow) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    for name in LEGACY_PROVIDER_FIELDS {
        fields.insert((*name).to_owned(), digest(&source.fields[*name])?);
    }
    let metadata: Option<Map<String, Value>> = source.json_field("metadata")?;
    for name in LEGACY_METADATA_FIELDS {
        fields.insert(
            format!("metadata.{name}"),
            digest(&metadata.as_ref().and_then(|value| value.get(*name)))?,
        );
    }
    Ok(fields)
}

fn digest(value: &impl serde::Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| "Cannot fingerprint Provider field")?)
    ))
}

/// The target record for `provider_id`, parsed and validated by Provider.
fn read_target(
    connection: &Connection,
    provider_id: &str,
) -> MigrationResult<Option<StaticProviderRecord>> {
    read_stored_static(connection, provider_id)
        .map_err(|_| Error::Object("Unreadable native provider is preserved".into()))
}

/// Keep a provider record a previous run imported for the selections that
/// name it; an unreadable record keeps its read error for that row.
fn remember_target(
    connection: &Connection,
    provider_id: &str,
    imported: &mut HashMap<String, StaticProviderRecord>,
    unreadable: &mut HashMap<String, String>,
) -> MigrationResult<()> {
    match read_target(connection, provider_id) {
        Ok(Some(record)) => {
            imported.insert(provider_id.to_owned(), record);
        }
        Ok(None) => {}
        Err(Error::Object(reason)) => {
            unreadable.insert(provider_id.to_owned(), reason);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

pub(crate) async fn import(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    target: &Native,
) -> MigrationResult<()> {
    if !source.inventory.tables.contains_key("providers") {
        return Ok(());
    }
    let snapshot = read_legacy_source(source.snapshot.clone(), &owner.stopping)
        .map_err(|_| Error::Object("Legacy provider schema is unsupported".into()))?;
    let bundled = BundledCatalog::load().map_err(crate::native::failure)?;
    let mut imported: HashMap<String, StaticProviderRecord> = HashMap::new();
    let mut unreadable: HashMap<String, String> = HashMap::new();
    for (kind, rows) in [
        ("provider", snapshot.providers),
        ("selection", snapshot.selections),
    ] {
        for row in rows {
            crate::engine::check(owner, &task.id).await?;
            let mut converted_omitted = None;
            let result = (|| -> MigrationResult<Outcome> {
                let mut db = target.db.lock().unwrap();
                let tx = db.transaction().map_err(crate::native::failure)?;
                let seen: Option<String> = tx
                    .query_row(
                        "SELECT source_digest FROM provider_imports WHERE kind=?1 AND id=?2",
                        params![kind, row.id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(crate::native::failure)?;
                let coverage_key = format!(
                    "migration/provider/{}",
                    crate::source::fingerprint(&row.id)?
                );
                if let Some(seen) = seen {
                    if kind == "provider" {
                        let current: Option<String> = tx
                            .query_row(
                                "SELECT record FROM providers WHERE provider_id=?1",
                                [&row.id],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(crate::native::failure)?;
                        if current.is_none() {
                            return Ok(Outcome::Deleted);
                        }
                        let coverage: Option<String> = tx
                            .query_row(
                                "SELECT document FROM settings_documents WHERE source=?1",
                                [&coverage_key],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(crate::native::failure)?;
                        if let Some(coverage) = coverage {
                            let coverage: Value =
                                serde_json::from_str(&coverage).map_err(crate::native::failure)?;
                            let fields = covered_fields(&row).map_err(Error::Object)?;
                            let previous: BTreeMap<String, String> =
                                serde_json::from_value(coverage["fields"].clone())
                                    .map_err(crate::native::failure)?;
                            if previous
                                .iter()
                                .any(|(field, digest)| fields.get(field) != Some(digest))
                            {
                                return Ok(Outcome::Conflict);
                            }
                            if fields.len() > previous.len() {
                                let current = read_target(&tx, &row.id)?.ok_or_else(|| {
                                    Error::Object("Unreadable native provider is preserved".into())
                                })?;
                                if serde_json::to_value(&current).map_err(crate::native::failure)?
                                    != coverage["target"]
                                {
                                    return Ok(Outcome::Conflict);
                                }
                                let version =
                                    current.version().checked_add(1).ok_or_else(|| {
                                        Error::Target("Provider version exhausted".into())
                                    })?;
                                let previous =
                                    current.is_builtin().then(|| current.with_version(1));
                                let conversion = convert_legacy_provider(&row, previous, &bundled)
                                    .map_err(Error::Object)?;
                                converted_omitted = Some(conversion.omitted);
                                let converted = conversion.record.with_version(version);
                                let converted_value = serde_json::to_value(&converted)
                                    .map_err(crate::native::failure)?;
                                write_stored_static(&tx, &row.id, &converted)
                                    .map_err(crate::native::failure)?;
                                Native::save_document(
                                    &tx,
                                    &coverage_key,
                                    &json!({"fields":fields,"target":converted_value}),
                                )?;
                                tx.commit().map_err(crate::native::failure)?;
                                imported.insert(row.id.clone(), converted);
                                return Ok(Outcome::Imported);
                            }
                            remember_target(&tx, &row.id, &mut imported, &mut unreadable)?;
                            return Ok(Outcome::AlreadyImported);
                        }
                    }
                    if seen == row.digest {
                        if kind == "provider" {
                            remember_target(&tx, &row.id, &mut imported, &mut unreadable)?;
                        }
                        return Ok(Outcome::AlreadyImported);
                    }
                    return Ok(Outcome::Conflict);
                }
                if kind == "provider" {
                    let previous = read_target(&tx, &row.id)?;
                    if previous
                        .as_ref()
                        .is_some_and(|record| !record.is_pristine_builtin())
                    {
                        return Ok(Outcome::Conflict);
                    }
                    let conversion =
                        convert_legacy_provider(&row, previous, &bundled).map_err(Error::Object)?;
                    converted_omitted = Some(conversion.omitted);
                    let converted =
                        serde_json::to_value(&conversion.record).map_err(crate::native::failure)?;
                    write_stored_static(&tx, &row.id, &conversion.record)
                        .map_err(crate::native::failure)?;
                    Native::save_document(
                        &tx,
                        &coverage_key,
                        &json!({"fields":covered_fields(&row).map_err(Error::Object)?,"target":converted}),
                    )?;
                    imported.insert(row.id.clone(), conversion.record);
                } else {
                    let blocked = row
                        .fields
                        .get("providerId")
                        .and_then(Value::as_str)
                        .and_then(|id| unreadable.get(id));
                    if let Some(reason) = blocked {
                        return Err(Error::Object(reason.clone()));
                    }
                    let selection = convert_legacy_selection(&row, &imported, &bundled)
                        .map_err(Error::Object)?;
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM provider_selections WHERE scope=?1)",
                            [&selection.scope],
                            |r| r.get(0),
                        )
                        .map_err(crate::native::failure)?;
                    if exists {
                        return Ok(Outcome::Conflict);
                    }
                    tx.execute(
                        "INSERT INTO provider_selections VALUES (?1,1,?2,?3)",
                        params![selection.scope, selection.provider_id, selection.model_id],
                    )
                    .map_err(crate::native::failure)?;
                }
                tx.execute(
                    "INSERT INTO provider_imports VALUES (?1,?2,?3,?4,?5)",
                    params![kind, row.id, snapshot.schema, row.version, row.digest],
                )
                .map_err(crate::native::failure)?;
                tx.commit().map_err(crate::native::failure)?;
                Ok(Outcome::Imported)
            })();
            let (outcome, reason) = match result {
                Ok(outcome) => (outcome, None),
                Err(Error::Object(reason)) => (Outcome::Unsupported, Some(reason)),
                Err(e) => return Err(e),
            };
            let omitted = converted_omitted.unwrap_or_else(|| {
                if kind == "provider" {
                    legacy_omitted_fields(&row).unwrap_or_default()
                } else {
                    Vec::new()
                }
            });
            owner
                .ledger
                .object(
                    &task.id,
                    &ObjectResult {
                        kind: kind.into(),
                        id: row.id,
                        outcome,
                        target_id: None,
                        reason,
                        omitted,
                    },
                )
                .await?;
        }
    }
    Ok(())
}
