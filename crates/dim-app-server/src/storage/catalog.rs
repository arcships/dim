//! SessionStore's catalog projection (#556, native storage format v22): one
//! `session_catalog` row per Session, written by its creation and by the
//! transaction that commits each `main` batch, and rebuilt from the stored
//! events by the upgrade. The catalog page owns how it is read; this module
//! owns when and what it is written.
use super::{
    codec::Row,
    sqlite::error,
    timeline::{read_sequence, sql_sequence},
};
use dim_agent_runtime::{Result, SessionConfig, StoredEvent};
use dim_agent_types::{AgentEvent, ContentBlock, EventKind, UserInput};
use rusqlite::{Connection, OptionalExtension, params};

/// The `main` events the preview window covers: its batches are read whole,
/// so the batch that holds this sequence is inside it entirely.
const PREVIEW_WINDOW: u64 = 256;
/// Characters the preview keeps before it ends in `…`.
const PREVIEW_CHARS: usize = 120;

/// The preview a user input gives `session.summaries`: its text blocks joined
/// with newlines, cut to 120 characters, `None` when the text is empty (an
/// image-only message, for example).
pub(crate) fn preview(user: &UserInput) -> Option<String> {
    let text = user
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| truncate(&text, PREVIEW_CHARS))
}

fn truncate(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

/// Writes a row for every stored Session, for the upgrade's backfill.
pub(super) fn backfill(connection: &Connection) -> Result<()> {
    let sessions = connection
        .prepare("SELECT session_id FROM agent_sessions ORDER BY session_id")
        .map_err(error)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    for session in sessions {
        backfill_session(connection, &session)?;
    }
    Ok(())
}

/// Writes `session`'s row as a backfill over its events reads it:
/// `updated_at_ms` is the `create_at` of its last committed `main` event
/// (`created_at_ms` without one) and `preview` the first user input the
/// window decides. Fork writes its row with the same rule, so the two are
/// column-identical. A Session whose stored data cannot be read still gets a
/// row, the one `agent_sessions` alone gives, and the reason is named on
/// stderr: a projection never blocks the events it projects.
pub(super) fn backfill_session(connection: &Connection, session: &str) -> Result<()> {
    let (created_at, config): (i64, String) = connection
        .query_row(
            "SELECT created_at_ms, config FROM agent_sessions WHERE session_id = ?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    match stored(connection, session, created_at, &config) {
        Ok((updated_at, preview, cwd)) => write(connection, session, updated_at, preview, &cwd),
        Err(reason) => {
            eprintln!(
                "Session {session}: the catalog row could not be read from its stored events ({reason}); it keeps the creation time and no preview."
            );
            let updated_at = u64::try_from(created_at).unwrap_or_default();
            let cwd = stored_cwd(connection, session);
            write(connection, session, updated_at, None, &cwd)
        }
    }
}

/// The row a backfill over `session`'s events gives, or why its stored data
/// could not be read.
fn stored(
    connection: &Connection,
    session: &str,
    created_at: i64,
    config: &str,
) -> std::result::Result<(u64, Option<String>, String), String> {
    let created_at =
        u64::try_from(created_at).map_err(|_| "its creation time is negative".to_string())?;
    let cwd = serde_json::from_str::<SessionConfig>(config)
        .map_err(|error| format!("its config does not decode: {error}"))?
        .cwd;
    let updated_at = last_event_at(connection, session)
        .map_err(|error| error.to_string())?
        .unwrap_or(created_at);
    let preview = decided(connection, session).map_err(|error| error.to_string())?;
    Ok((updated_at, preview, cwd))
}

/// The stored config's `cwd` read by SQLite alone, `""` when even that fails.
fn stored_cwd(connection: &Connection, session: &str) -> String {
    connection
        .query_row(
            "SELECT json_extract(config, '$.cwd') FROM agent_sessions WHERE session_id = ?1",
            [session],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default()
}

fn write(
    connection: &Connection,
    session: &str,
    updated_at: u64,
    preview: Option<String>,
    cwd: &str,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO session_catalog (session_id, updated_at_ms, preview, cwd) \
             VALUES (?1, ?2, ?3, ?4)",
            params![session, sql_sequence(updated_at)?, preview, cwd],
        )
        .map_err(error)?;
    Ok(())
}

/// Writes the row `session.create` keeps from creation: the creation time and
/// no preview.
pub(super) fn created(
    connection: &Connection,
    session: &str,
    config: &SessionConfig,
    created_at: u64,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO session_catalog (session_id, updated_at_ms, preview, cwd) \
             VALUES (?1, ?2, NULL, ?3)",
            params![session, sql_sequence(created_at)?, config.cwd],
        )
        .map_err(error)?;
    Ok(())
}

