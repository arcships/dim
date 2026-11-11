//! The Timeline export (ADR 005 D3): one Session's whole Timeline as JSONL,
//! read through this module's Timeline reader from one read-only snapshot.
//! Lines and fields: docs/design/appserver/timeline-export.md §5.
use super::{
    SqliteStorage,
    codec::{CONTENT_BUDGET, Reader},
    sqlite::error,
    timeline::{read_directory, read_range},
};
use dim_agent_runtime::{BranchRecord, Result, TimelineKey, TimelineTrace};
use dim_agent_types::AgentEvent;
use serde::Serialize;
use std::{io::Write, path::PathBuf, sync::Arc};

/// The export version in the header; it rises only when a line kind or an
/// envelope field is removed or changes meaning.
const EXPORT_VERSION: u32 = 1;
/// Events per read call.
const PAGE: u64 = 256;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Line<'a> {
    Header {
        export: u32,
        session_id: &'a str,
        store_format: u32,
        sensitive: bool,
    },
    Branch {
        info: &'a BranchRecord,
        head: u64,
    },
    Event {
        branch_id: &'a str,
        event: &'a AgentEvent,
        trace: Option<&'a TimelineTrace>,
        batch_end: u64,
    },
    End {
        branches: usize,
        events: u64,
    },
}

/// Writes `session`'s whole Timeline in the database at `path` to `out`,
/// without changing the database. A failed read leaves an incomplete stream
/// without its terminal record.
pub async fn export_timeline(path: PathBuf, session: &str, out: &mut impl Write) -> Result<()> {
    let (db, format) = SqliteStorage::open_read_only(path)?;
    let written = write(&db, format, session, out).await;
    let closed = db.close().await;
    written.and(closed)
}

async fn write(
    db: &Arc<SqliteStorage>,
    format: u32,
    session: &str,
    out: &mut impl Write,
) -> Result<()> {
    let id = session.to_owned();
    let exists = db
        .run(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id = ?1)",
                    [&id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(error)
        })
        .await?;
    if !exists {
        return Err(error(format!(
            "Session {session} is not in {}",
            db.path.display()
        )));
    }
    let mut reader = Reader::new(CONTENT_BUDGET);
    let branches = read_directory(db, session).await?;
    line(
        out,
        &Line::Header {
            export: EXPORT_VERSION,
            session_id: session,
            store_format: format,
            sensitive: true,
        },
    )?;
    for branch in &branches {
        line(
            out,
            &Line::Branch {
                info: &branch.info,
                head: branch.sequence,
            },
        )?;
    }
    let mut events = 0;
    for branch in &branches {
        let key = TimelineKey {
            session_id: session.to_owned(),
            branch_id: branch.info.id.clone(),
        };
        let mut next = 1;
        let mut expected = 1;
        let mut batch_end = 0;
        while next <= branch.sequence {
            let last = branch.sequence.min(next + PAGE - 1);
            for record in read_range(db, &mut reader, &key, next..=last, false).await? {
                let sequence = record.event.sequence;
                if sequence != expected {
                    return Err(error("Timeline export contains a sequence gap"));
                }
                if batch_end < sequence {
                    batch_end = record.batch_end;
                }
                if record.batch_end != batch_end
                    || batch_end < sequence
                    || batch_end > branch.sequence
                {
                    return Err(error("Timeline export contains an invalid batch boundary"));
                }
                expected += 1;
                line(
                    out,
                    &Line::Event {
                        branch_id: &key.branch_id,
                        event: &record.event,
                        trace: record.trace.as_ref(),
                        batch_end: record.batch_end,
                    },
                )?;
                events += 1;
            }
            next = last + 1;
        }
        if expected != branch.sequence + 1 || batch_end != branch.sequence {
            return Err(error("Timeline export does not reach the branch head"));
        }
    }
    line(
        out,
        &Line::End {
            branches: branches.len(),
            events,
        },
    )?;
    out.flush().map_err(error)
}

fn line(out: &mut impl Write, line: &Line) -> Result<()> {
    serde_json::to_writer(&mut *out, line).map_err(error)?;
    out.write_all(b"\n").map_err(error)
}
