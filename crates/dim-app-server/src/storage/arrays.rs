use super::{codec::hash, sqlite::error};
use dim_agent_runtime::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;

pub(super) const SCHEMA: &[(&str, &str)] = &[(
    "timeline_array_elements",
    "CREATE TABLE timeline_array_elements (session_id TEXT NOT NULL, array_id TEXT NOT NULL, element_index INTEGER NOT NULL CHECK(element_index >= 0), content_ref TEXT NOT NULL, array_length INTEGER, PRIMARY KEY(session_id, array_id, element_index), CHECK((element_index = 0 AND array_length IS NOT NULL AND array_length > 0) OR (element_index > 0 AND array_length IS NULL)))",
)];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub id: String,
    pub length: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Element<V = Value> {
    pub array: String,
    pub index: u64,
    pub value: V,
}

struct Fingerprint {
    digest: Sha256,
    bytes: usize,
}

impl Write for Fingerprint {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.digest.update(bytes);
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "array size overflow")
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn fingerprint(values: &[Value]) -> Result<(String, usize)> {
    let mut fingerprint = Fingerprint {
        digest: Sha256::new().chain_update("array").chain_update([0]),
        bytes: 0,
    };
    serde_json::to_writer(&mut fingerprint, values).map_err(error)?;
    Ok((
        format!("{:x}", fingerprint.digest.finalize()),
        fingerprint.bytes,
    ))
}

pub(super) fn encode(
    connection: &Connection,
    session: &str,
    event: &mut Value,
) -> Result<Option<Reference>> {
    let Some(values) = event
        .get("data")
        .and_then(|data| data.get("changes"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    let (id, bytes) = fingerprint(values)?;
    if bytes <= 4096 {
        return Ok(None);
    }
    let reference = Reference {
        id,
        length: u64::try_from(values.len()).map_err(error)?,
    };
    let existing:Option<(i64,Option<i64>)>=connection.prepare_cached("SELECT element_index,array_length FROM timeline_array_elements WHERE session_id=?1 AND array_id=?2 ORDER BY element_index LIMIT 1").map_err(error)?
        .query_row(params![session,reference.id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(error)?;
    if let Some((index, length)) = existing {
        if index != 0 || length != Some(i64::try_from(reference.length).map_err(error)?) {
            return Err(error("existing array has an invalid header"));
        }
    } else {
        let mut content = connection.prepare_cached("INSERT OR IGNORE INTO timeline_content(session_id,hash,kind,body) VALUES(?1,?2,'array-element',?3)").map_err(error)?;
        let mut locator = connection.prepare_cached("INSERT OR IGNORE INTO timeline_array_elements(session_id,array_id,element_index,content_ref,array_length) VALUES(?1,?2,?3,?4,?5)").map_err(error)?;
        for (index, value) in values.iter().enumerate() {
            let index = u64::try_from(index).map_err(error)?;
            let element = Element {
                array: reference.id.clone(),
                index,
                value,
            };
            let body = serde_json::to_string(&element).map_err(error)?;
            let content_ref = hash("array-element", &body);
            content
                .execute(params![session, content_ref, body])
                .map_err(error)?;
            locator
                .execute(params![
                    session,
                    reference.id,
                    i64::try_from(index).map_err(error)?,
                    content_ref,
                    if index == 0 {
                        Some(i64::try_from(reference.length).map_err(error)?)
                    } else {
                        None
                    }
                ])
                .map_err(error)?;
        }
    }
    event["data"]
        .as_object_mut()
        .expect("the changes array belongs to data")
        .remove("changes");
    Ok(Some(reference))
}

pub(super) fn content_ref(
    connection: &Connection,
    session: &str,
    array: &Reference,
    index: u64,
) -> Result<Option<String>> {
    if index >= array.length {
        return Ok(None);
    }
    connection.prepare_cached("SELECT content_ref FROM timeline_array_elements WHERE session_id=?1 AND array_id=?2 AND element_index=?3").map_err(error)?
        .query_row(params![session,array.id,i64::try_from(index).map_err(error)?],|row|row.get(0)).optional().map_err(error)?
        .ok_or_else(||error(format!("missing array element {}/{index}",array.id))).map(Some)
}

pub(super) fn check(connection: &Connection, session: &str, array: &Reference) -> Result<()> {
    let length: Option<i64> = connection.prepare_cached("SELECT array_length FROM timeline_array_elements WHERE session_id=?1 AND array_id=?2 AND element_index=0").map_err(error)?
        .query_row(params![session,array.id],|row|row.get(0)).optional().map_err(error)?.flatten();
    if array.length == 0 || length != Some(i64::try_from(array.length).map_err(error)?) {
        return Err(error(format!("incomplete array {}", array.id)));
    }
    Ok(())
}

pub(super) fn backfill(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("SELECT session_id,branch_id,sequence,body FROM timeline_events ORDER BY session_id,branch_id,sequence").map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    let mut update = connection.prepare_cached("UPDATE timeline_events SET body=?4 WHERE session_id=?1 AND branch_id=?2 AND sequence=?3").map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        let session: String = row.get(0).map_err(error)?;
        let branch: String = row.get(1).map_err(error)?;
        let sequence: i64 = row.get(2).map_err(error)?;
        let body: String = row.get(3).map_err(error)?;
        let mut physical: super::timeline::Row = serde_json::from_str(&body).map_err(error)?;
        let mut changed = false;
        for wire in physical.wires_mut() {
            changed |= wire.externalize_arrays(connection, &session)?;
        }
        if changed {
            update
                .execute(params![
                    session,
                    branch,
                    sequence,
                    serde_json::to_string(&physical).map_err(error)?
                ])
                .map_err(error)?;
        }
    }
    Ok(())
}
