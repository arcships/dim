use super::*;
use std::borrow::Cow;

type Result<T> = dim_agent_runtime::Result<T>;

const MODEL_FIELDS: &[&str] = &[
    "disabledModelIds",
    "modelPreferences",
    "modelAliases",
    "endpointTypes",
    "endpointOverrides",
];

pub(super) enum Models<'a> {
    Header,
    All,
    Mutable,
    One(&'a str),
    Selected(&'a [String]),
}

struct Image<'a> {
    bundled: &'a BundledCatalog,
    blob: Option<rusqlite::blob::Blob<'a>>,
}

impl<'a> Image<'a> {
    fn open(
        connection: &'a rusqlite::Connection,
        revision: &str,
        bundled: &'a BundledCatalog,
    ) -> Result<Self> {
        let blob = if revision == bundled.revision() {
            None
        } else {
            let row = connection
                .query_row(
                    "SELECT rowid FROM provider_sources WHERE revision=?1",
                    [revision],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            Some(
                connection
                    .blob_open("main", "provider_sources", "image", row, true)
                    .map_err(storage)?,
            )
        };
        let image = Self { bundled, blob };
        if image.number(28)? != 1 {
            return Err(storage("Unsupported admitted catalog image format"));
        }
        Ok(image)
    }

    fn read(&self, start: usize, length: usize) -> Result<Cow<'_, [u8]>> {
        let end = start
            .checked_add(length)
            .ok_or_else(|| storage("Invalid catalog source offset"))?;
        if let Some(blob) = &self.blob {
            if end > blob.len() {
                return Err(storage("Truncated admitted catalog source"));
            }
            let mut bytes = vec![0; length];
            blob.read_at_exact(&mut bytes, start).map_err(storage)?;
            Ok(Cow::Owned(bytes))
        } else {
            self.bundled
                .image()
                .get(start..end)
                .map(Cow::Borrowed)
                .ok_or_else(|| storage("Invalid catalog source offset"))
        }
    }

    fn number(&self, at: usize) -> Result<usize> {
        Ok(u32::from_le_bytes(self.read(at, 4)?.as_ref().try_into().unwrap()) as usize)
    }

    fn span(&self, at: usize) -> Result<Cow<'_, [u8]>> {
        self.read(self.number(at)?, self.number(at + 4)?)
    }

    fn text(&self, at: usize) -> Result<String> {
        String::from_utf8(self.span(at)?.into_owned()).map_err(storage)
    }

    fn bound(&self, key: &str, upper: bool) -> Result<usize> {
        let base = self.number(16)?;
        let (mut lo, mut hi) = (0, self.number(20)?);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let candidate = self.text(base + mid * 12)?;
            if candidate.as_str() < key || (upper && candidate == key) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    fn matching(&self, key: &str) -> Result<Vec<usize>> {
        let base = self.number(16)?;
        (self.bound(key, false)?..self.bound(key, true)?)
            .map(|i| self.number(base + i * 12 + 8))
            .collect()
    }

    fn targets(&self, provider: &str, after: &str, limit: usize) -> Result<Vec<(String, usize)>> {
        if self.blob.is_none() {
            return Ok(self
                .bundled
                .source()
                .targets(provider, after)
                .take(limit)
                .map(|p| (self.bundled.source().model(p).id.to_owned(), p))
                .collect());
        }
        let prefix = format!("t\0{provider}\0");
        let base = self.number(16)?;
        let start = self.bound(&format!("{prefix}{after}"), !after.is_empty())?;
        let mut rows = Vec::new();
        for i in start..self.number(20)? {
            if rows.len() == limit {
                break;
            }
            let key = self.text(base + i * 12)?;
            let Some(id) = key.strip_prefix(&prefix) else {
                break;
            };
            rows.push((id.to_owned(), self.number(base + i * 12 + 8)?));
        }
        Ok(rows)
    }

    fn entry(&self, position: usize) -> Result<Value> {
        #[cfg(feature = "testing")]
        read_work::update(|work| work.source_payloads += 1);
        let at = self.number(8)? + position * 68;
        let mut entry: ModelEntry =
            serde_json::from_reader(flate2::read::GzDecoder::new(self.span(at + 28)?.as_ref()))
                .map_err(storage)?;
        drop_undeclared_limits(&mut entry);
        serde_json::to_value(entry).map_err(storage)
    }

    fn name(&self, position: usize) -> Result<naming::Name> {
        if self.blob.is_none() {
            let row = self.bundled.source().model(position);
            return Ok(naming::Name {
                model_id: row.id.to_owned(),
                display_name: row.name.to_owned(),
                label: row.label.map(str::to_owned),
            });
        }
        let at = self.number(8)? + position * 68;
        let label = self.text(at + 36)?;
        Ok(naming::Name {
            model_id: self.text(at + 4)?,
            display_name: self.text(self.number(24)? + position * 8)?,
            label: (!label.is_empty()).then_some(label),
        })
    }
}