/// Writes `session`'s row in the transaction that commits one of its `main`
/// batches: `updated_at_ms` follows the batch's last event, and `preview` is
/// decided once, by the first user input the window holds, so a later batch
/// never changes it. A window that cannot be read leaves the preview unset
/// and logs a warning rather than failing the batch.
pub(super) fn committed(
    connection: &Connection,
    session: &str,
    events: &[StoredEvent],
) -> Result<()> {
    let last = events
        .last()
        .ok_or_else(|| error("a committed main batch holds no event"))?;
    let decides = events.iter().any(
        |record| matches!(&record.event.kind, EventKind::InputMessage(data) if data.user.is_some()),
    );
    let preview = if decides && undecided(connection, session)? {
        match decided(connection, session) {
            Ok(preview) => preview,
            Err(error) => {
                tracing::warn!(
                    session_id = session,
                    %error,
                    "Session catalog preview could not be read from its stored events; it stays unset"
                );
                None
            }
        }
    } else {
        None
    };
    connection
        .execute(
            "INSERT INTO session_catalog (session_id, updated_at_ms, preview, cwd) \
             SELECT s.session_id, ?2, ?3, json_extract(s.config, '$.cwd') FROM agent_sessions s \
             WHERE s.session_id = ?1 \
             ON CONFLICT (session_id) DO UPDATE SET updated_at_ms = excluded.updated_at_ms, \
             preview = COALESCE(session_catalog.preview, excluded.preview)",
            params![session, sql_sequence(last.event.create_at)?, preview],
        )
        .map_err(error)?;
    Ok(())
}

/// Whether the row has no preview yet; a Session without a row has none.
fn undecided(connection: &Connection, session: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT preview IS NULL FROM session_catalog WHERE session_id = ?1",
            [session],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or(true))
}

/// The `main` head, `None` when the Session has no `main` branch.
fn head(connection: &Connection, session: &str) -> Result<Option<u64>> {
    connection
        .query_row(
            "SELECT sequence FROM timeline_branches WHERE session_id = ?1 AND branch_id = 'main'",
            [session],
            |row| read_sequence(row, 0),
        )
        .optional()
        .map_err(error)
}

/// The `create_at` of the last committed `main` event, `None` without one.
fn last_event_at(connection: &Connection, session: &str) -> Result<Option<u64>> {
    let Some(head) = head(connection, session)?.filter(|head| *head > 0) else {
        return Ok(None);
    };
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE session_id = ?1 AND branch_id = 'main' \
             ORDER BY sequence DESC LIMIT 1",
            [session],
            |row| row.get(0),
        )
        .map_err(error)?;
    let row: Row = serde_json::from_str(&body).map_err(error)?;
    let last = row
        .wires()
        .last()
        .ok_or_else(|| error("stored row holds no event"))?;
    if last.event["sequence"].as_u64() != Some(head) {
        return Err(error("the last stored event is not the main head"));
    }
    last.event["create_at"]
        .as_u64()
        .map(Some)
        .ok_or_else(|| error("stored event has no create_at"))
}

/// The last sequence the preview window reads: the batch that holds sequence
/// 256 is read whole, so a user input just past 256 in it is inside.
fn window_end(connection: &Connection, session: &str) -> Result<u64> {
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE session_id = ?1 AND branch_id = 'main' \
             AND sequence <= ?2 AND last_sequence >= ?2 LIMIT 1",
            params![session, sql_sequence(PREVIEW_WINDOW)?],
            |row| row.get(0),
        )
        .map_err(error)?;
    let row: Row = serde_json::from_str(&body).map_err(error)?;
    row.wires()
        .iter()
        .find(|wire| wire.event["sequence"].as_u64() == Some(PREVIEW_WINDOW))
        .map(|wire| wire.batch_end)
        .ok_or_else(|| error("the batch holding the preview window's last event is missing"))
}

/// The preview the window decides: the first user input among the committed
/// `main` batches that start within the first 256 events, `None` when it
/// decides none or there is no user input at all. The decision is final,
/// including a decision of `None`, because the window only grows towards a
/// fixed end.
fn decided(connection: &Connection, session: &str) -> Result<Option<String>> {
    let head = head(connection, session)?.unwrap_or(0);
    if head == 0 {
        return Ok(None);
    }
    let end = if head <= PREVIEW_WINDOW {
        head
    } else {
        window_end(connection, session)?
    };
    let mut query = connection
        .prepare_cached(
            "SELECT body FROM timeline_events WHERE session_id = ?1 AND branch_id = 'main' \
             AND kind = 'input/message' AND sequence <= ?2 ORDER BY sequence",
        )
        .map_err(error)?;
    let bodies = query
        .query_map(params![session, sql_sequence(end)?], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?;
    for body in bodies {
        let row: Row = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        let Row::Event { record } = row else {
            return Err(error("an input message row is an output row"));
        };
        let event: AgentEvent = serde_json::from_value(record.event).map_err(error)?;
        let EventKind::InputMessage(data) = event.kind else {
            return Err(error("an input message row holds another kind"));
        };
        if let Some(user) = &data.user {
            return Ok(preview(user));
        }
    }
    Ok(None)
}
