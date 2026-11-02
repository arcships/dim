//! Explicit, read-only legacy configuration import. No sessions or OAuth grants.
use super::*;
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde::de::DeserializeOwned;
use serde_json::{Map, json};
use std::path::PathBuf;

const PROVIDER_COLUMNS: &str = "providerId source displayName driverKind defaultBaseUrl baseUrl auth credential accountUsageSettings headers proxy timeoutMs retryConfig models modelConfigOverrides capabilityDefaults activeModelId modelsSource catalogVersion modelsUpdatedAt metadata enabled createdAt updatedAt version";
const SELECTION_COLUMNS: &str =
    "selectionId scope cwd providerId modelId createdAt updatedAt version";

/// The legacy Provider columns the frozen mapping covers.
pub const LEGACY_PROVIDER_FIELDS: &[&str] = &[
    "source",
    "displayName",
    "driverKind",
    "defaultBaseUrl",
    "baseUrl",
    "auth",
    "credential",
    "headers",
    "proxy",
    "timeoutMs",
    "models",
    "modelConfigOverrides",
    "modelsSource",
    "enabled",
];
/// The legacy metadata keys the frozen mapping covers.
pub const LEGACY_METADATA_FIELDS: &[&str] =
    &["enabledModelIds", "disabledModelIds", "modelPreferences"];

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Imported,
    AlreadyImported,
    Rejected,
}

#[derive(Debug, Serialize)]
pub struct ImportItem {
    pub kind: &'static str,
    pub id: String,
    pub status: ImportStatus,
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub items: Vec<ImportItem>,
}

/// One legacy row, with its source identity and content digest.
pub struct LegacyRow {
    pub id: String,
    pub version: i64,
    pub digest: String,
    pub fields: Map<String, Value>,
}

impl LegacyRow {
    pub fn json_field<T: DeserializeOwned>(&self, name: &str) -> Result<T, String> {
        json_field(&self.fields, name)
    }
}

pub struct LegacySnapshot {
    pub schema: String,
    pub providers: Vec<LegacyRow>,
    pub selections: Vec<LegacyRow>,
}

pub fn read_legacy_source(
    path: PathBuf,
    cancel: &CancellationToken,
) -> Result<LegacySnapshot, HostError> {
    if !path.is_absolute() || !path.is_file() {
        return Err(failure("Import source must be an absolute SQLite file"));
    }
    let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| failure("Cannot open legacy SQLite source read-only"))?;
    db.pragma_update(None, "trusted_schema", false)
        .map_err(|_| failure("Cannot secure legacy SQLite reader"))?;
    let tx = db
        .transaction()
        .map_err(|_| failure("Cannot read legacy snapshot"))?;
    let mut schema = Vec::new();
    let mut bytes = 0usize;
    let mut read = |table: &str, columns: &str| -> Result<Vec<LegacyRow>, HostError> {
        let table_type: String = tx
            .query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [table],
                |row| row.get(0),
            )
            .map_err(|_| failure("Legacy provider tables are missing"))?;
        if table_type != "table" {
            return Err(failure("Legacy provider source must use tables"));
        }
        let columns: Vec<_> = columns.split_whitespace().collect();
        let mut statement = tx
            .prepare(&format!("PRAGMA table_xinfo({table})"))
            .map_err(|_| failure("Cannot inspect legacy schema"))?;
        let mut actual = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|_| failure("Cannot inspect legacy schema"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| failure("Cannot inspect legacy schema"))?;
        if actual.len() != columns.len()
            || columns.iter().any(|expected| {
                !actual.iter().any(|(name, ty, hidden, primary)| {
                    name == expected
                        && *hidden == 0
                        && *primary == i64::from(*expected == columns[0])
                        && ty
                            == if matches!(*expected, "version" | "enabled" | "timeoutMs") {
                                "INTEGER"
                            } else {
                                "TEXT"
                            }
                })
            })
        {
            return Err(failure(
                "Unsupported legacy provider schema; source preserved",
            ));
        }
        actual.sort();
        schema.push(actual);
        let mut statement = tx
            .prepare(&format!(
                "SELECT {} FROM {table} ORDER BY {} LIMIT 1025",
                columns.join(","),
                columns[0]
            ))
            .map_err(|_| failure("Cannot read legacy rows"))?;
        let mut rows = statement
            .query([])
            .map_err(|_| failure("Cannot read legacy rows"))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|_| failure("Cannot read legacy row"))? {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled("Provider import cancelled".into()));
            }
            if result.len() == 1024 {
                return Err(failure("Legacy import exceeds 1024 rows per table"));
            }
            let mut fields = Map::new();
            for (index, name) in columns.iter().enumerate() {
                let value = row
                    .get_ref(index)
                    .map_err(|_| failure("Cannot read legacy value"))?;
                bytes += match value {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.len(),
                    _ => 8,
                };
                if bytes > 8 * 1024 * 1024 {
                    return Err(failure("Legacy import exceeds 8 MiB"));
                }
                let value = match value {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Text(value) => Value::String(
                        std::str::from_utf8(value)
                            .map_err(|_| failure("Invalid legacy text"))?
                            .to_owned(),
                    ),
                    _ => return Err(failure("Invalid legacy SQLite value type")),
                };
                fields.insert((*name).into(), value);
            }
            let id = fields[columns[0]]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| failure("Invalid legacy row identity"))?
                .to_owned();
            let version = fields["version"]
                .as_i64()
                .filter(|v| *v > 0 && *v <= dim_agent_types::JS_MAX_SAFE_INTEGER as i64)
                .ok_or_else(|| failure("Invalid legacy row version"))?;
            let digest = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&fields)
                        .map_err(|_| failure("Cannot fingerprint legacy row"))?
                )
            );
            result.push(LegacyRow {
                id,
                version,
                digest,
                fields,
            });
        }
        Ok(result)
    };
    let providers = read("providers", PROVIDER_COLUMNS)?;
    let selections = read("provider_selections", SELECTION_COLUMNS)?;
    let schema = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&schema).map_err(failure)?)
    );
    Ok(LegacySnapshot {
        schema,
        providers,
        selections,
    })
}