pub(super) fn header(connection: &rusqlite::Connection, id: &str) -> Result<Option<(u32, String)>> {
    let row = connection
        .query_row(
            "SELECT version,record FROM providers WHERE provider_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if row.is_some() {
        return Ok(row);
    }
    let bundled = BundledCatalog::load().map_err(storage)?;
    let Some(provider) = bundled.provider_header(id).filter(seeded) else {
        return Ok(None);
    };
    let mut record = bundled_seed(provider).map_err(storage)?;
    record.definition_fingerprint = bundled.fingerprint(id).map(str::to_owned);
    Ok(Some((
        record.version,
        serde_json::to_string(&record).map_err(storage)?,
    )))
}

pub(super) fn base(
    connection: &rusqlite::Connection,
    id: &str,
    bundled: &BundledCatalog,
) -> Result<Option<String>> {
    let persisted: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM providers WHERE provider_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !persisted {
        return Ok(bundled
            .provider_header(id)
            .filter(seeded)
            .map(|_| bundled.revision().to_owned()));
    }
    connection
        .query_row(
            "SELECT revision FROM provider_bases WHERE provider_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)
}

pub(super) fn read(
    connection: &rusqlite::Connection,
    id: &str,
    models: Models<'_>,
) -> Result<Option<PersistedProviderRecord>> {
    let Some((version, body)) = header(connection, id)? else {
        return Ok(None);
    };
    let mut body: Value = serde_json::from_str(&body).map_err(storage)?;
    if body.get("models").is_some()
        && connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM providers WHERE provider_id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)?
    {
        return parse_provider_row(id, version, &body.to_string())
            .map(Some)
            .map_err(storage);
    }
    let bundled = BundledCatalog::load().map_err(storage)?;
    let mut source_routes = serde_json::Map::new();
    for (source, field) in [
        ("default", "models"),
        ("user", "userModels"),
        ("override", "modelConfigOverrides"),
    ] {
        let mut entries = Vec::new();
        if !matches!(models, Models::Header) {
            if source == "default"
                && let Some(revision) = base(connection, id, &bundled)?
            {
                let image = Image::open(connection, &revision, &bundled)?;
                let positions = match models {
                    Models::One(target) => image.matching(&format!("t\0{id}\0{target}"))?,
                    Models::Selected(targets) => {
                        let mut positions = Vec::new();
                        for target in targets {
                            positions.extend(image.matching(&format!("t\0{id}\0{target}"))?);
                        }
                        positions.sort_unstable();
                        positions
                    }
                    Models::All => {
                        let mut positions: Vec<_> = image
                            .targets(id, "", usize::MAX)?
                            .into_iter()
                            .map(|(_, p)| p)
                            .collect();
                        positions.sort_unstable();
                        positions
                    }
                    Models::Mutable => {
                        let mut query = connection.prepare("SELECT model_id FROM provider_models WHERE provider_id=?1 AND source='override' UNION SELECT model_id FROM provider_model_state WHERE provider_id=?1 AND field='modelAliases'").map_err(storage)?;
                        let targets = query
                            .query_map([id], |r| r.get::<_, String>(0))
                            .map_err(storage)?;
                        let mut positions = Vec::new();
                        for target in targets {
                            positions.extend(
                                image
                                    .matching(&format!("t\0{id}\0{}", target.map_err(storage)?))?,
                            );
                        }
                        positions.sort_unstable();
                        positions
                    }
                    Models::Header => unreachable!(),
                };
                for position in positions {
                    entries.push(image.entry(position)?);
                    let route = image.text(image.number(8)? + position * 68 + 60)?;
                    if !route.is_empty() {
                        source_routes.insert(
                            image.text(image.number(8)? + position * 68 + 4)?,
                            serde_json::from_str(&route).map_err(storage)?,
                        );
                    }
                }
            }
            let selected = if let Models::Selected(targets) = &models {
                Some(serde_json::to_string(targets).map_err(storage)?)
            } else {
                None
            };
            let (sql, target) = match models {
                Models::Selected(_) => (
                    "SELECT entry FROM provider_models WHERE provider_id=?1 AND source=?2 AND model_id IN (SELECT value FROM json_each(?3)) ORDER BY position",
                    selected.as_deref(),
                ),
                Models::One(target) => (
                    "SELECT entry FROM provider_models WHERE provider_id=?1 AND source=?2 AND model_id=?3 ORDER BY position",
                    Some(target),
                ),
                _ => (
                    "SELECT entry FROM provider_models WHERE provider_id=?1 AND source=?2 ORDER BY position",
                    None,
                ),
            };
            let mut query = connection.prepare(sql).map_err(storage)?;
            let mut rows = if let Some(target) = target {
                query.query(params![id, source, target])
            } else {
                query.query(params![id, source])
            }
            .map_err(storage)?;
            while let Some(row) = rows.next().map_err(storage)? {
                #[cfg(feature = "testing")]
                read_work::update(|work| work.stored_payloads += 1);
                let entry: String = row.get(0).map_err(storage)?;
                entries.push(serde_json::from_str(&entry).map_err(storage)?);
            }
        }
        body[field] = Value::Array(entries);
    }
    for field in MODEL_FIELDS {
        body[*field] = if *field == "disabledModelIds" {
            Value::Array(Vec::new())
        } else {
            serde_json::json!({})
        };
    }
    body["endpointTypes"] = Value::Object(source_routes);
    if !matches!(models, Models::Header) {
        let selected = if let Models::Selected(targets) = &models {
            Some(serde_json::to_string(targets).map_err(storage)?)
        } else {
            None
        };
        let sql = if selected.is_some() {
            "SELECT model_id,field,value FROM provider_model_state WHERE provider_id=?1 AND model_id IN (SELECT value FROM json_each(?2)) ORDER BY position"
        } else if matches!(models, Models::One(_)) {
            "SELECT model_id,field,value FROM provider_model_state WHERE provider_id=?1 AND model_id=?2 ORDER BY position"
        } else {
            "SELECT model_id,field,value FROM provider_model_state WHERE provider_id=?1 ORDER BY position"
        };
        let mut statement = connection.prepare(sql).map_err(storage)?;
        let mut rows = if let Some(selected) = &selected {
            statement.query(params![id, selected])
        } else if let Models::One(target) = models {
            statement.query(params![id, target])
        } else {
            statement.query([id])
        }
        .map_err(storage)?;
        while let Some(row) = rows.next().map_err(storage)? {
            let model: String = row.get(0).map_err(storage)?;
            let field: String = row.get(1).map_err(storage)?;
            let value: String = row.get(2).map_err(storage)?;
            let value: Value = serde_json::from_str(&value).map_err(storage)?;
            if field == "disabledModelIds" {
                body[&field]
                    .as_array_mut()
                    .ok_or_else(|| storage("Invalid model state"))?
                    .push(value);
            } else {
                body[&field][model] = value;
            }
        }
    }
    parse_provider_row(id, version, &body.to_string())
        .map(Some)
        .map_err(storage)
}

