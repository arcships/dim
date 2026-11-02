use super::{
    SessionStore, metadata,
    sqlite::error,
    timeline::{Row, read_sequence, stored_origin},
};
use crate::protocol::{Catalog, CatalogResult, SessionInfo, SessionSummary};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_runtime::{AgentError, Result};
use dim_agent_types::{ContentBlock, InputMessageData, ModelId};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub(super) fn create(connection: &Connection, session: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO session_catalog SELECT session_id, created_at_ms, NULL, json_extract(config, '$.cwd') FROM agent_sessions WHERE session_id=?1",
        [session],
    ).map_err(error)?;
    Ok(())
}

/// Initializes the catalog after importing a Session's native rows.
/// Call inside the transaction that inserts those rows, before committing.
pub fn initialize_session_catalog(connection: &Connection, session: &str) -> Result<()> {
    create(connection, session)?;
    refresh(connection, session)
}

pub(crate) fn truncate_preview(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

pub(super) fn refresh(connection: &Connection, session: &str) -> Result<()> {
    let last: Option<String> = connection.query_row(
        "SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' ORDER BY sequence DESC LIMIT 1",
        [session], |row| row.get(0),
    ).optional().map_err(error)?;
    if let Some(body) = last {
        let row: Row = serde_json::from_str(&body).map_err(error)?;
        let time = row
            .wires()
            .last()
            .and_then(|wire| wire.event["create_at"].as_u64())
            .ok_or_else(|| error("catalog activity event has no timestamp"))?;
        connection
            .execute(
                "UPDATE session_catalog SET updated_at_ms=?2 WHERE session_id=?1",
                params![session, i64::try_from(time).map_err(error)?],
            )
            .map_err(error)?;
    }
    let preview: Option<String> = connection
        .query_row(
            "SELECT preview FROM session_catalog WHERE session_id=?1",
            [session],
            |row| row.get(0),
        )
        .map_err(error)?;
    if preview.is_some() {
        return Ok(());
    }
    let boundary: Option<String> = connection.query_row(
        "SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND sequence<=256 ORDER BY sequence DESC LIMIT 1",
        [session], |row| row.get(0),
    ).optional().map_err(error)?;
    let Some(body) = boundary else { return Ok(()) };
    let row: Row = serde_json::from_str(&body).map_err(error)?;
    let end = row
        .wires()
        .iter()
        .rev()
        .find(|wire| wire.event["sequence"].as_u64().is_some_and(|s| s <= 256))
        .ok_or_else(|| error("catalog preview boundary is missing"))?
        .batch_end;
    let mut query = connection.prepare_cached(
        "SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND sequence<=?2 AND kind='input/message' ORDER BY sequence",
    ).map_err(error)?;
    let rows = query
        .query_map(
            params![session, i64::try_from(end).map_err(error)?],
            |row| row.get::<_, String>(0),
        )
        .map_err(error)?;
    for body in rows {
        let row: Row = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        let Row::Event { record } = row else {
            return Err(error("catalog input is not an event row"));
        };
        let input: InputMessageData =
            serde_json::from_value(record.event["data"].clone()).map_err(error)?;
        if let Some(user) = input.user {
            let text = user
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let preview = (!text.is_empty()).then(|| truncate_preview(&text, 120));
            connection
                .execute(
                    "UPDATE session_catalog SET preview=?2 WHERE session_id=?1",
                    params![session, preview],
                )
                .map_err(error)?;
            break;
        }
    }
    Ok(())
}

pub(super) fn backfill(connection: &Connection) -> Result<()> {
    let sessions = connection
        .prepare("SELECT session_id FROM agent_sessions")
        .map_err(error)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    for session in sessions {
        create(connection, &session)?;
        refresh(connection, &session)?;
    }
    Ok(())
}

impl SessionStore {
    pub(crate) async fn catalog(
        &self,
        request: Catalog,
        agent: Option<String>,
        version: u32,
        hosts: Vec<String>,
        closed: Vec<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<CatalogResult> {
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &request.query,
                    &request.cwd,
                    request.with_preview,
                    request.with_internal,
                    request.with_acp_sessions,
                ))
                .map_err(error)?
            )
        );
        let cursor = request
            .cursor
            .as_ref()
            .map(|cursor| {
                let invalid = || AgentError::InvalidInput("invalid catalog cursor".into());
                let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
                let (time, id, filters): (u64, String, String) =
                    serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if filters != digest || time > i64::MAX as u64 {
                    return Err(invalid());
                }
                Ok((time, id))
            })
            .transpose()?;
        self.timeline.db.run_cancellable(cancel, move |connection| {
            let index = if request.with_preview { "session_catalog_with_preview" } else { "session_catalog_by_activity" };
            let mut sql = format!(
                "SELECT c.session_id AS session_id, c.cwd, s.created_at_ms, b.sequence, o.agent_id, \
                 m.revision, m.title, m.title_source, c.preview, c.updated_at_ms AS updated_at_ms, \
                 CASE WHEN json_valid(s.config) THEN json_extract(s.config, '$.model.providerId') END, \
                 CASE WHEN json_valid(s.config) THEN json_extract(s.config, '$.model.modelId') END, \
                 s.origin, s.leader_session_id \
                 FROM session_catalog c INDEXED BY {index} CROSS JOIN agent_sessions s ON s.session_id=c.session_id \
                 CROSS JOIN session_owners o ON o.session_id=c.session_id \
                 CROSS JOIN timeline_branches b ON b.session_id=c.session_id AND b.branch_id='main' \
                 LEFT JOIN session_metadata m ON m.session_id=c.session_id \
                 WHERE o.protocol_version=?1 AND o.agent_id IN (SELECT value FROM json_each(?2)) \
                 AND c.session_id NOT IN (SELECT value FROM json_each(?3))"
            );
            let mut values: Vec<rusqlite::types::Value> = vec![i64::from(version).into(), serde_json::to_string(&hosts).map_err(error)?.into(), serde_json::to_string(&closed).map_err(error)?.into()];
            if request.with_preview { sql.push_str(" AND c.preview IS NOT NULL"); }
            if !request.with_internal { sql.push_str(" AND s.origin != 'internal'"); }
            if !request.with_acp_sessions { sql.push_str(" AND s.origin != 'acp'"); }
            if let Some(agent) = agent { values.push(agent.into()); sql.push_str(&format!(" AND o.agent_id=?{}", values.len())); }
            if let Some(cwd) = request.cwd { values.push(cwd.into()); sql.push_str(&format!(" AND c.cwd=?{}", values.len())); }
            if let Some(query) = request.query {
                values.push(query.to_lowercase().into()); let p = values.len();
                sql.push_str(&format!(" AND (instr(catalog_fold(c.session_id),?{p})>0 OR instr(catalog_fold(c.cwd),?{p})>0 OR instr(catalog_fold(c.preview),?{p})>0 OR instr(catalog_fold(m.title),?{p})>0 OR instr(catalog_fold(CASE WHEN json_valid(s.config) THEN json_extract(s.config,'$.model.providerId') END),?{p})>0 OR instr(catalog_fold(CASE WHEN json_valid(s.config) THEN json_extract(s.config,'$.model.modelId') END),?{p})>0)"));
            }
            if let Some((time, id)) = cursor {
                values.push((time as i64).into());
                let time = values.len();
                values.push(id.into());
                let id = values.len();
                sql = format!("{sql} AND c.updated_at_ms=?{time} AND c.session_id>?{id} UNION ALL {sql} AND c.updated_at_ms<?{time}");
            }
            sql.push_str(" ORDER BY updated_at_ms DESC, session_id");
            if let Some(limit) = request.limit { values.push(((limit+1) as i64).into()); sql.push_str(&format!(" LIMIT ?{}", values.len())); }
            let mut statement = connection.prepare_cached(&sql).map_err(error)?;
            let mut rows = statement.query(rusqlite::params_from_iter(values)).map_err(error)?;
            let mut sessions = Vec::new();
            let mut fetched = 0usize;
            let mut last: Option<(u64, String)> = None;
            while let Some(row) = rows.next().map_err(error)? {
                fetched += 1;
                let id: String = match row.get(0) {
                    Ok(id) => id,
                    Err(error) => {
                        tracing::error!(%error, "Invalid Session catalog row");
                        continue;
                    }
                };
                if let Ok(updated_at) = read_sequence(row, 9) {
                    last = Some((updated_at, id.clone()));
                }
                let parsed = (|| -> Result<SessionSummary> {
                    Ok(SessionSummary {
                        session: SessionInfo {
                            session_id: id.clone(), cwd: row.get(1).map_err(error)?,
                            created_at: read_sequence(row, 2).map_err(error)?, sequence: read_sequence(row, 3).map_err(error)?,
                            agent_id: Some(row.get(4).map_err(error)?), title: Some(metadata::decode(row, 5).map_err(error)?.into()),
                            model: ModelId { provider_id: row.get(10).map_err(error)?, model_id: row.get(11).map_err(error)? },
                            origin: Some(stored_origin(row, 12)?), parent_session_id: row.get(13).map_err(error)?,
                            running: false, attached_clients: 0,
                        }, preview: row.get(8).map_err(error)?, updated_at: read_sequence(row, 9).map_err(error)?,
                        // Agents marks the rows this daemon closed; the query
                        // already leaves them out unless it asked for them.
                        closed: false,
                    })
                })();
                match parsed {
                    Ok(summary) => sessions.push(summary),
                    Err(error) => {
                        tracing::error!(session_id=%id, %error, "Invalid Session catalog row");
                    }
                }
            }
            // `fetched` counts the raw window (`limit + 1`), not the readable page: a skipped row
            // must not end the listing. When the page is full, the cursor follows its last shown
            // row; when the window is full but the page is not, it follows the last fetched row so
            // the next page starts past it.
            let next_cursor = match request.limit.filter(|limit| fetched > *limit) {
                Some(limit) => {
                    let cursor = if sessions.len() > limit {
                        sessions.truncate(limit);
                        sessions.last().map(|last| {
                            (last.updated_at, last.session.session_id.clone())
                        })
                    } else {
                        last.or_else(|| {
                            sessions.last().map(|summary| {
                                (summary.updated_at, summary.session.session_id.clone())
                            })
                        })
                    };
                    match cursor {
                        Some((time, id)) => Some(
                            URL_SAFE_NO_PAD
                                .encode(serde_json::to_vec(&(time, &id, &digest)).map_err(error)?),
                        ),
                        None => None,
                    }
                }
                None => None,
            };
            Ok(CatalogResult { sessions, next_cursor })
        }).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        ServerConfig, connection::ConnectionHub, connection::Entry, serve, storage::SqliteStorage,
    };
    use dim_agent_runtime::{AgentBuilder, AgentConfig};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

    type Wire = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    async fn send(wire: &mut Wire, id: u64, method: &str, params: Value) {
        wire.send(Message::Text(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }

    async fn response(wire: &mut Wire, id: u64) -> Value {
        loop {
            let frame = wire.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }

    async fn connect(address: std::net::SocketAddr) -> Wire {
        let mut wire = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap()
            .0;
        send(
            &mut wire,
            1,
            "hello",
            json!({"protocolVersion":3,"clientKind":"other"}),
        )
        .await;
        assert!(
            response(&mut wire, 1).await["result"]["features"]
                .as_array()
                .unwrap()
                .contains(&json!("session.catalog.cancel"))
        );
        wire
    }

    /// Returns once the server has dropped every websocket connection: a
    /// connection cancels its in-flight calls before it leaves the hub.
    async fn wait_until_disconnected(hub: &ConnectionHub) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while hub.entry_count(Entry::Bare) > 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the disconnected client's connection is still open"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn catalog_cancellation_interrupts_running_sql_and_preserves_following_writes() {
        let directory =
            std::env::temp_dir().join(format!("dim-catalog-cancel-{}", uuid::Uuid::new_v4()));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                database_path: Some(directory.join("store.sqlite")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let address = server.websocket_address.unwrap();
        let mut wire = connect(address).await;
        send(
            &mut wire,
            2,
            "session.create",
            json!({"cwd":std::env::temp_dir()}),
        )
        .await;
        let created = response(&mut wire, 2).await;
        let seed = created["result"]["sessionId"]
            .as_str()
            .expect("a native Session")
            .to_owned();
        drop(wire);
        let database = server.root.get::<SqliteStorage>().unwrap();
        let hub = server.root.get::<ConnectionHub>().unwrap();
        database.run(move |connection| {
            let tx = connection.transaction().map_err(error)?;
            for index in 0..2300 {
                let id = format!("catalog-{index:04}");
                tx.execute("INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id) SELECT ?1, config, created_at_ms, leader_session_id FROM agent_sessions WHERE session_id=?2", params![id,seed]).map_err(error)?;
                tx.execute("INSERT INTO session_owners SELECT ?1,agent_id,protocol_version FROM session_owners WHERE session_id=?2", params![id,seed]).map_err(error)?;
                tx.execute("INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',0)", [&id]).map_err(error)?;
                create(&tx, &id)?;
            }
            tx.commit().map_err(error)
        }).await.unwrap();

        for disconnect in [false, true] {
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            database
                .run(move |connection| {
                    let entered = std::sync::Mutex::new(Some(entered));
                    connection
                        .create_scalar_function(
                            "catalog_fold",
                            1,
                            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                            move |ctx| {
                                observed.fetch_add(1, Ordering::SeqCst);
                                if let Some(entered) = entered.lock().unwrap().take() {
                                    let _ = entered.send(());
                                    blocked
                                        .recv_timeout(Duration::from_secs(10))
                                        .expect("the running SQL is released");
                                }
                                Ok(ctx
                                    .get::<Option<String>>(0)?
                                    .map(|text| text.to_lowercase()))
                            },
                        )
                        .map_err(error)
                })
                .await
                .unwrap();
            let mut wire = connect(address).await;
            send(
                &mut wire,
                2,
                "session.catalog",
                json!({"query":"no such catalog row"}),
            )
            .await;
            tokio::time::timeout(Duration::from_secs(10), ready)
                .await
                .unwrap()
                .unwrap();
            let mut wire = if disconnect {
                drop(wire);
                wait_until_disconnected(&hub).await;
                None
            } else {
                send(
                    &mut wire,
                    3,
                    "session.catalog.cancel",
                    json!({"requestId":2.0}),
                )
                .await;
                assert_eq!(response(&mut wire, 3).await["result"], json!({}));
                Some(wire)
            };
            let writing = database.run(|connection| {
                connection.execute("UPDATE session_catalog SET preview='write after cancellation' WHERE session_id='catalog-0000'", []).map_err(error)
            });
            tokio::pin!(writing);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut writing)
                    .await
                    .is_err(),
                "the query owns the real SQLite connection"
            );
            release.send(()).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), writing)
                    .await
                    .unwrap()
                    .unwrap(),
                1
            );
            assert!(
                calls.load(Ordering::SeqCst) < 2300,
                "cancellation interrupted the scan before it visited every Session"
            );
            if let Some(wire) = &mut wire {
                let reply = response(wire, 2).await;
                assert_eq!(reply["error"]["data"]["appError"], "cancelled", "{reply}");
                send(wire, 4, "session.catalog.cancel", json!({"requestId":2})).await;
                assert_eq!(response(wire, 4).await["result"], json!({}));
                send(
                    wire,
                    5,
                    "session.catalog",
                    json!({"query":"write after cancellation"}),
                )
                .await;
                let page = response(wire, 5).await;
                assert_eq!(
                    page["result"]["sessions"].as_array().unwrap().len(),
                    1,
                    "{page}"
                );
            }
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn catalog_skips_a_session_whose_config_cannot_be_decoded() {
        let directory =
            std::env::temp_dir().join(format!("dim-catalog-bad-{}", uuid::Uuid::new_v4()));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                database_path: Some(directory.join("store.sqlite")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let address = server.websocket_address.unwrap();
        let mut wire = connect(address).await;
        send(
            &mut wire,
            2,
            "session.create",
            json!({"cwd":std::env::temp_dir()}),
        )
        .await;
        let good = response(&mut wire, 2).await["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        send(
            &mut wire,
            3,
            "session.create",
            json!({"cwd":std::env::temp_dir()}),
        )
        .await;
        let bad = response(&mut wire, 3).await["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(wire);
        let database = server.root.get::<SqliteStorage>().unwrap();
        let corrupt = bad.clone();
        database
            .run(move |connection| {
                connection
                    .execute(
                        "UPDATE agent_sessions SET config='not json' WHERE session_id=?1",
                        [&corrupt],
                    )
                    .map_err(error)?;
                Ok(())
            })
            .await
            .unwrap();

        let mut wire = connect(address).await;
        send(&mut wire, 4, "session.catalog", json!({"limit":50})).await;
        let catalog = response(&mut wire, 4).await;
        let ids: Vec<&str> = catalog["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|summary| summary["sessionId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&good.as_str()), "{catalog}");
        assert!(!ids.contains(&bad.as_str()), "{catalog}");

        send(&mut wire, 5, "session.summaries", json!({})).await;
        let summaries = response(&mut wire, 5).await;
        let ids: Vec<&str> = summaries["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|summary| summary["sessionId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&good.as_str()), "{summaries}");
        assert!(!ids.contains(&bad.as_str()), "{summaries}");

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn directory_stops_when_its_caller_is_gone() {
        let directory =
            std::env::temp_dir().join(format!("dim-directory-cancel-{}", uuid::Uuid::new_v4()));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                database_path: Some(directory.join("store.sqlite")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let store = server.root.get::<SessionStore>().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let result = store.directory(None, 3, cancel).await;
        assert!(
            matches!(result, Err(AgentError::Cancelled(_))),
            "{result:?}"
        );
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn catalog_pages_past_a_session_that_cannot_be_decoded() {
        let directory =
            std::env::temp_dir().join(format!("dim-catalog-page-{}", uuid::Uuid::new_v4()));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                database_path: Some(directory.join("store.sqlite")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let address = server.websocket_address.unwrap();
        let mut wire = connect(address).await;
        let mut sessions = Vec::new();
        for (id, time) in [(2u64, 100u64), (3, 200), (4, 300), (5, 400)] {
            send(
                &mut wire,
                id,
                "session.create",
                json!({"cwd":std::env::temp_dir()}),
            )
            .await;
            let session = response(&mut wire, id).await["result"]["sessionId"]
                .as_str()
                .unwrap()
                .to_owned();
            sessions.push((session, time));
        }
        drop(wire);
        let database = server.root.get::<SqliteStorage>().unwrap();
        let corrupt = sessions[3].0.clone();
        let times = sessions.clone();
        database
            .run(move |connection| {
                for (session, time) in &times {
                    connection
                        .execute(
                            "UPDATE session_catalog SET updated_at_ms=?2 WHERE session_id=?1",
                            params![session, *time as i64],
                        )
                        .map_err(error)?;
                }
                connection
                    .execute(
                        "UPDATE agent_sessions SET config='not json' WHERE session_id=?1",
                        [&corrupt],
                    )
                    .map_err(error)?;
                Ok(())
            })
            .await
            .unwrap();

        let mut wire = connect(address).await;
        send(&mut wire, 10, "session.catalog", json!({"limit":2})).await;
        let first = response(&mut wire, 10).await;
        let page: Vec<&str> = first["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|summary| summary["sessionId"].as_str().unwrap())
            .collect();
        assert_eq!(
            page,
            vec![sessions[2].0.as_str(), sessions[1].0.as_str()],
            "{first}"
        );
        let cursor = first["result"]["nextCursor"]
            .as_str()
            .expect("a full window has a cursor");
        send(
            &mut wire,
            11,
            "session.catalog",
            json!({"limit":2,"cursor":cursor}),
        )
        .await;
        let second = response(&mut wire, 11).await;
        let page: Vec<&str> = second["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|summary| summary["sessionId"].as_str().unwrap())
            .collect();
        assert_eq!(page, vec![sessions[0].0.as_str()], "{second}");
        assert!(second["result"]["nextCursor"].is_null(), "{second}");

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
