use super::sqlite::error;
use dim_agent_runtime::{Result, TimelineReadScope};
use rusqlite::{Connection, OptionalExtension, params, types::Value as SqlValue};
use serde_json::Value;

pub(super) const SCHEMA: &[(&str, &str)] = &[
    (
        "timeline_text",
        "CREATE TABLE timeline_text (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL, physical_sequence INTEGER NOT NULL, property TEXT NOT NULL, partition_key TEXT NOT NULL, UNIQUE(session_id, branch_id, sequence, property))",
    ),
    (
        "timeline_text_partition",
        "CREATE INDEX timeline_text_partition ON timeline_text(session_id, branch_id, property, partition_key, sequence)",
    ),
    (
        "timeline_text_fts",
        "CREATE VIRTUAL TABLE timeline_text_fts USING fts5(text, content='', contentless_delete=1, detail=none, tokenize='trigram case_sensitive 1')",
    ),
    (
        "timeline_text_delete",
        "CREATE TRIGGER timeline_text_delete AFTER DELETE ON timeline_text BEGIN DELETE FROM timeline_text_fts WHERE rowid=old.id; END",
    ),
    (
        "timeline_text_tails",
        "CREATE TABLE timeline_text_tails (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, request_id TEXT NOT NULL, tail TEXT NOT NULL, PRIMARY KEY(session_id, branch_id, request_id))",
    ),
];

const DATA_EXCLUDED: &[&str] = &["context/prepare", "model/call", "context/compaction"];

pub(super) fn data_index_sql(kind: &str) -> String {
    let domains = dim_agent_types::RESERVED_DOMAINS
        .iter()
        .map(|domain| format!("'{domain}'"))
        .collect::<Vec<_>>()
        .join(",");
    let excluded = DATA_EXCLUDED
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!("(substr({kind},1,instr({kind},'/')-1) IN ({domains}) AND {kind} NOT IN ({excluded}))")
}

pub(super) fn indexed(kind: &str, field: &str) -> bool {
    match field {
        "data" => {
            !DATA_EXCLUDED.contains(&kind)
                && kind
                    .split_once('/')
                    .is_some_and(|(domain, _)| dim_agent_types::RESERVED_DOMAINS.contains(&domain))
        }
        "data.delta" => kind == "step/text",
        _ => false,
    }
}

pub(super) fn restrict_coverage(connection: &Connection) -> Result<()> {
    let indexed = data_index_sql("e.kind");
    connection.execute_batch(&format!("DELETE FROM timeline_text WHERE property='data' AND NOT EXISTS(SELECT 1 FROM timeline_events e WHERE e.session_id=timeline_text.session_id AND e.branch_id=timeline_text.branch_id AND e.sequence=timeline_text.physical_sequence AND {indexed})")).map_err(error)
}

fn document(
    connection: &Connection,
    location: (&str, &str),
    sequence: u64,
    physical: u64,
    field: &str,
    key: &str,
    text: &str,
) -> Result<()> {
    let (session, branch) = location;
    let id: i64 = connection.prepare_cached("INSERT INTO timeline_text(session_id,branch_id,sequence,physical_sequence,property,partition_key) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_id,branch_id,sequence,property) DO UPDATE SET physical_sequence=excluded.physical_sequence,partition_key=excluded.partition_key RETURNING id")
        .map_err(error)?.query_row(params![session,branch,sequence as i64,physical as i64,field,key], |row|row.get(0)).map_err(error)?;
    connection
        .prepare_cached("DELETE FROM timeline_text_fts WHERE rowid=?1")
        .map_err(error)?
        .execute([id])
        .map_err(error)?;
    connection
        .prepare_cached("INSERT INTO timeline_text_fts(rowid,text) VALUES(?1,?2)")
        .map_err(error)?
        .execute(params![id, text.to_lowercase()])
        .map_err(error)?;
    Ok(())
}