pub(super) fn static_read(
    connection: &rusqlite::Connection,
    id: &str,
    models: Models<'_>,
) -> Result<Option<StaticProviderRecord>> {
    match read(connection, id, models)? {
        Some(PersistedProviderRecord::Static(record)) => Ok(Some(*record)),
        Some(_) => Err(storage("Invalid persisted provider record")),
        None => Ok(None),
    }
}

pub(super) fn write_header(
    connection: &rusqlite::Connection,
    id: &str,
    version: u32,
    body: &str,
) -> Result<bool> {
    let mut body: Value = serde_json::from_str(body).map_err(storage)?;
    for field in ["models", "userModels", "modelConfigOverrides"]
        .iter()
        .chain(MODEL_FIELDS)
    {
        body.as_object_mut()
            .ok_or_else(|| storage("Invalid provider header"))?
            .remove(*field);
    }
    let inserted = connection
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM providers WHERE provider_id=?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(storage)?;
    connection.execute("INSERT INTO providers VALUES (?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET version=excluded.version,record=excluded.record", params![id, version, body.to_string()]).map_err(storage)?;
    Ok(inserted)
}

pub(super) fn admit_base(
    connection: &rusqlite::Connection,
    id: &str,
    record: &StaticProviderRecord,
    bundled: &BundledCatalog,
) -> Result<()> {
    if record.kind == StaticRecordKind::Builtin
        && !record.remote_models
        && bundled.fingerprint(id).is_some_and(|fingerprint| {
            record.definition_fingerprint.as_deref() == Some(fingerprint)
        })
    {
        connection.execute("INSERT OR IGNORE INTO provider_bases SELECT ?1,revision FROM provider_sources WHERE revision=?2", params![id, bundled.revision()]).map_err(storage)?;
    }
    connection.execute("INSERT OR IGNORE INTO provider_bundle SELECT ?1,?2,?3,0,activation,?4 FROM provider_catalog_revision WHERE id=1", params![id,bundled.fingerprint(id),record.definition_fingerprint,bundled.revision()]).map_err(storage)?;
    Ok(())
}

