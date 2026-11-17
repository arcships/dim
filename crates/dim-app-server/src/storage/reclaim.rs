//! Reclaims `timeline_content` rows no stored reference reaches any more
//! (#1697). The v50 image conversion rewrote inline image bytes into Session
//! files and gave the rewritten requests content addresses of their own, but
//! the rows holding the old addresses stayed, so the store kept the base64
//! payload beside the file. Removing such a row deletes only content nothing
//! reaches: it rewrites no fact, keeps every live address and changes no
//! format.
//!
//! Content is shared only inside one Session, so reachability is per Session
//! and has two edges:
//!
//! ```text
//!  timeline_events.body
//!    WireEvent.refs  --------------------------> the hash it names
//!      (record, trace.original, trace.steps[].result, output records)
//!
//!  node content row $.parent  ---------------> the node of the message
//!                                               before it
//! ```
//!
//! The first edge uses `WireEvent::references`, also used by fork. Array locators name
//! `array-element` rows, a kind no rewrite turns into a node and this module
//! never removes.
//!
//! Deleting is safe because a content row is derived and re-creatable: a
//! writer inserts content before the rows that name it, in the same
//! transaction, and never rewrites it in place. A row no reference reaches
//! cannot become reachable later, and a writer that needs the same bytes again
//! inserts them under the same address.
//!
//! The offline repair reads every Session before it deletes anything, so a
//! Session whose rows name content the store does not hold refuses the whole
//! run rather than being shrunk into looking healthy. The image conversion
//! deletes inside the transaction of its own rewrites instead, and removes only
//! the rows whose addresses that rewrite replaced.
use super::{sqlite::error, timeline::Row};
use dim_agent_runtime::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;

/// The kinds the image conversion rewrites, and so the only kinds a repair
/// removes. A `node` or v16 `message` body is the one place an inline image
/// lived; a `tools` or `system` body holds none, and an `array-element` row
/// belongs to the locator row that names it. Rows of any other kind that no
/// reference reaches are left for the owner that introduced them.
const CANDIDATE_KINDS: &str = "'node','message'";
/// Rows read per statement while the roots are collected, so a long history
/// never holds more than one page of bodies.
const PAGE: usize = 256;

/// What one repair removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Content rows removed.
    pub rows: u64,
    /// The UTF-8 bytes those rows held in their bodies. This is what the store
    /// no longer holds; it is not space returned to the file system. A delete
    /// returns the rows' pages to the store's freelist, and only a `VACUUM`
    /// gives them back, so this count and the database file's size answer
    /// different questions.
    pub bytes: u64,
}
impl Reclaimed {
    fn add(&mut self, other: Self) {
        self.rows += other.rows;
        self.bytes += other.bytes;
    }
}

/// Sessions with stored events or content, including events whose content
/// rows are all missing and content left by an interrupted Session deletion.
fn sessions(connection: &Connection) -> Result<Vec<String>> {
    let mut query = connection
        .prepare(
            "SELECT session_id FROM timeline_events \
             UNION SELECT session_id FROM timeline_content ORDER BY session_id",
        )
        .map_err(error)?;
    query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)
}