pub(super) fn event(
    connection: &Connection,
    session: &str,
    branch: &str,
    physical: u64,
    event: &Value,
) -> Result<()> {
    let sequence = event["sequence"]
        .as_u64()
        .ok_or_else(|| error("text index event has no sequence"))?;
    let kind = format!(
        "{}/{}",
        event["type"]
            .as_str()
            .ok_or_else(|| error("text index event has no type"))?,
        event["name"]
            .as_str()
            .ok_or_else(|| error("text index event has no name"))?
    );
    let data = &event["data"];
    let request = data["request_id"].as_str();
    let key = request.map_or_else(
        || format!("event:{sequence}"),
        |request| format!("request:{request}"),
    );
    if indexed(&kind, "data") {
        document(
            connection,
            (session, branch),
            sequence,
            physical,
            "data",
            &key,
            &data.to_string(),
        )?;
    }
    if kind == "step/text" {
        let request = request.ok_or_else(|| error("text event has no request identity"))?;
        let delta = data["delta"]
            .as_str()
            .ok_or_else(|| error("text event has no delta"))?;
        let tail: Option<String>=connection.prepare_cached("SELECT tail FROM timeline_text_tails WHERE session_id=?1 AND branch_id=?2 AND request_id=?3").map_err(error)?.query_row(params![session,branch,request],|row|row.get(0)).optional().map_err(error)?;
        let text = tail.unwrap_or_default() + delta;
        document(
            connection,
            (session, branch),
            sequence,
            physical,
            "data.delta",
            &key,
            &text,
        )?;
        let tail: String = text
            .chars()
            .rev()
            .take(199)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        connection.prepare_cached("INSERT INTO timeline_text_tails VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,branch_id,request_id) DO UPDATE SET tail=excluded.tail").map_err(error)?.execute(params![session,branch,request,tail]).map_err(error)?;
    }
    Ok(())
}

pub(super) fn row(
    connection: &Connection,
    session: &str,
    branch: &str,
    physical: u64,
    body: &str,
) -> Result<()> {
    row_through(connection, session, branch, physical, body, u64::MAX)
}

pub(super) fn row_through(
    connection: &Connection,
    session: &str,
    branch: &str,
    physical: u64,
    body: &str,
    through: u64,
) -> Result<()> {
    let row: super::timeline::Row = serde_json::from_str(body).map_err(error)?;
    match row {
        super::timeline::Row::Event { mut record } => {
            let kind = format!(
                "{}/{}",
                record.event["type"]
                    .as_str()
                    .ok_or_else(|| error("text event has no type"))?,
                record.event["name"]
                    .as_str()
                    .ok_or_else(|| error("text event has no name"))?
            );
            if record.changes.is_some() && indexed(&kind, "data") {
                let mut reader = super::codec::Reader::new(0);
                loop {
                    match reader.property(connection, session, &record, "data")? {
                        Some(dim_agent_runtime::QueryValue::Value { value }) => {
                            record.event["data"] = value;
                            break;
                        }
                        Some(_) => return Err(error("array event has no data object")),
                        None => reader.begin_hold(),
                    }
                }
            }
            event(connection, session, branch, physical, &record.event)
        }
        super::timeline::Row::Output {
            turn_id,
            request_id,
            records,
            ..
        } => {
            for record in records {
                let mut item = record.event;
                item["data"]["turn_id"] = turn_id.clone().into();
                item["data"]["request_id"] = request_id.clone().into();
                if item["sequence"]
                    .as_u64()
                    .is_some_and(|sequence| sequence <= through)
                {
                    event(connection, session, branch, physical, &item)?;
                }
            }
            Ok(())
        }
    }
}