pub(super) fn write(
    connection: &rusqlite::Connection,
    id: &str,
    version: u32,
    body: &str,
) -> Result<()> {
    write_inner(connection, id, version, body, false)
}

pub(super) fn write_pinned(
    connection: &rusqlite::Connection,
    id: &str,
    version: u32,
    body: &str,
) -> Result<()> {
    write_inner(connection, id, version, body, true)
}

fn write_inner(
    connection: &rusqlite::Connection,
    id: &str,
    version: u32,
    body: &str,
    preserve_history: bool,
) -> Result<()> {
    let record = match parse_provider_row(id, version, body) {
        Ok(record) => record,
        Err(_) => {
            connection.execute("INSERT INTO providers VALUES (?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET version=excluded.version,record=excluded.record", params![id,version,body]).map_err(storage)?;
            return Ok(());
        }
    };
    let bundled = BundledCatalog::load().map_err(storage)?;
    let first_configuration = write_header(connection, id, version, body)?;
    if let PersistedProviderRecord::Static(record) = &record {
        if record.remote_models {
            connection
                .execute("DELETE FROM provider_bases WHERE provider_id=?1", [id])
                .map_err(storage)?;
        } else if first_configuration && !preserve_history {
            admit_base(connection, id, record, &bundled)?;
        }
    }
    let sourced = base(connection, id, &bundled)?.is_some();
    let body: Value = serde_json::from_str(body).map_err(storage)?;
    for (source, field) in [
        ("default", "models"),
        ("user", "userModels"),
        ("override", "modelConfigOverrides"),
    ] {
        let empty = Vec::new();
        let entries = if source == "default" && sourced {
            &empty
        } else {
            body[field].as_array().unwrap_or(&empty)
        };
        let mut retained = HashSet::new();
        for (position, entry) in entries.iter().enumerate() {
            let model = entry.get("entry").unwrap_or(entry);
            let model_id = model["modelId"]
                .as_str()
                .ok_or_else(|| storage("Missing model identity"))?;
            retained.insert(model_id);
            connection.execute("INSERT INTO provider_models VALUES (?1,?2,?3,?4,?5) ON CONFLICT(provider_id,model_id,source) DO UPDATE SET position=excluded.position,entry=excluded.entry WHERE position!=excluded.position OR entry!=excluded.entry", params![id,model_id,source,position as i64,entry.to_string()]).map_err(storage)?;
        }
        let mut query = connection
            .prepare("SELECT model_id FROM provider_models WHERE provider_id=?1 AND source=?2")
            .map_err(storage)?;
        let previous = query
            .query_map(params![id, source], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        for model in previous {
            if !retained.contains(model.as_str()) {
                connection.execute("DELETE FROM provider_models WHERE provider_id=?1 AND model_id=?2 AND source=?3", params![id,model,source]).map_err(storage)?;
            }
        }
    }
    for field in MODEL_FIELDS {
        let values: Vec<_> = if *field == "disabledModelIds" {
            body[*field]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(p, v)| v.as_str().map(|id| (id, p, v)))
                .collect()
        } else {
            body[*field]
                .as_object()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(p, (id, v))| (id.as_str(), p, v))
                .collect()
        };
        let retained: HashSet<_> = values.iter().map(|(id, _, _)| *id).collect();
        for (model, position, value) in &values {
            connection.execute("INSERT INTO provider_model_state VALUES (?1,?2,?3,?4,?5) ON CONFLICT(provider_id,model_id,field) DO UPDATE SET position=excluded.position,value=excluded.value WHERE position!=excluded.position OR value!=excluded.value",params![id,model,field,*position as i64,value.to_string()]).map_err(storage)?;
        }
        let mut query = connection
            .prepare("SELECT model_id FROM provider_model_state WHERE provider_id=?1 AND field=?2")
            .map_err(storage)?;
        let previous = query
            .query_map(params![id, field], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        for model in previous {
            if !retained.contains(model.as_str()) {
                connection.execute("DELETE FROM provider_model_state WHERE provider_id=?1 AND model_id=?2 AND field=?3",params![id,model,field]).map_err(storage)?;
            }
        }
    }
    write_names(connection, id, &record, &body, sourced, &bundled)?;
    connection
        .execute(
            "UPDATE provider_bundle SET naming_revision=?2 WHERE provider_id=?1",
            params![id, bundled.revision()],
        )
        .map_err(storage)?;

    if !matches!(&record,PersistedProviderRecord::Static(r) if r.kind==StaticRecordKind::Builtin) {
        connection.execute("INSERT INTO provider_bundle VALUES (?1,?2,NULL,0,0,?2) ON CONFLICT(provider_id) DO UPDATE SET fingerprint=excluded.fingerprint,naming_revision=excluded.naming_revision",params![id,bundled.revision()]).map_err(storage)?;
    }
    Ok(())
}

