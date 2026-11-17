use super::sqlite::error;
use dim_agent_runtime::Result;
use rusqlite::{Connection, params};
use serde_json::Value;

pub(super) const SCHEMA: &[(&str, &str)] = &[
    (
        "timeline_output_ids",
        "CREATE TABLE timeline_output_ids (session_id TEXT NOT NULL,event_id TEXT NOT NULL,branch_id TEXT NOT NULL,physical_sequence INTEGER NOT NULL CHECK(physical_sequence > 0),logical_sequence INTEGER NOT NULL CHECK(logical_sequence >= physical_sequence),PRIMARY KEY(session_id,event_id))",
    ),
    (
        "timeline_output_ids_by_position",
        "CREATE INDEX timeline_output_ids_by_position ON timeline_output_ids(session_id,branch_id,physical_sequence,logical_sequence,event_id)",
    ),
];

pub(super) fn insert(
    connection: &Connection,
    session: &str,
    branch: &str,
    physical: u64,
    id: &str,
    logical: u64,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO timeline_output_ids VALUES(?1,?2,?3,?4,?5)",
            params![
                session,
                id,
                branch,
                i64::try_from(physical).map_err(error)?,
                i64::try_from(logical).map_err(error)?
            ],
        )
        .map_err(error)?;
    Ok(())
}

pub(super) fn row(
    connection: &Connection,
    session: &str,
    branch: &str,
    physical: u64,
    encoded: &str,
    through: u64,
) -> Result<()> {
    let row: super::timeline::Row = serde_json::from_str(encoded).map_err(error)?;
    for wire in row.wires() {
        let logical = wire
            .event
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| error("output has no logical sequence"))?;
        let id = wire
            .event
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| error("output has no event identity"))?;
        if logical < physical {
            return Err(error("output identity precedes its physical row"));
        }
        if logical <= through {
            insert(connection, session, branch, physical, id, logical)?;
        }
    }
    Ok(())
}

pub(super) fn backfill(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("SELECT session_id,branch_id,sequence,last_sequence,body FROM timeline_events WHERE kind IN ('step/text','step/thinking') ORDER BY session_id,branch_id,sequence").map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    while let Some(record) = rows.next().map_err(error)? {
        row(
            connection,
            &record.get::<_, String>(0).map_err(error)?,
            &record.get::<_, String>(1).map_err(error)?,
            super::timeline::read_sequence(record, 2).map_err(error)?,
            &record.get::<_, String>(4).map_err(error)?,
            super::timeline::read_sequence(record, 3).map_err(error)?,
        )?;
    }
    Ok(())
}

pub fn initialize_session_output_id_index(connection: &Connection, session: &str) -> Result<()> {
    let mut statement = connection.prepare("SELECT branch_id,sequence,last_sequence,body FROM timeline_events WHERE session_id=?1 AND kind IN ('step/text','step/thinking') ORDER BY branch_id,sequence").map_err(error)?;
    let mut rows = statement.query([session]).map_err(error)?;
    while let Some(record) = rows.next().map_err(error)? {
        row(
            connection,
            session,
            &record.get::<_, String>(0).map_err(error)?,
            super::timeline::read_sequence(record, 1).map_err(error)?,
            &record.get::<_, String>(3).map_err(error)?,
            super::timeline::read_sequence(record, 2).map_err(error)?,
        )?;
    }
    Ok(())
}