pub(super) fn backfill(connection: &Connection) -> Result<()> {
    let mut statement=connection.prepare("SELECT session_id,branch_id,sequence,body FROM timeline_events ORDER BY session_id,branch_id,sequence").map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    while let Some(item) = rows.next().map_err(error)? {
        row(
            connection,
            &item.get::<_, String>(0).map_err(error)?,
            &item.get::<_, String>(1).map_err(error)?,
            super::timeline::read_sequence(item, 2).map_err(error)?,
            &item.get::<_, String>(3).map_err(error)?,
        )?;
    }
    Ok(())
}

/// Builds text candidates for a newly imported, unindexed Session. Call inside the transaction
/// that inserts its events, before committing or exposing the Session.
pub fn initialize_session_text_index(connection: &Connection, session: &str) -> Result<()> {
    let mut statement = connection.prepare("SELECT branch_id,sequence,body FROM timeline_events WHERE session_id=?1 ORDER BY branch_id,sequence").map_err(error)?;
    let mut rows = statement.query([session]).map_err(error)?;
    while let Some(item) = rows.next().map_err(error)? {
        row(
            connection,
            session,
            &item.get::<_, String>(0).map_err(error)?,
            super::timeline::read_sequence(item, 1).map_err(error)?,
            &item.get::<_, String>(2).map_err(error)?,
        )?;
    }
    Ok(())
}

/// Compiles one candidate-position insertion, or retains all rows for unsupported expressions.
pub(super) fn candidates(
    scope: &TimelineReadScope,
    field: &str,
    partition: &[String],
    terms: &[String],
    table: &str,
) -> Option<(String, Vec<SqlValue>)> {
    if !matches!(field, "data" | "data.delta")
        || !(field == "data" && partition.is_empty()
            || field == "data.delta" && partition == ["kind", "data.request_id"])
        || !partition.is_empty() && scope.segments.len() > 1
    {
        return None;
    }
    let grams = trigrams(terms)?;
    let mut parameters = vec![
        SqlValue::Text(scope.key.session_id.clone()),
        SqlValue::Text(field.into()),
    ];
    let group = if partition.is_empty() {
        "id"
    } else {
        "branch_id,partition_key"
    };
    let mut intersections = Vec::new();
    for grams in grams {
        parameters.push(SqlValue::Text(grams));
        intersections.push(format!("SELECT DISTINCT {group} FROM timeline_text_fts CROSS JOIN timeline_text ON timeline_text.id=timeline_text_fts.rowid WHERE timeline_text_fts MATCH ?{} AND session_id=?1 AND property=?2",parameters.len()));
    }
    let matching = intersections.join(" INTERSECT ");
    let membership = if partition.is_empty() {
        "m.id=hit.id"
    } else {
        "m.branch_id=hit.branch_id AND m.partition_key=hit.partition_key"
    };
    let mut segments = Vec::new();
    for (ordinal, segment) in scope.segments.iter().enumerate() {
        parameters.push(SqlValue::Text(segment.branch_id.clone()));
        segments.push(format!("SELECT {ordinal} AS ord,m.sequence AS seq,m.physical_sequence AS physical FROM matching hit CROSS JOIN timeline_text m ON {membership} WHERE m.session_id=?1 AND m.property=?2 AND m.branch_id=?{}",parameters.len()));
    }
    if !segments.is_empty() {
        let sql = format!(
            "WITH matching AS MATERIALIZED ({matching}) INSERT INTO {table} {}",
            segments.join(" UNION ALL ")
        );
        return Some((sql, parameters));
    }
    None
}

pub(super) fn trigrams(terms: &[String]) -> Option<Vec<String>> {
    let grams = terms
        .iter()
        .map(|term| term.to_lowercase())
        .filter(|term| {
            (3..=200).contains(&term.len())
                && term.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'=')
                })
        })
        .map(|term| {
            term.as_bytes()
                .windows(3)
                .map(|gram| format!("\"{}\"", std::str::from_utf8(gram).unwrap()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(" AND ")
        })
        .collect::<Vec<_>>();
    (!grams.is_empty()).then_some(grams)
}