fn write_names(
    connection: &rusqlite::Connection,
    id: &str,
    record: &PersistedProviderRecord,
    body: &Value,
    sourced: bool,
    bundled: &BundledCatalog,
) -> Result<()> {
    let entries = match record {
        PersistedProviderRecord::Static(record) => static_entries(bundled, id, record),
        PersistedProviderRecord::Managed(record) => record
            .effective_models(bundled)
            .into_iter()
            .map(|m| m.entry)
            .collect(),
    };
    connection
        .execute("DELETE FROM provider_names WHERE provider_id=?1", [id])
        .map_err(storage)?;
    for entry in entries {
        if sourced
            && body["modelAliases"].get(&entry.model_id).is_none()
            && body["userModels"]
                .as_array()
                .is_none_or(|rows| !rows.iter().any(|r| r["modelId"] == entry.model_id))
            && body["modelConfigOverrides"]
                .as_array()
                .is_none_or(|rows| !rows.iter().any(|r| r["modelId"] == entry.model_id))
        {
            continue;
        }
        let name = naming::Name::from(&entry);
        connection
            .execute(
                "INSERT INTO provider_names VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    name.model_id,
                    name.display_name,
                    name.label,
                    body["models"]
                        .as_array()
                        .is_some_and(|rows| rows
                            .iter()
                            .any(|r| r.get("entry").unwrap_or(r)["modelId"] == name.model_id
                                && r["zdr"] == true))
                ],
            )
            .map_err(storage)?;
        for key in naming::keys(&name) {
            connection
                .execute(
                    "INSERT INTO provider_name_keys VALUES (?1,?2,?3)",
                    params![id, key, name.model_id],
                )
                .map_err(storage)?;
        }
    }
    Ok(())
}

