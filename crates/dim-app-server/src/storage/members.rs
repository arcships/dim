use super::sqlite::error;
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOrderKey, QuerySelect, QuerySpec, QueryValue,
    Result, TimelineQueryDirection, TimelineReadScope, event_property, project_property,
};
use dim_agent_types::AgentEvent;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use tokio_util::sync::CancellationToken;

mod tree;
use tree::Tree;

pub(super) const SCHEMA: &[(&str, &str)] = &[
    (
        "timeline_member_nodes",
        "CREATE TABLE timeline_member_nodes (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), left_id INTEGER, right_id INTEGER, height INTEGER NOT NULL CHECK(height BETWEEN 1 AND 64))",
    ),
    (
        "timeline_member_roots",
        "CREATE TABLE timeline_member_roots (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, group_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence >= 0), root_id INTEGER, PRIMARY KEY(session_id,branch_id,index_id,group_key,sequence))",
    ),
    (
        "timeline_member_origins",
        "CREATE TABLE timeline_member_origins (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, match_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), group_key BLOB NOT NULL, PRIMARY KEY(session_id,branch_id,index_id,match_key,sequence,group_key))",
    ),
    (
        "timeline_member_terminals",
        "CREATE TABLE timeline_member_terminals (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, match_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), PRIMARY KEY(session_id,branch_id,index_id,match_key))",
    ),
];

struct Terminal {
    kind: &'static str,
    key: &'static [&'static str],
    predicate: Option<(&'static str, &'static str)>,
    tag_prefix: Option<&'static str>,
}

enum Groups {
    None,
    Presence(&'static str, bool),
    Elements(&'static str, &'static str),
}

struct Profile {
    id: &'static str,
    origins: &'static [&'static str],
    wake: bool,
    key: &'static [&'static str],
    terminals: &'static [Terminal],
    groups: Groups,
    tuple: bool,
}