fn field<T: DeserializeOwned>(fields: &Map<String, Value>, name: &str) -> Result<T, String> {
    serde_json::from_value(fields[name].clone()).map_err(|_| format!("Invalid {name}"))
}
fn json_field<T: DeserializeOwned>(fields: &Map<String, Value>, name: &str) -> Result<T, String> {
    let value = match &fields[name] {
        Value::Null => Value::Null,
        Value::String(text) => {
            serde_json::from_str(text).map_err(|_| format!("Invalid {name} JSON"))?
        }
        _ => return Err(format!("Invalid {name}")),
    };
    serde_json::from_value(value).map_err(|_| format!("Unsupported or invalid {name}"))
}
fn retains(source: &Value, target: &Value) -> bool {
    match (source, target) {
        (Value::Object(source), Value::Object(target)) => source
            .iter()
            .all(|(key, value)| target.get(key).is_some_and(|target| retains(value, target))),
        (Value::Array(source), Value::Array(target)) => {
            source.len() == target.len() && source.iter().zip(target).all(|(s, t)| retains(s, t))
        }
        _ => source == target,
    }
}

/// The source fields of `source` the frozen mapping drops instead of rejecting.
pub fn legacy_omitted_fields(source: &LegacyRow) -> Result<Vec<String>, String> {
    let fields = &source.fields;
    let mut omitted = Vec::new();
    for name in ["accountUsageSettings", "retryConfig", "capabilityDefaults"] {
        let value: Option<Value> = json_field(fields, name)?;
        if value.as_ref().is_some_and(|value| {
            !matches!(value, Value::Object(fields) if fields.is_empty())
                && !matches!(value, Value::Array(items) if items.is_empty())
        }) {
            omitted.push(name.into());
        }
    }
    let metadata: Option<Map<String, Value>> = json_field(fields, "metadata")?;
    for key in metadata
        .into_iter()
        .flat_map(|value| value.into_iter().map(|(key, _)| key))
    {
        if !LEGACY_METADATA_FIELDS.contains(&key.as_str()) {
            omitted.push(format!("metadata.{key}"));
        }
    }
    Ok(omitted)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyAuth {
    #[serde(rename = "type")]
    kind: String,
    header_name: Option<String>,
    prefix: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase", deny_unknown_fields)]
enum LegacyCredential {
    #[serde(rename = "apiKey")]
    ApiKey {
        api_key: String,
        header_name: Option<String>,
        prefix: Option<String>,
    },
    #[serde(rename = "bearer")]
    Bearer {
        token: String,
        header_name: Option<String>,
        prefix: Option<String>,
    },
    #[serde(rename = "secretRef")]
    Reference {
        r#ref: String,
        credential_type: Option<String>,
        header_name: Option<String>,
        prefix: Option<String>,
    },
    #[serde(rename = "custom")]
    Custom {
        custom_headers: HashMap<String, String>,
    },
    #[serde(rename = "awsBedrockAccessKey")]
    Aws {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
    },
}

/// The native record a legacy row maps to, and the source fields it omits.
pub struct LegacyConversion {
    pub record: StaticProviderRecord,
    pub omitted: Vec<String>,
}

/// Convert one legacy Provider row under the frozen mapping.
pub fn convert_legacy_provider(
    source: &LegacyRow,
    previous: Option<StaticProviderRecord>,
    bundled: &BundledCatalog,
) -> Result<LegacyConversion, String> {
    let fields = &source.fields;
    if managed_authority(&source.id) != CredentialAuthority::Static {
        return Err("Managed OAuth requires its owner's explicit recovery".into());
    }
    let omitted = legacy_omitted_fields(source)?;
    let _: Option<String> = field(fields, "activeModelId")?;
    let source_kind: String = field(fields, "source")?;
    let previous = match previous {
        Some(previous) => Some(previous),
        None if source_kind == "builtin" => Some(
            bundled_seed_for(bundled, &source.id)
                .map_err(|_| "Bundled catalog is unreadable".to_owned())?
                .ok_or_else(|| "Builtin identity has no bundled definition".to_owned())?,
        ),
        None => None,
    };
    let kind = match source_kind.as_str() {
        "custom" if bundled.provider(&source.id).is_some() => {
            return Err("Builtin identity cannot be imported as custom".into());
        }
        "custom" if previous.is_some() => {
            return Err("Target provider exists or is not a pristine builtin".into());
        }
        "custom" => StaticRecordKind::Custom,
        "builtin"
            if previous
                .as_ref()
                .is_none_or(StaticProviderRecord::is_pristine_builtin) =>
        {
            StaticRecordKind::Builtin
        }
        "builtin" => return Err("Target provider exists or is not a pristine builtin".into()),
        _ => return Err("Unsupported source kind".into()),
    };
    let legacy_auth: LegacyAuth = json_field(fields, "auth")?;
    let auth = match legacy_auth.kind.as_str() {
        "api_key" => StaticAuth::ApiKey {
            header_name: legacy_auth
                .header_name
                .unwrap_or_else(|| "Authorization".into()),
            prefix: legacy_auth.prefix.unwrap_or_default(),
        },
        "bearer" if legacy_auth.header_name.is_none() && legacy_auth.prefix.is_none() => {
            StaticAuth::Bearer
        }
        "custom" if legacy_auth.header_name.is_none() && legacy_auth.prefix.is_none() => {
            StaticAuth::Anonymous
        }
        "aws_bedrock" if legacy_auth.header_name.is_none() && legacy_auth.prefix.is_none() => {
            StaticAuth::AwsBedrock
        }
        _ => return Err("Unsupported legacy auth; OAuth credentials must not be imported".into()),
    };
    let mut headers =
        json_field::<Option<HashMap<String, String>>>(fields, "headers")?.unwrap_or_default();
    let credential = json_field::<Option<LegacyCredential>>(fields, "credential")?;
    let check_override = |name: Option<String>, prefix: Option<String>| -> Result<(), String> {
        if let StaticAuth::ApiKey {
            header_name,
            prefix: expected,
        } = &auth
        {
            if name
                .as_ref()
                .is_none_or(|n| n.eq_ignore_ascii_case(header_name))
                && prefix.as_ref().is_none_or(|p| p == expected)
            {
                return Ok(());
            }
        } else if name.is_none() && prefix.is_none() {
            return Ok(());
        }
        Err("Credential auth overrides need explicit reconfiguration".into())
    };
    let credential = match credential {
        None if auth == StaticAuth::Anonymous => {
            return Err("Missing custom credentials cannot become anonymous auth".into());
        }
        None => None,
        Some(LegacyCredential::ApiKey {
            api_key,
            header_name,
            prefix,
        }) if matches!(auth, StaticAuth::ApiKey { .. }) => {
            check_override(header_name, prefix)?;
            Some(StaticCredential::Inline(ProviderSecret::Token(api_key)))
        }
        Some(LegacyCredential::Bearer {
            token,
            header_name,
            prefix,
        }) if matches!(auth, StaticAuth::Bearer | StaticAuth::AwsBedrock) => {
            check_override(header_name, prefix)?;
            Some(StaticCredential::Inline(ProviderSecret::Token(token)))
        }
        Some(LegacyCredential::Reference {
            r#ref,
            credential_type,
            header_name,
            prefix,
        }) => {
            let expected = match auth {
                StaticAuth::ApiKey { .. } => "apiKey",
                StaticAuth::Bearer => "bearer",
                _ => return Err("Unsupported secret reference auth".into()),
            };
            if credential_type
                .as_deref()
                .is_some_and(|kind| kind != expected)
            {
                return Err("Unsupported secret reference type".into());
            }
            check_override(header_name, prefix)?;
            Some(StaticCredential::Reference { reference: r#ref })
        }
        Some(LegacyCredential::Aws {
            access_key_id,
            secret_access_key,
            session_token,
        }) if auth == StaticAuth::AwsBedrock => Some(StaticCredential::Inline(
            ProviderSecret::Aws(AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
            }),
        )),
        Some(LegacyCredential::Custom { custom_headers })
            if auth == StaticAuth::Anonymous && !custom_headers.is_empty() =>
        {
            let mut names = HashSet::new();
            for (key, value) in custom_headers {
                if !names.insert(key.to_ascii_lowercase()) {
                    return Err("Ambiguous case-insensitive custom credential headers".into());
                }
                headers.retain(|name, _| !name.eq_ignore_ascii_case(&key));
                headers.insert(key, value);
            }
            None
        }
        _ => return Err("Credential does not match legacy auth".into()),
    };
    let mut models: Vec<ModelEntry> = json_field(fields, "models")?;
    let mut overrides =
        json_field::<Option<Vec<ModelEntry>>>(fields, "modelConfigOverrides")?.unwrap_or_default();
    for (name, models) in [("models", &models), ("modelConfigOverrides", &overrides)] {
        let original: Option<Value> = json_field(fields, name)?;
        if let Some(original) = original
            && !retains(
                &original,
                &serde_json::to_value(models).map_err(|_| "Invalid model serialization")?,
            )
        {
            return Err(format!("Unsupported fields in {name}"));
        }
    }
    let models_source: String = field(fields, "modelsSource")?;
    if !matches!(models_source.as_str(), "bundled" | "remote" | "user") {
        return Err("Invalid modelsSource".into());
    }
    let mut user_models = if models_source == "user" {
        std::mem::take(&mut models)
    } else {
        Vec::new()
    };
    if models_source == "user" {
        if let Some(previous) = &previous {
            models = previous.models.clone();
        }
        overrides.retain(|entry| {
            if let Some(user) = user_models
                .iter_mut()
                .find(|user| user.model_id == entry.model_id)
            {
                *user = entry.clone();
                false
            } else {
                true
            }
        });
    }
    // User-supplied entries declare what they state, unless the value may be
    // the assumed pair the legacy schema and v24's probe filled in. The
    // `models` of a `bundled` or `remote` row are listing output, so they keep
    // the v26 rewrite's stricter rule: an unmarked limit that `metadata.raw`
    // does not declare is dropped whatever its value. A `user` row's `models`
    // is the seed the record starts from — the previous record's models or,
    // without a target, the bundled definition's — and not the row's input, so
    // it stays exactly as that seed states it.
    for model in &mut models {
        if models_source != "user" {
            drop_undeclared_limits(model);
        }
    }
    for model in user_models.iter_mut().chain(&mut overrides) {
        declare_supplied_limits(model);
    }
    let metadata =
        json_field::<Option<Map<String, Value>>>(fields, "metadata")?.unwrap_or_default();
    let model_preferences: BTreeMap<String, ModelPreferences> = metadata
        .get("modelPreferences")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| "Invalid modelPreferences")?
        .unwrap_or_default();
    let mut disabled: Vec<String> = metadata
        .get("disabledModelIds")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|_| "Invalid disabledModelIds")?
        .unwrap_or_default();
    if let Some(enabled) = metadata.get("enabledModelIds") {
        let enabled: Vec<String> =
            serde_json::from_value(enabled.clone()).map_err(|_| "Invalid enabledModelIds")?;
        let enabled: HashSet<_> = enabled
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect();
        if disabled.iter().any(|id| enabled.contains(id.as_str())) {
            return Err("Conflicting enabled and disabled model IDs".into());
        }
        disabled.extend(
            models
                .iter()
                .chain(&user_models)
                .filter(|model| !enabled.contains(model.model_id.as_str()))
                .map(|model| model.model_id.clone()),
        );
    }
    disabled.sort();
    disabled.dedup();
    let enabled: i64 = field(fields, "enabled")?;
    if !matches!(enabled, 0 | 1) {
        return Err("Invalid enabled flag".into());
    }
    let mut record = StaticProviderRecord {
        kind,
        display_name: field(fields, "displayName")?,
        driver_kind: field(fields, "driverKind")?,
        auth,
        credential_authority: CredentialAuthority::Static,
        base_url: field::<Option<String>>(fields, "baseUrl")?
            .or(field(fields, "defaultBaseUrl")?)
            .unwrap_or_default(),
        network: ModelNetwork {
            proxy: field(fields, "proxy")?,
            timeout_ms: field(fields, "timeoutMs")?,
        },
        headers,
        credential,
        models,
        remote_models: models_source == "remote",
        endpoint_types: BTreeMap::new(),
        routes: HashMap::new(),
        bedrock_region: None,
        model_config_overrides: overrides,
        user_models,
        model_preferences,
        endpoint_overrides: BTreeMap::new(),
        disabled_model_ids: disabled,
        enabled: enabled == 1,
        definition_fingerprint: previous
            .as_ref()
            .and_then(|p| p.definition_fingerprint.clone()),
        version: if previous.is_some() { 2 } else { 1 },
    };
    if let Some(previous) = previous {
        if record.display_name != previous.display_name
            || record.driver_kind != previous.driver_kind
            || record.auth != previous.auth
        {
            return Err("Legacy builtin definition differs from the current definition".into());
        }
        record.endpoint_types = previous.endpoint_types;
    }
    validate_static_record(&source.id, &mut record)
        .map_err(|_| "Invalid or unsupported provider configuration; source preserved")?;
    Ok(LegacyConversion { record, omitted })
}

impl ProviderService {
    pub async fn import_legacy(
        self: &Arc<Self>,
        source_path: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<ImportResult, HostError> {
        let owner = self.clone();
        self.owned(cancel, move |cancel| Box::pin(async move {
            let read_cancel = cancel.clone();
            let snapshot = tokio::task::spawn_blocking(move || read_legacy_source(source_path, &read_cancel)).await.map_err(|_| failure("Legacy reader failed"))??;
            owner.check(&cancel)?;
            let bundled = owner.bundled.clone();
            owner.db.run(move |connection| {
                if cancel.is_cancelled() { return Err(storage("Provider import cancelled")); }
                let tx = connection.transaction().map_err(storage)?;
                let mut items = Vec::new();
                let mut imported: HashMap<String, StaticProviderRecord> = HashMap::new();
                let mut unreadable: HashMap<String, String> = HashMap::new();
                for (kind, rows) in [("provider", snapshot.providers), ("selection", snapshot.selections)] {
                    for source in rows {
                        let seen: Option<String> = tx.query_row("SELECT source_digest FROM provider_imports WHERE kind=?1 AND id=?2", params![kind, source.id], |row| row.get(0)).optional().map_err(storage)?;
                        if let Some(digest) = seen {
                            if kind == "provider" && digest == source.digest {
                                let stored = tx.query_row("SELECT version,record FROM providers WHERE provider_id=?1", [&source.id], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))).optional().map_err(storage)?;
                                match stored.map(|(version, body)| parse_static(&source.id, version, &body)) {
                                    Some(Ok(record)) => { imported.insert(source.id.clone(), record); }
                                    Some(Err(_)) => { unreadable.insert(source.id.clone(), "Target provider is unreadable; preserved".into()); }
                                    None => {}
                                }
                            }
                            items.push(ImportItem { kind, id: source.id, status: ImportStatus::AlreadyImported, reason: Some(if digest == source.digest { "Source row was already imported" } else { "Previously imported identity changed; automatic replay refused" }.into()), omitted: None });
                            continue;
                        }
                        let result: Result<Option<Vec<String>>, String> = if kind == "provider" {
                            let previous = tx.query_row("SELECT version,record FROM providers WHERE provider_id=?1", [&source.id], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))).optional().map_err(storage)?;
                            let previous = previous.map(|(version, body)| parse_static(&source.id, version, &body)).transpose().map_err(|_| "Target provider is unreadable; preserved".to_owned());
                            match previous.and_then(|previous| convert_legacy_provider(&source, previous, &bundled)) {
                                Err(reason) => Err(reason),
                                Ok(conversion) => {
                                    tx.execute("INSERT INTO providers(provider_id,version,record) VALUES (?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET version=excluded.version,record=excluded.record", params![source.id, conversion.record.version(), serde_json::to_string(&conversion.record).map_err(storage)?]).map_err(storage)?;
                                    imported.insert(source.id.clone(), conversion.record);
                                    Ok(Some(conversion.omitted))
                                }
                            }
                        } else {
                            let blocked = source.fields.get("providerId").and_then(Value::as_str).and_then(|id| unreadable.get(id)).cloned();
                            match blocked {
                                Some(reason) => Err(reason),
                                None => match convert_legacy_selection(&source, &imported, &bundled) {
                                    Err(reason) => Err(reason),
                                    Ok(selection) => {
                                        let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM provider_selections WHERE scope=?1)", [&selection.scope], |row| row.get(0)).map_err(storage)?;
                                        if existing { Err("Target selection already exists".into()) }
                                        else {
                                            tx.execute("INSERT INTO provider_selections(scope,version,provider_id,model_id) VALUES (?1,1,?2,?3)", params![selection.scope, selection.provider_id, selection.model_id]).map_err(storage)?;
                                            Ok(None)
                                        }
                                    }
                                },
                            }
                        };
                        match result {
                            Ok(omitted) => {
                                tx.execute("INSERT INTO provider_imports(kind,id,source_schema,source_version,source_digest) VALUES (?1,?2,?3,?4,?5)", params![kind, source.id, snapshot.schema, source.version, source.digest]).map_err(storage)?;
                                items.push(ImportItem { kind, id: source.id, status: ImportStatus::Imported, reason: None, omitted });
                            }
                            Err(reason) => items.push(ImportItem { kind, id: source.id, status: ImportStatus::Rejected, reason: Some(reason), omitted: None }),
                        }
                    }
                }
                tx.commit().map_err(storage)?;
                Ok(ImportResult { items })
            }).await.map_err(failure)
        })).await
    }
}