pub(crate) fn migrate(connection: &rusqlite::Connection) -> Result<()> {
    let mut after = String::new();
    loop {
        let row: Option<(String,u32,String)> = connection.query_row("SELECT provider_id,version,record FROM providers WHERE provider_id>?1 ORDER BY provider_id LIMIT 1", [&after], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
        let Some((id, version, body)) = row else {
            break;
        };
        write_pinned(connection, &id, version, &body)?;
        after = id;
    }
    Ok(())
}

pub(super) fn ids(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
) -> Result<BTreeSet<String>> {
    let mut query = connection
        .prepare("SELECT provider_id FROM providers ORDER BY provider_id")
        .map_err(storage)?;
    let mut ids = query
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(storage)?;
    ids.extend(bundled.provider_ids().map(str::to_owned));
    Ok(ids)
}

pub(super) fn providers_for_model(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
    model: &str,
) -> Result<BTreeSet<String>> {
    let mut query=connection.prepare("SELECT DISTINCT provider_id FROM provider_models WHERE model_id=?1 AND source!='override'").map_err(storage)?;
    let mut ids = query
        .query_map([model], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(storage)?;
    ids.extend(bundled.source().matching(&format!("m\0{model}")).map(|p| {
        bundled
            .source()
            .provider(bundled.source().model(p).provider)
            .id
            .to_owned()
    }));
    let mut query = connection
        .prepare("SELECT provider_id,revision FROM provider_bases WHERE revision!=?1")
        .map_err(storage)?;
    for row in query
        .query_map([bundled.revision()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(storage)?
    {
        let (provider, revision) = row.map_err(storage)?;
        if !Image::open(connection, &revision, bundled)?
            .matching(&format!("t\0{provider}\0{model}"))?
            .is_empty()
        {
            ids.insert(provider);
        }
    }
    Ok(ids)
}

pub(super) fn model_keys(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
    provider: &str,
    target: Option<&str>,
    after: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut keys = BTreeSet::new();
    if let Some(revision) = base(connection, provider, bundled)? {
        let image = Image::open(connection, &revision, bundled)?;
        if let Some(target) = target {
            if target > after
                && !image
                    .matching(&format!("t\0{provider}\0{target}"))?
                    .is_empty()
            {
                keys.insert(target.to_owned());
            }
        } else {
            keys.extend(
                image
                    .targets(provider, after, limit)?
                    .into_iter()
                    .map(|(id, _)| id),
            );
        }
    }
    #[cfg(feature = "testing")]
    read_work::update(|work| work.candidate_keys += keys.len());
    let sql = if target.is_some() {
        "SELECT DISTINCT model_id FROM provider_models WHERE provider_id=?1 AND model_id=?2 AND source!='override' AND model_id>?3"
    } else {
        "SELECT DISTINCT model_id FROM provider_models WHERE provider_id=?1 AND model_id>?2 AND source!='override' ORDER BY model_id LIMIT ?3"
    };
    let mut query = connection.prepare(sql).map_err(storage)?;
    let mut rows = if let Some(target) = target {
        query.query(params![provider, target, after])
    } else {
        query.query(params![provider, after, limit as i64])
    }
    .map_err(storage)?;
    while let Some(row) = rows.next().map_err(storage)? {
        #[cfg(feature = "testing")]
        read_work::update(|work| work.candidate_keys += 1);
        keys.insert(row.get::<_, String>(0).map_err(storage)?);
    }
    drop(rows);
    #[cfg(feature = "testing")]
    read_work::update(|work| {
        work.candidate_vm_steps += query.get_status(rusqlite::StatementStatus::VmStep) as usize
    });
    Ok(keys.into_iter().take(limit).collect())
}

pub(super) fn name(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
    provider: &str,
    model: &str,
) -> Result<Option<naming::Name>> {
    let name = connection
        .query_row(
            "SELECT display_name,label FROM provider_names WHERE provider_id=?1 AND model_id=?2",
            params![provider, model],
            |r| {
                Ok(naming::Name {
                    model_id: model.to_owned(),
                    display_name: r.get(0)?,
                    label: r.get(1)?,
                })
            },
        )
        .optional()
        .map_err(storage)?;
    if name.is_some() {
        #[cfg(feature = "testing")]
        read_work::update(|work| work.naming_rows += 1);
        return Ok(name);
    }
    let Some(revision) = base(connection, provider, bundled)? else {
        return Ok(None);
    };
    let image = Image::open(connection, &revision, bundled)?;
    let name = image
        .matching(&format!("t\0{provider}\0{model}"))?
        .first()
        .map(|&p| image.name(p))
        .transpose()?;
    #[cfg(feature = "testing")]
    if name.is_some() {
        read_work::update(|work| work.naming_rows += 1);
    }
    Ok(name)
}

pub(super) fn names(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
    provider: &str,
    targets: &[String],
) -> Result<HashMap<String, String>> {
    let revision = base(connection, provider, bundled)?;
    let image = revision
        .as_ref()
        .map(|revision| Image::open(connection, revision, bundled))
        .transpose()?;
    let matching = |key: &str| -> Result<Vec<String>> {
        let mut ids = BTreeSet::new();
        let mut query = connection
            .prepare("SELECT model_id FROM provider_name_keys WHERE provider_id=?1 AND key=?2")
            .map_err(storage)?;
        ids.extend(
            query
                .query_map(params![provider, key], |r| r.get::<_, String>(0))
                .map_err(storage)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?,
        );
        if let Some(image) = &image {
            for position in image.matching(&format!("s\0{provider}\0{key}"))? {
                ids.insert(image.name(position)?.model_id);
            }
        }
        let mut matching = Vec::new();
        for id in ids {
            if name(connection, bundled, provider, &id)?
                .is_some_and(|n| naming::keys(&n).contains(key))
            {
                matching.push(id);
            }
        }
        Ok(matching)
    };
    let mut found = BTreeMap::new();
    let mut visited = HashSet::new();
    let mut queue = Vec::new();
    for id in targets {
        let Some(name) = name(connection, bundled, provider, id)? else {
            continue;
        };
        let peers = matching(&format!("=\0{}", name.display_name.trim()))?;
        if peers.len() == 1 {
            found.insert(id.clone(), name);
        } else {
            queue.extend(peers);
        }
    }
    while let Some(id) = queue.pop() {
        if found.contains_key(&id) {
            continue;
        }
        let Some(name) = name(connection, bundled, provider, &id)? else {
            continue;
        };
        for key in naming::keys(&name) {
            if !visited.insert(key.clone()) {
                continue;
            }
            queue.extend(matching(&key)?);
        }
        found.insert(id, name);
    }
    let entries: Vec<_> = found.into_values().collect();
    let labels = naming::unique_names(&entries);
    Ok(entries
        .into_iter()
        .zip(labels)
        .map(|(name, label)| (name.model_id, label))
        .collect())
}

pub(super) struct OrderedModel {
    pub source: String,
    pub position: i64,
    pub model_id: String,
}

pub(super) fn ordered_models(
    connection: &rusqlite::Connection,
    bundled: &BundledCatalog,
    provider: &str,
    after: Option<&OrderedModel>,
    limit: usize,
) -> Result<Vec<OrderedModel>> {
    let mut result = Vec::new();
    if after.is_none_or(|a| a.source == "base")
        && let Some(revision) = base(connection, provider, bundled)?
    {
        let image = Image::open(connection, &revision, bundled)?;
        if let Some((_, position)) = image.targets(provider, "", 1)?.first() {
            let provider = image.number(image.number(8)? + position * 68)?;
            let at = image.number(0)? + provider * 32;
            let start = after.map_or(image.number(at + 16)?, |a| a.position as usize + 1);
            let end = image.number(at + 20)?;
            for position in (start..end).take(limit) {
                result.push(OrderedModel {
                    source: "base".into(),
                    position: position as i64,
                    model_id: image.text(image.number(8)? + position * 68 + 4)?,
                });
            }
        }
    }
    if result.len() < limit {
        let mut statement=connection.prepare("SELECT source,position,model_id FROM provider_models WHERE provider_id=?1 AND source!='override' AND (source,position)>(?2,?3) ORDER BY source,position LIMIT ?4").map_err(storage)?;
        let source = after.map_or("", |a| a.source.as_str());
        let position = after.map_or(-1, |a| a.position);
        let rows = statement
            .query_map(
                params![
                    provider,
                    source,
                    position,
                    (limit - result.len()).min(i64::MAX as usize) as i64
                ],
                |r| {
                    Ok(OrderedModel {
                        source: r.get(0)?,
                        position: r.get(1)?,
                        model_id: r.get(2)?,
                    })
                },
            )
            .map_err(storage)?;
        result.extend(
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?,
        );
    }
    Ok(result)
}

pub(super) fn adopt(
    connection: &rusqlite::Connection,
    id: &str,
    bundled: &BundledCatalog,
    cancel: &CancellationToken,
) -> Result<()> {
    let activation: i64 = connection
        .query_row(
            "SELECT activation FROM provider_catalog_revision WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let installed: Option<(Option<String>, bool, i64, String)> = connection
        .query_row(
            "SELECT fingerprint,pending,attempted,naming_revision FROM provider_bundle WHERE provider_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((fingerprint, pending, attempted, generation)) = &installed {
        if generation != bundled.revision() {
            if let Ok(Some(record)) = read(connection, id, Models::Mutable) {
                let body = serde_json::to_value(&record).map_err(storage)?;
                write_names(
                    connection,
                    id,
                    &record,
                    &body,
                    base(connection, id, bundled)?.is_some(),
                    bundled,
                )?;
            }
            if cancel.is_cancelled() {
                return Err(storage("Provider source adoption cancelled"));
            }
            connection
                .execute(
                    "UPDATE provider_bundle SET naming_revision=?2 WHERE provider_id=?1",
                    params![id, bundled.revision()],
                )
                .map_err(storage)?;
        }
        if ((fingerprint.as_deref() == bundled.fingerprint(id)
            || fingerprint.as_deref() == Some(bundled.revision()))
            && !pending)
            || (*pending && *attempted == activation)
        {
            return Ok(());
        }
    }
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM providers WHERE provider_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !exists {
        return Ok(());
    }
    let Ok(Some(record)) = read(connection, id, Models::Header) else {
        return Ok(());
    };
    let header = match record {
        PersistedProviderRecord::Static(record) if record.kind == StaticRecordKind::Builtin => {
            record
        }
        _ => {
            if installed.is_none() {
                let record = read(connection, id, Models::Mutable)?
                    .ok_or_else(|| storage("Provider disappeared"))?;
                write_names(
                    connection,
                    id,
                    &record,
                    &serde_json::to_value(&record).map_err(storage)?,
                    false,
                    bundled,
                )?;
            }
            connection.execute("INSERT INTO provider_bundle VALUES (?1,?2,NULL,0,?3,?2) ON CONFLICT(provider_id) DO UPDATE SET fingerprint=excluded.fingerprint,naming_revision=excluded.naming_revision",params![id,bundled.revision(),activation]).map_err(storage)?;
            return Ok(());
        }
    };
    let fingerprint = bundled.fingerprint(id);
    let expected = bundled
        .provider_header(id)
        .filter(seeded)
        .and(fingerprint)
        .map(str::to_owned);
    let mut pending = false;
    if header.definition_fingerprint != expected || (expected.is_none() && header.enabled) {
        let definition = bundled_seed_for(bundled, id).map_err(storage)?;
        let previous =
            read_stored_static(connection, id)?.ok_or_else(|| storage("Provider disappeared"))?;
        let mut record = if let Some(definition) = definition {
            reseed_candidate(&previous, definition)
        } else {
            let mut record = previous.clone();
            record.enabled = false;
            record
        };
        if record
            .user_models
            .iter()
            .any(|user| record.models.iter().any(|m| m.model_id == user.model_id))
        {
            pending = true;
        } else {
            record.version = previous
                .version
                .checked_add(1)
                .ok_or_else(|| storage("Provider version overflow"))?;
            validate_static_record(id, &mut record).map_err(storage)?;
            if cancel.is_cancelled() {
                return Err(storage("Provider source adoption cancelled"));
            }
            connection
                .execute("DELETE FROM provider_bases WHERE provider_id=?1", [id])
                .map_err(storage)?;
            admit_base(connection, id, &record, bundled)?;
            persistence::write(
                connection,
                id,
                record.version,
                &serde_json::to_string(&record).map_err(storage)?,
            )?;
        }
    }
    connection
        .execute(
            "INSERT OR REPLACE INTO provider_bundle VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                id,
                fingerprint,
                expected,
                pending,
                activation,
                bundled.revision()
            ],
        )
        .map_err(storage)?;
    Ok(())
}
