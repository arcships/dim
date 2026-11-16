use super::{codec, sqlite::error, timeline::Row};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_runtime::Result;
use dim_agent_types::*;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

/// What the conversion rewrote, by the identity its derived rows are keyed
/// by.
#[derive(Default)]
struct Changed {
    /// Sessions holding a rewritten row: the conversion invalidated their
    /// derived recovery snapshots. Sorted, so the rewrites are stable.
    sessions: BTreeSet<String>,
    /// The rewritten rows whose text candidates must be derived again, by
    /// Session, branch and sequence, each with its kind.
    rows: BTreeMap<(String, String, u64), String>,
}

/// What one image conversion did beyond rewriting the fact rows: how many
/// indexed rows it derived text candidates for. A flow test bounds that by the
/// rows it converts, never by the size of the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Converted {
    /// Indexed rows whose candidates the conversion derived again.
    pub(super) indexed: usize,
}

pub(super) fn upgrade(
    connection: &Connection,
    database: &Path,
    progress: Option<&super::sqlite::UpgradeProgress>,
) -> Result<Converted> {
    let mut directory = database.as_os_str().to_os_string();
    directory.push(".session-files");
    let mut importer = Importer {
        connection,
        directory: directory.into(),
        files: HashMap::new(),
        changes: 0,
    };
    let mut changed = Changed::default();
    let mut cursor = 0i64;
    let mut reader = codec::Reader::default();
    loop {
        let row = connection.query_row(
            "SELECT e.rowid, e.session_id, e.branch_id, e.sequence, e.kind, e.body FROM timeline_events e JOIN agent_sessions s ON s.session_id=e.session_id WHERE e.rowid > ?1 ORDER BY e.rowid LIMIT 1",
            [cursor], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, super::timeline::read_sequence(row, 3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?)),
        );
        let (id, session, branch, sequence, kind, body) = match row {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => break,
            Err(failure) => return Err(error(failure)),
        };
        cursor = id;
        let value: serde_json::Value = serde_json::from_str(&body).map_err(error)?;
        if !requires_conversion(&value) {
            continue;
        }
        let Row::Event { record } = serde_json::from_value::<Row>(value).map_err(error)? else {
            continue;
        };
        let mut wire = *record;
        let mut record = loop {
            reader.begin_hold();
            match reader.decode(connection, &session, wire)? {
                Ok(record) => break record,
                Err(pending) => wire = pending,
            }
        };
        let before = importer.changes;
        importer.event(&session, &mut Arc::make_mut(&mut record.event).kind)?;
        if let Some(trace) = &mut record.trace {
            importer.event(&session, &mut trace.original.kind)?;
            for step in &mut trace.steps {
                if let Some(draft) = &mut step.result {
                    importer.event(&session, &mut draft.kind)?;
                }
            }
        }
        if importer.changes == before {
            continue;
        }
        let mut content = Vec::new();
        let mut fresh = HashMap::new();
        let lists = std::iter::once(&record.event.kind)
            .chain(codec::drafts(&record.trace).map(|draft| &draft.kind))
            .map(|kind| {
                codec::request(kind)
                    .map(|request| {
                        codec::encode_list(request, 0, None, None, &mut content, &mut fresh)
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        for node in content {
            connection.execute("INSERT OR IGNORE INTO timeline_content(session_id,hash,kind,body) VALUES(?1,?2,?3,?4)",
                params![session,node.hash,node.kind,node.body]).map_err(error)?;
        }
        let mut wire = codec::encode(&record, lists.into_iter())?;
        wire.externalize_arrays(connection, &session)?;
        let body = serde_json::to_string(&Row::Event {
            record: Box::new(wire),
        })
        .map_err(error)?;
        connection
            .execute(
                "UPDATE timeline_events SET body=?1 WHERE rowid=?2",
                params![body, id],
            )
            .map_err(error)?;
        changed.sessions.insert(session.clone());
        changed.rows.insert((session, branch, sequence), kind);
    }
    if changed.sessions.is_empty() {
        return Ok(Converted::default());
    }
    // A snapshot's state may hold the inline image bytes its facts no longer
    // do, so it cannot outlive the conversion. A Session the conversion did
    // not touch keeps the fast path its snapshot grants.
    for session in &changed.sessions {
        connection
            .execute(
                "DELETE FROM timeline_snapshots WHERE session_id=?1",
                [session],
            )
            .map_err(error)?;
    }
    // An output row's candidates carry the tail of the deltas before it, and
    // the tails table holds the newest tail only, so re-deriving deltas out of
    // order would read a tail this upgrade never rebuilt. The conversion
    // rewrites no delta — an output row holds no image to convert — so this
    // states what conversion may touch: a rewritten output row falls back to
    // the rebuild that derives every row in order.
    if changed
        .rows
        .values()
        .any(|kind| super::timeline::output_row(kind))
    {
        super::text::backfill(connection)?;
        return Ok(Converted::default());
    }
    if let Some(progress) = progress {
        progress.enter(super::sqlite::UpgradeStage::Index);
    }
    let mut indexed = 0;
    for (session, branch, sequence) in changed.rows.keys() {
        indexed += usize::from(super::text::rewrite_event(
            connection, session, branch, *sequence,
        )?);
    }
    Ok(Converted { indexed })
}

fn requires_conversion(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.get("type").is_some_and(|kind| kind == "image")
                || object.get("refs").is_some_and(|refs| !refs.is_null())
                || object.values().any(requires_conversion)
        }
        serde_json::Value::Array(values) => values.iter().any(requires_conversion),
        _ => false,
    }
}

struct Importer<'a> {
    connection: &'a Connection,
    directory: PathBuf,
    files: HashMap<String, String>,
    changes: usize,
}
impl Importer<'_> {
    fn source(&mut self, session: &str, source: &mut ImageSource) -> Result<bool> {
        match source.source_type {
            ImageSourceType::File => Ok(true),
            ImageSourceType::Url => {
                self.changes += 1;
                Ok(false)
            }
            ImageSourceType::Base64 => {
                self.changes += 1;
                let data = source
                    .data
                    .as_deref()
                    .ok_or_else(|| error("Historical image data is missing"))?;
                if data.len() as u64 > dim_agent_capabilities::images::base64_len(16 * 1024 * 1024)
                {
                    return Err(error("Historical image exceeds the 16 MiB import limit"));
                }
                let bytes = STANDARD.decode(data).map_err(error)?;
                let hash = format!("{:x}", Sha256::digest(&bytes));
                let id = match self.files.get(&hash) {
                    Some(id) => id.clone(),
                    None => {
                        let id = uuid::Uuid::new_v4().to_string();
                        let directory = self.directory.join(&id);
                        crate::create_private_directory(&directory).map_err(error)?;
                        let mut file =
                            tempfile::NamedTempFile::new_in(&directory).map_err(error)?;
                        file.write_all(&bytes).map_err(error)?;
                        file.as_file().sync_all().map_err(error)?;
                        let file = file
                            .persist_noclobber(directory.join("image"))
                            .map_err(error)?;
                        let mut permissions = file.metadata().map_err(error)?.permissions();
                        permissions.set_readonly(true);
                        file.set_permissions(permissions).map_err(error)?;
                        #[cfg(unix)]
                        {
                            std::fs::File::open(&directory)
                                .and_then(|dir| dir.sync_all())
                                .map_err(error)?;
                            std::fs::File::open(&self.directory)
                                .and_then(|dir| dir.sync_all())
                                .map_err(error)?;
                        }
                        self.connection.execute("INSERT INTO session_files(file_id,node,writer) VALUES(?1,NULL,NULL)",[&id]).map_err(error)?;
                        self.files.insert(hash, id.clone());
                        id
                    }
                };
                self.connection.execute("INSERT OR IGNORE INTO session_file_owners(session_id,file_id) VALUES(?1,?2)",params![session,id]).map_err(error)?;
                source.source_type = ImageSourceType::File;
                source.file_id = Some(id);
                source.data = None;
                source.url = None;
                Ok(true)
            }
        }
    }
    fn blocks(&mut self, session: &str, blocks: &mut [ContentBlock]) -> Result<()> {
        for block in blocks {
            if let ContentBlock::Image { source } = block
                && !self.source(session, source)?
            {
                *block = ContentBlock::Text {
                    text: "[Image omitted: the image cannot be sent to this model.]".into(),
                };
            }
        }
        Ok(())
    }
    fn tool(&mut self, session: &str, content: &mut ToolContent) -> Result<()> {
        if let ToolContent::Blocks(blocks) = content {
            self.blocks(session, blocks)?;
        }
        Ok(())
    }
    fn parts(&mut self, session: &str, parts: &mut [MessagePart]) -> Result<()> {
        for part in parts {
            match part {
                MessagePart::Image { source } => {
                    if !self.source(session, source)? {
                        *part = MessagePart::Text {
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                            text: "[Image omitted: the image cannot be sent to this model.]".into(),
                        };
                    }
                }
                MessagePart::ToolResult { content, .. } => self.tool(session, content)?,
                _ => {}
            }
        }
        Ok(())
    }
    fn message(&mut self, session: &str, message: &mut Message) -> Result<()> {
        if let MessageContent::Parts(parts) = &mut message.content {
            self.parts(session, parts)?;
        }
        Ok(())
    }
    fn event(&mut self, session: &str, event: &mut EventKind) -> Result<()> {
        match event {
            EventKind::InputMessage(input) => {
                if let Some(user) = &mut input.user {
                    self.blocks(session, &mut user.content)?;
                }
            }
            EventKind::ToolCompleted(result) => self.tool(session, &mut result.result.content)?,
            EventKind::ToolResult(result) => {
                for result in &mut result.results {
                    self.tool(session, &mut result.result.content)?;
                }
            }
            EventKind::ExecEnded(result) => self.tool(session, &mut result.result.content)?,
            EventKind::ModelContent(content) => self.parts(session, &mut content.content)?,
            EventKind::ContextCompaction(compaction) => {
                if let ContextCompactionResult::Completed { replacement, .. } =
                    &mut compaction.outcome
                {
                    for message in replacement {
                        self.message(session, message)?;
                    }
                }
            }
            EventKind::ModelCall(call) => {
                for message in &mut Arc::make_mut(&mut call.request).messages {
                    self.message(session, Arc::make_mut(message))?;
                }
            }
            EventKind::ContextPrepare(call) => {
                for message in &mut Arc::make_mut(&mut call.request).messages {
                    self.message(session, Arc::make_mut(message))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