const PROFILES: &[Profile] = &[
    Profile {
        id: "input/v1",
        origins: &["input/message"],
        wake: false,
        key: &["id"],
        terminals: &[
            Terminal {
                kind: "input/consumed",
                key: &["data.input_id"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "input/deleted",
                key: &["data.input_id"],
                predicate: None,
                tag_prefix: None,
            },
        ],
        groups: Groups::Presence("data.handover", false),
        tuple: false,
    },
    Profile {
        id: "tool/v1",
        origins: &["step/tool-call"],
        wake: false,
        key: &["id"],
        terminals: &[Terminal {
            kind: "step/tool-result",
            key: &["data.call_event_id"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::Elements("data.calls", "name"),
        tuple: false,
    },
    Profile {
        id: "exec/v1",
        origins: &["exec/started"],
        wake: false,
        key: &["data.task_id"],
        terminals: &[Terminal {
            kind: "exec/ended",
            key: &["data.task_id"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::None,
        tuple: false,
    },
    Profile {
        id: "collaboration/v1",
        origins: &["collaboration/request"],
        wake: false,
        key: &["id"],
        terminals: &[
            Terminal {
                kind: "collaboration/result",
                key: &["data.request_event_id"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "collaboration/delivered",
                key: &["data.op_event_id"],
                predicate: Some(("data.status", "error")),
                tag_prefix: None,
            },
        ],
        groups: Groups::None,
        tuple: false,
    },
    Profile {
        id: "input-handover/v1",
        origins: &["input/message"],
        wake: false,
        key: &["id"],
        terminals: &[
            Terminal {
                kind: "input/consumed",
                key: &["data.input_id"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "input/deleted",
                key: &["data.input_id"],
                predicate: None,
                tag_prefix: None,
            },
        ],
        groups: Groups::Presence("data.handover", true),
        tuple: true,
    },
    Profile {
        id: "prepare/v1",
        origins: &["context/prepare"],
        wake: false,
        key: &["data.turn_id", "data.request_id"],
        terminals: &[
            Terminal {
                kind: "model/call",
                key: &["data.turn_id", "data.request_id"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "step/model-output-end",
                key: &["data.turn_id", "data.request_id"],
                predicate: None,
                tag_prefix: None,
            },
        ],
        groups: Groups::None,
        tuple: true,
    },
    Profile {
        id: "model-call/v1",
        origins: &["model/call"],
        wake: false,
        key: &["data.turn_id", "data.request_id"],
        terminals: &[Terminal {
            kind: "step/model-output-end",
            key: &["data.turn_id", "data.request_id"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::None,
        tuple: true,
    },
    Profile {
        id: "model-start/v1",
        origins: &["step/model-start"],
        wake: false,
        key: &["data.request_id"],
        terminals: &[Terminal {
            kind: "step/model-output-end",
            key: &["data.request_id"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::None,
        tuple: true,
    },
    Profile {
        id: "compaction-attempt/v1",
        origins: &["context/compaction-start"],
        wake: false,
        key: &["position.sequence"],
        terminals: &[Terminal {
            kind: "context/compaction",
            key: &["data.start"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::None,
        tuple: true,
    },
    Profile {
        id: "compaction-command/v1",
        origins: &["context/compact"],
        wake: false,
        key: &["position.sequence"],
        terminals: &[
            Terminal {
                kind: "context/compaction-start",
                key: &["data.command"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "context/compaction",
                key: &["data.command"],
                predicate: None,
                tag_prefix: None,
            },
        ],
        groups: Groups::None,
        tuple: true,
    },
    Profile {
        id: "approval/v1",
        origins: &["approval/request"],
        wake: false,
        key: &["data.approval_id"],
        terminals: &[
            Terminal {
                kind: "approval/respond",
                key: &["data.approval_id"],
                predicate: None,
                tag_prefix: None,
            },
            Terminal {
                kind: "approval/cancelled",
                key: &["data.approval_id"],
                predicate: None,
                tag_prefix: None,
            },
        ],
        groups: Groups::None,
        tuple: false,
    },
    Profile {
        id: "collaboration-delivery/v1",
        origins: &[
            "collaboration/request",
            "collaboration/cancel",
            "collaboration/close",
            "collaboration/message",
        ],
        wake: false,
        key: &["id"],
        terminals: &[Terminal {
            kind: "collaboration/delivered",
            key: &["data.op_event_id"],
            predicate: None,
            tag_prefix: None,
        }],
        groups: Groups::None,
        tuple: false,
    },
    Profile {
        id: "exec-wake/v1",
        origins: &["exec/ended"],
        wake: true,
        key: &["id"],
        terminals: &[Terminal {
            kind: "input/message",
            key: &["tags"],
            predicate: None,
            tag_prefix: Some("exec-wake-result:"),
        }],
        groups: Groups::None,
        tuple: false,
    },
];

fn field_in(path: &str, values: &[&str]) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values: values.iter().map(|value| (*value).into()).collect(),
    }
}

fn wake_conditions() -> Vec<QueryFilter> {
    vec![
        field_in("data.status", &["completed", "failed"]),
        QueryFilter::Not {
            condition: Box::new(QueryFilter::Exists {
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        field_in("kind", &["stop/interrupt"]),
                        field_in("data.scope", &["branch"]),
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Le,
                            right: QueryOperand::Outer {
                                path: "position.sequence".into(),
                            },
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Exists {
                                condition: Box::new(QueryFilter::And {
                                    conditions: vec![
                                        field_in(
                                            "kind",
                                            &["branch/start", "branch/resume", "session/forked"],
                                        ),
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "position.sequence".into(),
                                            },
                                            comparison: QueryComparison::Gt,
                                            right: QueryOperand::Outer {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "position.sequence".into(),
                                            },
                                            comparison: QueryComparison::Le,
                                            right: QueryOperand::Root {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                    ],
                                }),
                            }),
                        },
                    ],
                }),
            }),
        },
    ]
}

fn wake_eligible(
    connection: &Connection,
    session: &str,
    branch: &str,
    sequence: u64,
    value: &impl Fn(&str) -> Result<QueryValue>,
    tail: &[std::sync::Arc<AgentEvent>],
) -> Result<bool> {
    if !matches!(value("data.status")?, QueryValue::Value { value: Value::String(status) } if status == "completed" || status == "failed")
    {
        return Ok(false);
    }
    let mut opening = 0i64;
    for kind in ["branch/start", "branch/resume", "session/forked"] {
        let position: Option<i64> = connection.prepare_cached("SELECT sequence FROM timeline_events WHERE session_id=?1 AND branch_id=?2 AND kind=?3 AND sequence<=?4 ORDER BY sequence DESC LIMIT 1")
            .map_err(error)?.query_row(params![session,branch,kind,sequence as i64], |row| row.get(0)).optional().map_err(error)?;
        opening = opening.max(position.unwrap_or(0));
    }
    let mut closing: i64 = connection.prepare_cached("SELECT coalesce(max(sequence),0) FROM timeline_events WHERE session_id=?1 AND branch_id=?2 AND kind='stop/interrupt' AND json_extract(body,'$.record.event.data.scope')='branch' AND sequence<=?3")
        .map_err(error)?.query_row(params![session,branch,sequence as i64], |row| row.get(0)).map_err(error)?;
    for event in tail.iter().filter(|event| event.sequence <= sequence) {
        match &event.kind {
            dim_agent_types::EventKind::BranchStart(_)
            | dim_agent_types::EventKind::BranchResume(_)
            | dim_agent_types::EventKind::SessionForked(_) => {
                opening = opening.max(event.sequence as i64)
            }
            dim_agent_types::EventKind::StopInterrupt(data)
                if data.scope == dim_agent_types::InterruptScope::Branch =>
            {
                closing = closing.max(event.sequence as i64)
            }
            _ => {}
        }
    }
    Ok(closing == 0 || opening > closing)
}

fn terminal_keys(
    terminal: &Terminal,
    tuple: bool,
    value: &impl Fn(&str) -> Result<QueryValue>,
) -> Result<Vec<Vec<u8>>> {
    if let Some(prefix) = terminal.tag_prefix {
        let QueryValue::Value {
            value: Value::Array(tags),
        } = value(terminal.key[0])?
        else {
            return Ok(Vec::new());
        };
        tags.iter()
            .filter_map(|tag| tag.as_str().and_then(|tag| tag.strip_prefix(prefix)))
            .map(|id| (QueryValue::Value { value: id.into() }).scalar_key())
            .collect()
    } else {
        Ok(key(terminal.key, tuple, value)?.into_iter().collect())
    }
}

pub(super) fn branch(connection: &Connection, session: &str, branch: &str) -> Result<()> {
    for profile in PROFILES {
        connection
            .prepare_cached(
                "INSERT OR IGNORE INTO timeline_member_roots VALUES(?1,?2,?3,?4,0,NULL)",
            )
            .map_err(error)?
            .execute(params![session, branch, profile.id, &[0u8][..]])
            .map_err(error)?;
    }
    Ok(())
}

fn match_key(value: QueryValue) -> Result<Option<Vec<u8>>> {
    match value {
        QueryValue::Missing => Ok(None),
        value => value.scalar_key().map(Some),
    }
}

fn key(
    fields: &[&str],
    tuple: bool,
    value: &impl Fn(&str) -> Result<QueryValue>,
) -> Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    if tuple {
        result.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    }
    for field in fields {
        let Some(key) = match_key(value(field)?)? else {
            return Ok(None);
        };
        if tuple {
            result.extend_from_slice(&(key.len() as u64).to_be_bytes());
        }
        result.extend_from_slice(&key);
    }
    Ok(Some(result))
}

fn groups(profile: &Profile, value: &impl Fn(&str) -> Result<QueryValue>) -> Result<Vec<Vec<u8>>> {
    let mut result = Vec::new();
    match profile.groups {
        Groups::None => result.push(vec![0]),
        Groups::Presence(field, present) => {
            if !matches!(value(field)?, QueryValue::Missing) == present {
                result.push(vec![1, u8::from(present)]);
            }
        }
        Groups::Elements(field, path) => {
            let QueryValue::Elements { values } = value(&format!("{field}[].{path}"))? else {
                return Err(error("member group source is not an array"));
            };
            for value in values {
                if let Some(mut key) = match_key(value)? {
                    key.insert(0, 2);
                    if !result.contains(&key) {
                        result.push(key);
                    }
                }
            }
        }
    }
    Ok(result)
}

fn root(
    connection: &Connection,
    session: &str,
    branch: &str,
    profile: &Profile,
    group: &[u8],
    through: i64,
    #[cfg(feature = "testing")] statistics: Option<&super::query::Statistics>,
) -> Result<Option<i64>> {
    let sql = "SELECT root_id FROM timeline_member_roots WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND group_key=?4 AND sequence<=?5 ORDER BY sequence DESC LIMIT 1";
    let mut statement = connection.prepare_cached(sql).map_err(error)?;
    #[cfg(feature = "testing")]
    if statistics.is_some() {
        for status in [
            rusqlite::StatementStatus::VmStep,
            rusqlite::StatementStatus::FullscanStep,
            rusqlite::StatementStatus::Sort,
            rusqlite::StatementStatus::AutoIndex,
        ] {
            statement.reset_status(status);
        }
    }
    let version = statement
        .query_row(
            params![session, branch, profile.id, group, through],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(error)?;
    #[cfg(feature = "testing")]
    if let Some(statistics) = statistics {
        super::query::record_statement(
            statistics,
            connection,
            &statement,
            sql,
            &[
                session.to_owned().into(),
                branch.to_owned().into(),
                profile.id.to_owned().into(),
                group.to_vec().into(),
                through.into(),
            ],
            "member_root",
        )?;
    }
    if version.is_none() {
        if group == [0] {
            return Err(error("member index initialization is missing"));
        }
        root(
            connection,
            session,
            branch,
            profile,
            &[0],
            0,
            #[cfg(feature = "testing")]
            statistics,
        )?;
    }
    Ok(version.flatten())
}

fn save_root(
    connection: &Connection,
    session: &str,
    branch: &str,
    profile: &Profile,
    group: &[u8],
    sequence: i64,
    root: Option<i64>,
) -> Result<()> {
    connection.prepare_cached("INSERT INTO timeline_member_roots VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_id,branch_id,index_id,group_key,sequence) DO UPDATE SET root_id=excluded.root_id")
        .map_err(error)?.execute(params![session, branch, profile.id, group, sequence, root]).map_err(error)?;
    Ok(())
}

pub(super) fn event(
    connection: &Connection,
    session: &str,
    branch: &str,
    event: &AgentEvent,
) -> Result<()> {
    event_profiles(
        connection,
        session,
        branch,
        event.kind.key(),
        event.sequence,
        &|path| event_property(event, branch, path),
        PROFILES,
    )
}

#[allow(clippy::too_many_arguments)]
fn event_profiles(
    connection: &Connection,
    session: &str,
    branch: &str,
    kind: &str,
    sequence: u64,
    value: &impl Fn(&str) -> Result<QueryValue>,
    profiles: &[Profile],
) -> Result<()> {
    let sequence = i64::try_from(sequence).map_err(error)?;
    let tree = Tree {
        connection,
        session,
        #[cfg(feature = "testing")]
        statistics: None,
    };
    for profile in profiles {
        if profile.origins.contains(&kind) {
            if profile.wake
                && !wake_eligible(connection, session, branch, sequence as u64, value, &[])?
            {
                continue;
            }
            let groups = groups(profile, value)?;
            if groups.is_empty() {
                continue;
            }
            let Some(key) = key(profile.key, profile.tuple, value)? else {
                continue;
            };
            let closed = connection.prepare_cached("SELECT 1 FROM timeline_member_terminals WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND match_key=?4")
                .map_err(error)?.exists(params![session, branch, profile.id, key]).map_err(error)?;
            if closed {
                if profile.id == "exec/v1" {
                    for group in groups {
                        connection
                            .prepare_cached(
                                "INSERT INTO timeline_member_origins VALUES(?1,?2,?3,?4,?5,?6)",
                            )
                            .map_err(error)?
                            .execute(params![session, branch, profile.id, key, sequence, group])
                            .map_err(error)?;
                    }
                }
                continue;
            }
            let mut changes = BTreeMap::new();
            for group in groups {
                let previous = root(
                    connection,
                    session,
                    branch,
                    profile,
                    &group,
                    sequence,
                    #[cfg(feature = "testing")]
                    None,
                )?;
                let next = match changes.get(&previous) {
                    Some(next) => *next,
                    None => {
                        let next = tree.insert(previous, sequence)?;
                        changes.insert(previous, next);
                        next
                    }
                };
                save_root(
                    connection,
                    session,
                    branch,
                    profile,
                    &group,
                    sequence,
                    Some(next),
                )?;
                connection
                    .prepare_cached("INSERT INTO timeline_member_origins VALUES(?1,?2,?3,?4,?5,?6)")
                    .map_err(error)?
                    .execute(params![session, branch, profile.id, key, sequence, group])
                    .map_err(error)?;
            }
        }
        for terminal in profile
            .terminals
            .iter()
            .filter(|terminal| terminal.kind == kind)
        {
            if let Some((field, expected)) = terminal.predicate
                && value(field)?
                    != (QueryValue::Value {
                        value: expected.into(),
                    })
            {
                continue;
            }
            for key in terminal_keys(terminal, profile.tuple, value)? {
                connection.prepare_cached("INSERT INTO timeline_member_terminals VALUES(?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING")
                .map_err(error)?.execute(params![session, branch, profile.id, key, sequence]).map_err(error)?;
                let origins = connection.prepare_cached("SELECT sequence,group_key FROM timeline_member_origins WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND match_key=?4")
                .map_err(error)?.query_map(params![session, branch, profile.id, key], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))
                .map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
                let mut changes = BTreeMap::new();
                for (origin, group) in origins {
                    let previous = root(
                        connection,
                        session,
                        branch,
                        profile,
                        &group,
                        sequence,
                        #[cfg(feature = "testing")]
                        None,
                    )?;
                    let next = match changes.get(&(previous, origin)) {
                        Some(next) => *next,
                        None => {
                            let next = tree.remove(previous, origin)?;
                            changes.insert((previous, origin), next);
                            next
                        }
                    };
                    save_root(connection, session, branch, profile, &group, sequence, next)?;
                }
                if profile.id != "exec/v1" {
                    connection.prepare_cached("DELETE FROM timeline_member_origins WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND match_key=?4")
                    .map_err(error)?.execute(params![session, branch, profile.id, key]).map_err(error)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn row(
    connection: &Connection,
    session: &str,
    branch: &str,
    kind: &str,
    encoded: &str,
) -> Result<()> {
    if !PROFILES.iter().any(|profile| {
        profile.origins.contains(&kind)
            || profile
                .terminals
                .iter()
                .any(|terminal| terminal.kind == kind)
    }) {
        return Ok(());
    }
    let super::timeline::Row::Event { record } = serde_json::from_str(encoded).map_err(error)?
    else {
        return Err(error("member origin or terminal has an output encoding"));
    };
    raw_event(connection, session, branch, kind, &record.event, PROFILES)
}

fn raw_event(
    connection: &Connection,
    session: &str,
    branch: &str,
    kind: &str,
    event: &Value,
    profiles: &[Profile],
) -> Result<()> {
    let sequence = event
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| error("member event has no sequence"))?;
    let actual = format!(
        "{}/{}",
        event["type"].as_str().unwrap_or_default(),
        event["name"].as_str().unwrap_or_default()
    );
    if actual != kind {
        return Err(error("member event kind disagrees with its stored kind"));
    }
    event_profiles(
        connection,
        session,
        branch,
        kind,
        sequence,
        &|path| {
            if path == "position.sequence" {
                return Ok(QueryValue::Value {
                    value: sequence.into(),
                });
            }
            project_property(event, path)
        },
        profiles,
    )
}

pub(super) fn backfill_v40(connection: &Connection) -> Result<()> {
    backfill_profiles(connection, &PROFILES[4..10])
}

pub(super) fn backfill_v42(connection: &Connection) -> Result<()> {
    backfill_profiles(connection, &PROFILES[10..])?;
    connection.execute("INSERT OR IGNORE INTO timeline_member_origins SELECT session_id,branch_id,'exec/v1',query_scalar_key(body -> '$.record.event.data.task_id'),sequence,x'00' FROM timeline_events WHERE kind='exec/started'", []).map_err(error)?;
    Ok(())
}

fn backfill_profiles(connection: &Connection, profiles: &[Profile]) -> Result<()> {
    let mut branches = connection
        .prepare("SELECT session_id,branch_id FROM timeline_branches")
        .map_err(error)?;
    let mut rows = branches.query([]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        for profile in profiles {
            connection
                .execute(
                    "INSERT OR IGNORE INTO timeline_member_roots VALUES(?1,?2,?3,?4,0,NULL)",
                    params![
                        row.get::<_, String>(0).map_err(error)?,
                        row.get::<_, String>(1).map_err(error)?,
                        profile.id,
                        &[0u8][..]
                    ],
                )
                .map_err(error)?;
        }
    }
    let kinds = profiles
        .iter()
        .flat_map(|profile| {
            profile
                .origins
                .iter()
                .copied()
                .chain(profile.terminals.iter().map(|terminal| terminal.kind))
        })
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = connection.prepare(&format!("SELECT session_id,branch_id,body,kind FROM timeline_events WHERE kind IN ({kinds}) ORDER BY session_id,branch_id,sequence")).map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        let body: String = row.get(2).map_err(error)?;
        let super::timeline::Row::Event { record } = serde_json::from_str(&body).map_err(error)?
        else {
            return Err(error("a member fact has output encoding"));
        };
        raw_event(
            connection,
            &row.get::<_, String>(0).map_err(error)?,
            &row.get::<_, String>(1).map_err(error)?,
            &row.get::<_, String>(3).map_err(error)?,
            &record.event,
            profiles,
        )?;
    }
    Ok(())
}

pub(super) fn backfill(connection: &Connection) -> Result<()> {
    backfill_profiles(connection, &PROFILES[..10])
}

/// Builds member indexes for imported events inside their insertion transaction.
/// The Session's fact rows must not already have been indexed.
pub fn initialize_session_member_index(connection: &Connection, session: &str) -> Result<()> {
    let mut branches = connection
        .prepare("SELECT session_id,branch_id FROM timeline_branches WHERE session_id=?1")
        .map_err(error)?;
    let mut rows = branches.query([session]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        branch(
            connection,
            &row.get::<_, String>(0).map_err(error)?,
            &row.get::<_, String>(1).map_err(error)?,
        )?;
    }
    let kinds = PROFILES
        .iter()
        .flat_map(|profile| {
            profile
                .origins
                .iter()
                .copied()
                .chain(profile.terminals.iter().map(|terminal| terminal.kind))
        })
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = connection.prepare(&format!("SELECT session_id,branch_id,kind,body FROM timeline_events WHERE kind IN ({kinds}) AND session_id=?1 ORDER BY session_id,branch_id,sequence")).map_err(error)?;
    let mut rows = statement.query([session]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        self::row(
            connection,
            &row.get::<_, String>(0).map_err(error)?,
            &row.get::<_, String>(1).map_err(error)?,
            &row.get::<_, String>(2).map_err(error)?,
            &row.get::<_, String>(3).map_err(error)?,
        )?;
    }
    Ok(())
}

fn terms<'a>(filter: &'a QueryFilter, result: &mut Vec<&'a QueryFilter>) {
    if let QueryFilter::And { conditions } = filter {
        for condition in conditions {
            terms(condition, result);
        }
    } else {
        result.push(filter);
    }
}

fn values<'a>(filter: &'a QueryFilter, field: &str) -> Option<Vec<&'a Value>> {
    match filter {
        QueryFilter::In {
            value: QueryOperand::Field { path },
            values,
        } if path == field => Some(values.iter().collect()),
        QueryFilter::Compare {
            left: QueryOperand::Field { path },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        }
        | QueryFilter::Compare {
            right: QueryOperand::Field { path },
            comparison: QueryComparison::Eq,
            left: QueryOperand::Value { value },
        } if path == field => Some(vec![value]),
        _ => None,
    }
}

fn closes(profile: &Profile, filter: &QueryFilter, seen: &mut Vec<&'static str>) -> bool {
    if let QueryFilter::Or { conditions } = filter {
        return conditions
            .iter()
            .all(|condition| closes(profile, condition, seen));
    }
    if let [
        Terminal {
            kind,
            key: [field],
            tag_prefix: Some(prefix),
            ..
        },
    ] = profile.terminals
    {
        return filter
            == &QueryFilter::And {
                conditions: vec![
                    field_in("kind", &[*kind]),
                    QueryFilter::Any {
                        field: (*field).into(),
                        condition: Box::new(QueryFilter::Compare {
                            left: QueryOperand::Element {
                                path: String::new(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Concat {
                                parts: vec![
                                    QueryOperand::Value {
                                        value: (*prefix).into(),
                                    },
                                    QueryOperand::Outer { path: "id".into() },
                                ],
                            },
                        }),
                    },
                ],
            }
            && {
                seen.push(kind);
                true
            };
    }
    let mut conditions = Vec::new();
    terms(filter, &mut conditions);
    let Some((at, kinds)) = conditions
        .iter()
        .enumerate()
        .find_map(|(at, condition)| values(condition, "kind").map(|kinds| (at, kinds)))
    else {
        return false;
    };
    conditions.remove(at);
    if kinds.is_empty() {
        return false;
    }
    for kind in kinds {
        let Some(terminal) = profile
            .terminals
            .iter()
            .find(|terminal| kind.as_str() == Some(terminal.kind))
        else {
            return false;
        };
        let mut joined = Vec::new();
        let mut predicate = terminal.predicate.is_none();
        for condition in &conditions {
            match condition {
                QueryFilter::Compare {
                    left: QueryOperand::Field { path: inner },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Outer { path: outer },
                }
                | QueryFilter::Compare {
                    right: QueryOperand::Field { path: inner },
                    comparison: QueryComparison::Eq,
                    left: QueryOperand::Outer { path: outer },
                } if terminal
                    .key
                    .iter()
                    .zip(profile.key)
                    .any(|(a, b)| inner == a && outer == b)
                    && !joined.contains(&(inner.as_str(), outer.as_str())) =>
                {
                    joined.push((inner.as_str(), outer.as_str()))
                }
                condition
                    if terminal.predicate.is_some_and(|(field, expected)| {
                        values(condition, field).is_some_and(|values| {
                            values.len() == 1 && values[0].as_str() == Some(expected)
                        })
                    }) && !predicate =>
                {
                    predicate = true
                }
                _ => return false,
            }
        }
        if joined.len() != profile.key.len() || !predicate {
            return false;
        }
        if !seen.contains(&terminal.kind) {
            seen.push(terminal.kind);
        }
    }
    true
}

fn group(profile: &Profile, filter: &QueryFilter) -> Result<Option<Vec<u8>>> {
    Ok(match (&profile.groups, filter) {
        (Groups::Presence(field, false), QueryFilter::Not { condition }) if matches!(condition.as_ref(), QueryFilter::Present { value: QueryOperand::Field { path } } if path == field) => {
            Some(vec![1, 0])
        }
        (
            Groups::Presence(field, true),
            QueryFilter::Present {
                value: QueryOperand::Field { path },
            },
        ) if path == field => Some(vec![1, 1]),
        (
            Groups::Elements(field, name),
            QueryFilter::Any {
                field: array,
                condition,
            },
        ) if array == field => {
            if let QueryFilter::Compare {
                left: QueryOperand::Element { path },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value { value },
            }
            | QueryFilter::Compare {
                right: QueryOperand::Element { path },
                comparison: QueryComparison::Eq,
                left: QueryOperand::Value { value },
            } = condition.as_ref()
                && path == name
            {
                let mut key = QueryValue::Value {
                    value: value.clone(),
                }
                .scalar_key()?;
                key.insert(0, 2);
                Some(key)
            } else {
                None
            }
        }
        _ => None,
    })
}

pub(super) struct Candidates<'a> {
    pub terminals: Vec<&'a QueryFilter>,
    pub positions: Vec<u64>,
}

#[cfg_attr(feature = "testing", allow(clippy::too_many_arguments))]
pub(super) fn candidates<'a>(
    connection: &Connection,
    scope: &TimelineReadScope,
    query: &'a QuerySpec,
    limit: usize,
    after: Option<(u64, u64, bool)>,
    cancel: &CancellationToken,
    #[cfg(feature = "testing")] statistics: &super::query::Statistics,
) -> Result<Option<Candidates<'a>>> {
    if scope.segments.len() != 1 {
        return Ok(None);
    }
    if !query.group_by.is_empty()
        || !matches!(
            query.select,
            QuerySelect::Fields { .. } | QuerySelect::Events
        )
        || !matches!(query.order_by.as_slice(), [] | [_])
        || query
            .order_by
            .first()
            .is_some_and(|order| order.key != QueryOrderKey::Position)
    {
        return Ok(None);
    }
    let Some(filter) = &query.filter else {
        return Ok(None);
    };
    candidates_for(
        connection,
        scope,
        query,
        filter,
        limit,
        after,
        cancel,
        #[cfg(feature = "testing")]
        statistics,
    )
}

#[cfg_attr(feature = "testing", allow(clippy::too_many_arguments))]
fn candidates_for<'a>(
    connection: &Connection,
    scope: &TimelineReadScope,
    query: &QuerySpec,
    filter: &'a QueryFilter,
    limit: usize,
    after: Option<(u64, u64, bool)>,
    cancel: &CancellationToken,
    #[cfg(feature = "testing")] statistics: &super::query::Statistics,
) -> Result<Option<Candidates<'a>>> {
    let [segment] = scope.segments.as_slice() else {
        return Ok(None);
    };
    if let QueryFilter::Or { conditions } = filter {
        let mut positions = BTreeSet::new();
        let mut terminals = Vec::new();
        for condition in conditions {
            let Some(found) = candidates_for(
                connection,
                scope,
                query,
                condition,
                limit,
                after,
                cancel,
                #[cfg(feature = "testing")]
                statistics,
            )?
            else {
                return Ok(None);
            };
            positions.extend(found.positions);
            terminals.extend(found.terminals);
        }
        return Ok(Some(Candidates {
            terminals,
            positions: positions.into_iter().collect(),
        }));
    }
    let mut conditions = Vec::new();
    terms(filter, &mut conditions);
    if after.is_none()
        && query.pick == Some(dim_agent_runtime::QueryPick::First)
        && conditions.len() == 2
        && let Some(kinds) = conditions
            .iter()
            .find_map(|condition| values(condition, "kind"))
        && kinds.len() == 1
        && let Some(kind) = kinds[0]
            .as_str()
            .filter(|kind| matches!(*kind, "exec/started" | "exec/ended"))
        && let Some(ids) = conditions
            .iter()
            .find_map(|condition| values(condition, "data.task_id"))
        && let [Value::String(id)] = ids.as_slice()
    {
        let key = QueryValue::Value {
            value: (*id).clone().into(),
        }
        .scalar_key()?;
        let sql = if kind == "exec/started" {
            "SELECT sequence FROM timeline_member_origins WHERE session_id=?1 AND branch_id=?2 AND index_id='exec/v1' AND match_key=?3 AND sequence>=?4 AND sequence<=?5 ORDER BY sequence LIMIT 1"
        } else {
            "SELECT sequence FROM timeline_member_terminals WHERE session_id=?1 AND branch_id=?2 AND index_id='exec/v1' AND match_key=?3 AND sequence<=?5 ORDER BY sequence LIMIT 1"
        };
        let parameters = vec![
            scope.key.session_id.clone().into(),
            segment.branch_id.clone().into(),
            rusqlite::types::Value::Blob(key),
            (segment.from as i64).into(),
            (segment.through.min(segment.stored_through) as i64).into(),
        ];
        let mut statement = connection.prepare_cached(sql).map_err(error)?;
        #[cfg(feature = "testing")]
        for status in [
            rusqlite::StatementStatus::VmStep,
            rusqlite::StatementStatus::FullscanStep,
            rusqlite::StatementStatus::Sort,
            rusqlite::StatementStatus::AutoIndex,
        ] {
            statement.reset_status(status);
        }
        let mut positions: Vec<u64> = statement
            .query_row(rusqlite::params_from_iter(&parameters), |row| {
                row.get::<_, i64>(0)
            })
            .optional()
            .map_err(error)?
            .into_iter()
            .map(|sequence| sequence as u64)
            .collect();
        #[cfg(feature = "testing")]
        super::query::record_statement(
            statistics,
            connection,
            &statement,
            sql,
            &parameters,
            "member_identity",
        )?;
        if kind == "exec/ended"
            && positions
                .first()
                .is_some_and(|sequence| *sequence < segment.from)
        {
            return Ok(None);
        }
        for event in segment.tail.iter().filter(|event| {
            event.sequence > segment.stored_through
                && event.sequence >= segment.from
                && event.sequence <= segment.through
                && event.kind.key() == kind
        }) {
            if event_property(event, &segment.branch_id, "data.task_id")?
                == (QueryValue::Value {
                    value: (*id).clone().into(),
                })
            {
                positions.push(event.sequence);
            }
        }
        positions.sort_unstable();
        positions.truncate(1);
        return Ok(Some(Candidates {
            terminals: vec![],
            positions,
        }));
    }
    for profile in PROFILES {
        let mut origin = false;
        let mut terminal = None;
        let mut grouped = None;
        let mut residual = false;
        let expected = if profile.wake {
            wake_conditions()
        } else {
            Vec::new()
        };
        if !expected.iter().all(|term| conditions.contains(&term)) {
            continue;
        }
        for condition in &conditions {
            if expected.contains(condition) {
                continue;
            }
            if values(condition, "kind").is_some_and(|values| {
                !values.is_empty()
                    && values.iter().all(|value| {
                        value
                            .as_str()
                            .is_some_and(|kind| profile.origins.contains(&kind))
                    })
            }) && !origin
            {
                origin = true;
                if values(condition, "kind")
                    .is_some_and(|kinds| kinds.len() != profile.origins.len())
                {
                    residual = true;
                }
                continue;
            }
            if let QueryFilter::Not { condition } = condition
                && let QueryFilter::Exists { condition } = condition.as_ref()
                && terminal.is_none()
            {
                let mut seen = Vec::new();
                if closes(profile, condition, &mut seen) && seen.len() == profile.terminals.len() {
                    terminal = Some(condition.as_ref());
                    continue;
                }
            }
            if grouped.is_none()
                && let Some(key) = group(profile, condition)?
            {
                grouped = Some(key);
                continue;
            }
            residual = true;
        }
        if !origin || (matches!(profile.groups, Groups::Presence(..)) && grouped.is_none()) {
            continue;
        }
        let Some(terminal) = terminal else { continue };
        let mut known_absent = vec![terminal];
        for condition in &conditions {
            if expected.contains(condition)
                && let QueryFilter::Not { condition } = condition
                && let QueryFilter::Exists { condition } = condition.as_ref()
            {
                known_absent.push(condition.as_ref());
            }
        }
        let descending = match query.pick {
            Some(dim_agent_runtime::QueryPick::Last) => true,
            Some(dim_agent_runtime::QueryPick::First) => false,
            None => query
                .order_by
                .first()
                .is_some_and(|order| order.direction == TimelineQueryDirection::Descending),
        };
        let through = i64::try_from(segment.through.min(segment.stored_through)).map_err(error)?;
        let mut low = i64::try_from(segment.from).map_err(error)?;
        let mut high = through;
        if let Some((ordinal, sequence, _)) = after {
            if ordinal != 0 {
                return Ok(Some(Candidates {
                    terminals: known_absent,
                    positions: Vec::new(),
                }));
            }
            let sequence = i64::try_from(sequence).map_err(error)?;
            if descending {
                high = high.min(sequence.saturating_sub(1));
            } else {
                low = low.max(sequence.saturating_add(1));
            }
        }
        let all_groups = grouped.is_none() && matches!(profile.groups, Groups::Elements(..));
        let mut selected_groups = if all_groups {
            Vec::new()
        } else {
            vec![grouped.unwrap_or_else(|| vec![0])]
        };
        if all_groups {
            let mut previous = vec![2u8];
            loop {
                let sql = "SELECT group_key FROM timeline_member_roots WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND group_key>?4 AND group_key<?5 ORDER BY group_key LIMIT 1";
                let mut statement = connection.prepare_cached(sql).map_err(error)?;
                #[cfg(feature = "testing")]
                for status in [
                    rusqlite::StatementStatus::VmStep,
                    rusqlite::StatementStatus::FullscanStep,
                    rusqlite::StatementStatus::Sort,
                    rusqlite::StatementStatus::AutoIndex,
                ] {
                    statement.reset_status(status);
                }
                let parameters = rusqlite::params![
                    scope.key.session_id,
                    segment.branch_id,
                    profile.id,
                    previous,
                    &[3u8][..]
                ];
                let next: Option<Vec<u8>> = statement
                    .query_row(parameters, |row| row.get(0))
                    .optional()
                    .map_err(error)?;
                #[cfg(feature = "testing")]
                super::query::record_statement(
                    statistics,
                    connection,
                    &statement,
                    sql,
                    &[
                        scope.key.session_id.clone().into(),
                        segment.branch_id.clone().into(),
                        profile.id.to_owned().into(),
                        previous.clone().into(),
                        vec![3u8].into(),
                    ],
                    "member_group",
                )?;
                let Some(next) = next else { break };
                previous = next.clone();
                selected_groups.push(next);
            }
        }
        let tree = Tree {
            connection,
            session: &scope.key.session_id,
            #[cfg(feature = "testing")]
            statistics: Some(statistics),
        };
        let mut closed = BTreeSet::new();
        let mut opened = Vec::new();
        for event in segment.tail.iter().filter(|event| {
            event.sequence > segment.stored_through && event.sequence <= segment.through
        }) {
            let value = |path: &str| event_property(event, &segment.branch_id, path);
            for terminal in profile
                .terminals
                .iter()
                .filter(|terminal| terminal.kind == event.kind.key())
            {
                if let Some((field, expected)) = terminal.predicate
                    && value(field)?
                        != (QueryValue::Value {
                            value: expected.into(),
                        })
                {
                    continue;
                }
                closed.extend(terminal_keys(terminal, profile.tuple, &value)?);
            }
            if profile.origins.contains(&event.kind.key())
                && (!profile.wake
                    || wake_eligible(
                        connection,
                        &scope.key.session_id,
                        &segment.branch_id,
                        event.sequence,
                        &value,
                        &segment.tail,
                    )?)
                && (all_groups
                    || groups(profile, &value)?
                        .iter()
                        .any(|group| selected_groups.contains(group)))
                && let Some(key) = key(profile.key, profile.tuple, &value)?
            {
                opened.push((event.sequence, key));
            }
        }
        let page_size = if residual { 128 } else { limit.max(1) };
        let mut positions = Vec::new();
        for group in &selected_groups {
            let root = root(
                connection,
                &scope.key.session_id,
                &segment.branch_id,
                profile,
                group,
                through,
                #[cfg(feature = "testing")]
                Some(statistics),
            )?;
            let (mut low, mut high) = (low, high);
            let before = positions.len();
            while low <= high {
                let members = tree.page(root, low, high, descending, page_size, cancel)?;
                let Some(last) = members.last().copied() else {
                    break;
                };
                for sequence in &members {
                    if closed.is_empty()
                        || stored_key(
                            connection,
                            &scope.key.session_id,
                            &segment.branch_id,
                            profile,
                            *sequence,
                        )?
                        .is_none_or(|key| !closed.contains(&key))
                    {
                        positions.push(*sequence as u64);
                    }
                }
                if members.len() < page_size || (!residual && positions.len() - before >= limit) {
                    break;
                }
                if descending {
                    high = last.saturating_sub(1);
                } else {
                    low = last.saturating_add(1);
                }
            }
        }
        for (sequence, key) in opened {
            if closed.contains(&key) {
                continue;
            }
            let is_closed=connection.prepare_cached("SELECT 1 FROM timeline_member_terminals WHERE session_id=?1 AND branch_id=?2 AND index_id=?3 AND match_key=?4 AND sequence<=?5").map_err(error)?.exists(params![scope.key.session_id,segment.branch_id,profile.id,key,through]).map_err(error)?;
            if !is_closed
                && sequence >= segment.from
                && after.is_none_or(|(ordinal, at, _)| {
                    ordinal == 0
                        && if descending {
                            sequence < at
                        } else {
                            sequence > at
                        }
                })
            {
                positions.push(sequence);
            }
        }
        positions.sort_unstable();
        positions.dedup();
        if descending {
            positions.reverse();
        }
        if !residual {
            positions.truncate(limit);
        }
        return Ok(Some(Candidates {
            terminals: known_absent,
            positions,
        }));
    }
    Ok(None)
}

fn stored_key(
    connection: &Connection,
    session: &str,
    branch: &str,
    profile: &Profile,
    sequence: i64,
) -> Result<Option<Vec<u8>>> {
    let columns = profile
        .key
        .iter()
        .map(|field| match *field {
            "id" => "json_array('text',event_id)".to_owned(),
            "position.sequence" => "json_array('integer',sequence)".to_owned(),
            field => {
                let path = format!("$.record.event.{field}");
                format!("json_array(json_type(body,'{path}'),json_extract(body,'{path}'))")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let encoded:String=connection.query_row(&format!("SELECT json_array({columns}) FROM timeline_events WHERE session_id=?1 AND branch_id=?2 AND sequence=?3"),params![session,branch,sequence],|row|row.get(0)).map_err(error)?;
    let parts: Vec<(Option<String>, Value)> = serde_json::from_str(&encoded).map_err(error)?;
    key(profile.key, profile.tuple, &|field| {
        let index = profile
            .key
            .iter()
            .position(|candidate| *candidate == field)
            .expect("declared member key");
        Ok(match &parts[index] {
            (None, _) => QueryValue::Missing,
            (Some(_), value) => QueryValue::Value {
                value: value.clone(),
            },
        })
    })
}