/// The content addresses `session`'s rows reach, directly or through a node's
/// `parent`.
fn reachable(connection: &Connection, session: &str) -> Result<BTreeSet<String>> {
    let mut roots = BTreeSet::new();
    let mut after = 0i64;
    loop {
        // `rowid` orders one Session's rows uniquely; `sequence` does not, because
        // every branch numbers its own.
        let rows: Vec<(i64, String)> = connection
            .prepare_cached(
                "SELECT rowid, body FROM timeline_events \
                 WHERE session_id = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3",
            )
            .map_err(error)?
            .query_map(params![session, after, PAGE as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        let Some((last, _)) = rows.last() else {
            break;
        };
        for (rowid, body) in &rows {
            let row: Row = serde_json::from_str(body).map_err(|failure| {
                error(format!(
                    "stored row {rowid} of {session} does not decode: {failure}"
                ))
            })?;
            for wire in row.wires() {
                roots.extend(wire.references().map(str::to_owned));
            }
        }
        after = *last;
    }
    closure(connection, session, &roots)
}

/// The roots and every node `parent` they reach, following the same edges as
/// a fork copy. A node body that is not
/// readable JSON contributes no parent, exactly as a fork copy treats it: such a
/// node cannot be read back either, so following it is not what fails.
fn closure(
    connection: &Connection,
    session: &str,
    roots: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if roots.is_empty() {
        return Ok(BTreeSet::new());
    }
    let roots = serde_json::to_string(roots).map_err(error)?;
    connection
        .prepare(
            "WITH RECURSIVE closure(hash) AS ( \
               SELECT value FROM json_each(?2) \
               UNION SELECT json_extract(c.body, '$.parent') FROM closure \
                 CROSS JOIN timeline_content c ON c.session_id = ?1 AND c.hash = closure.hash \
                 WHERE c.kind = 'node' AND json_valid(c.body) \
                   AND json_type(c.body, '$.parent') = 'text') \
             SELECT hash FROM closure",
        )
        .map_err(error)?
        .query_map(params![session, roots], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(error)
}

/// Fails when `session`'s rows name content the store does not hold. No
/// operation leaves such a reference behind: a delete removes whole Sessions
/// and whole requests, never a row a live fact still names.
fn assert_held(connection: &Connection, session: &str, reachable: &BTreeSet<String>) -> Result<()> {
    let names = serde_json::to_string(reachable).map_err(error)?;
    let absent: Option<String> = connection
        .prepare_cached(
            "SELECT value FROM json_each(?2) WHERE NOT EXISTS \
             (SELECT 1 FROM timeline_content WHERE session_id = ?1 AND hash = value) LIMIT 1",
        )
        .map_err(error)?
        .query_row(params![session, names], |row| row.get(0))
        .optional()
        .map_err(error)?;
    match absent {
        Some(address) => Err(error(format!(
            "Session {session} names content {address}, which the store does not hold"
        ))),
        None => Ok(()),
    }
}

/// The content rows of `session` that nothing reaches, with the bytes of each.
/// Empty when the Session has nothing to remove, which is what makes a repeated
/// repair write nothing.
fn unreached(
    connection: &Connection,
    session: &str,
    reachable: &BTreeSet<String>,
) -> Result<Vec<(String, u64)>> {
    let names = serde_json::to_string(reachable).map_err(error)?;
    connection
        .prepare(&format!(
            "SELECT hash, length(CAST(body AS BLOB)) \
             FROM timeline_content \
             WHERE session_id = ?1 AND kind IN ({CANDIDATE_KINDS}) \
               AND hash NOT IN (SELECT value FROM json_each(?2))"
        ))
        .map_err(error)?
        .query_map(params![session, names], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(error)?
        .map(|row| {
            let (hash, bytes) = row.map_err(error)?;
            Ok((hash, u64::try_from(bytes).map_err(error)?))
        })
        .collect()
}

/// The content rows of `session` the image conversion may remove: those its
/// rewrite orphaned.
///
/// `seeds` are the addresses the rewritten rows named before the rewrite. The
/// conversion reaches the rest of what it replaced only through `parent`, which
/// is why the seeds are closed over first: a replaced list's earlier nodes are
/// named by nothing but the node after them. A row the rewritten facts still
/// reach after the rewrite is kept whatever the seeds say, so a Session that
/// rewrote one request pointing into a list another request shares loses
/// nothing.
///
/// The conversion calls this inside the transaction of its own rewrites, so it
/// opens none, and it removes nothing an addressed row did not lead to. A
/// Session whose rows cannot be decoded fails the upgrade, as the conversion's
/// own pass over those rows already does.
pub(super) fn replaced(
    connection: &Connection,
    session: &str,
    seeds: &BTreeSet<String>,
) -> Result<Vec<(String, u64)>> {
    let replaced = closure(connection, session, seeds)?;
    let live = reachable(connection, session)?;
    let orphaned: BTreeSet<String> = replaced.difference(&live).cloned().collect();
    let candidates = unreached(connection, session, &live)?;
    Ok(candidates
        .into_iter()
        .filter(|(hash, _)| orphaned.contains(hash))
        .collect())
}

/// Removes exactly `rows` from `session` on the caller's transaction: the
/// deleted count must be the planned one, so the outcome the caller reports is
/// the one the store kept.
pub(super) fn remove(
    connection: &Connection,
    session: &str,
    rows: &[(String, u64)],
) -> Result<Reclaimed> {
    if rows.is_empty() {
        return Ok(Reclaimed::default());
    }
    let hashes = serde_json::to_string(&rows.iter().map(|(hash, _)| hash).collect::<Vec<_>>())
        .map_err(error)?;
    let deleted = connection
        .prepare_cached(&format!(
            "DELETE FROM timeline_content WHERE session_id = ?1 \
             AND kind IN ({CANDIDATE_KINDS}) AND hash IN (SELECT value FROM json_each(?2))"
        ))
        .map_err(error)?
        .execute(params![session, hashes])
        .map_err(error)?;
    if u64::try_from(deleted).map_err(error)? != rows.len() as u64 {
        return Err(error(format!(
            "{deleted} content rows of {session} left the store while {} were reclaimed",
            rows.len()
        )));
    }
    Ok(Reclaimed {
        rows: rows.len() as u64,
        bytes: rows.iter().map(|(_, bytes)| bytes).sum(),
    })
}

/// Removes the planned rows of `session` in one transaction of its own: either
/// every selected row goes or none does, so an interruption leaves them all for
/// the next run and the store never loses a live address. This repairs the
/// store and is not a format step: it neither reads nor writes the format
/// marker, so a repaired store stays readable by every daemon that reads its
/// format.
fn obsolete(
    connection: &mut Connection,
    session: &str,
    rows: &[(String, u64)],
) -> Result<Reclaimed> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    let removed = remove(&tx, session, rows)?;
    tx.commit().map_err(error)?;
    Ok(removed)
}

/// Removes every content row of the store that nothing reaches, as
/// `dimcode daemon vacuum` does before `VACUUM`.
///
/// Reading the whole store first is what makes a damaged store refuse as a
/// whole: `assert_held` runs for every Session before the first delete, so a
/// store that names content it does not hold is reported and left untouched
/// instead of being partly shrunk. Deleting afterwards keeps each Session's
/// rows in one transaction, so a run that stops between two of them leaves
/// whole Sessions repaired, every other Session as it was, and the next run
/// finishing the work.
///
/// The plan is read outside those transactions, which is sound because the
/// caller holds the database lease: no daemon writes while this runs, and
/// `remove` fails rather than reporting a row the store no longer had.
pub(super) fn obsolete_content(connection: &mut Connection) -> Result<Reclaimed> {
    let mut plan = Vec::new();
    for session in sessions(connection)? {
        let reachable = reachable(connection, &session)?;
        assert_held(connection, &session, &reachable)?;
        let rows = unreached(connection, &session, &reachable)?;
        if !rows.is_empty() {
            plan.push((session, rows));
        }
    }
    let mut removed = Reclaimed::default();
    for (session, rows) in &plan {
        removed.add(obsolete(connection, session, rows)?);
    }
    Ok(removed)
}