/// One native selection a legacy selection row maps to.
pub struct LegacySelection {
    pub scope: String,
    pub provider_id: String,
    pub model_id: String,
}

/// Convert one legacy selection row under the frozen mapping.
pub fn convert_legacy_selection(
    source: &LegacyRow,
    imported: &HashMap<String, StaticProviderRecord>,
    bundled: &BundledCatalog,
) -> Result<LegacySelection, String> {
    let fields = &source.fields;
    let scope: String = field(fields, "scope")?;
    let cwd: Option<String> = field(fields, "cwd")?;
    let scope = match (scope.as_str(), cwd) {
        ("global", None) => "global".into(),
        ("workspace", Some(cwd)) => {
            let path = Path::new(&cwd);
            if !path.is_absolute() {
                return Err("Workspace path must be absolute".into());
            }
            let canonical = path
                .canonicalize()
                .map_err(|_| "Workspace directory is unavailable")?;
            if !canonical.is_dir() {
                return Err("Workspace is not a directory".into());
            }
            selection_scope(Some(&canonical)).map_err(|_| "Invalid workspace path")?
        }
        _ => return Err("Invalid selection scope".into()),
    };
    let provider: String = field(fields, "providerId")?;
    let model: String =
        field(fields, "modelId").map_err(|_| "Implicit model selection is not imported")?;
    let record = imported
        .get(&provider)
        .ok_or_else(|| "Selection requires its provider to be imported in this batch".to_owned())?;
    stored_selection(bundled, &provider, record, &model)
        .map_err(|_| "Selected model is unavailable".to_owned())?;
    Ok(LegacySelection {
        scope,
        provider_id: provider,
        model_id: model,
    })
}
