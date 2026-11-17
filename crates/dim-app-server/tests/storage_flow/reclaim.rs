//! #1697: the content rows the v50 image conversion replaces are removed once
//! no reference reaches them. The conversion removes them in the transaction
//! that rewrites the facts, for the Sessions it rewrote; a store that committed
//! the conversion before that step keeps them until `dimcode daemon vacuum`
//! runs the same repair offline over every Session.
//!
//! Both paths must keep every live address: a node a request still walks, a
//! node a `parent` chain reaches, a row a *before* trace or an output row
//! names. Only content nothing reaches may go, and only the kinds the
//! conversion rewrites (`node`, `message`).

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use sha2::{Digest, Sha256};

const IMAGE: &[u8] = b"historical inline image bytes #1697";

fn content_hash(kind: &str, body: &str) -> String {
    format!(
        "{:x}",
        Sha256::new()
            .chain_update(kind)
            .chain_update([0])
            .chain_update(body)
            .finalize()
    )
}

fn insert_content(connection: &Connection, session: &str, kind: &str, body: &str) -> String {
    let hash = content_hash(kind, body);
    connection
        .execute(
            "INSERT INTO timeline_content (session_id, hash, kind, body) VALUES (?1, ?2, ?3, ?4)",
            params![session, hash, kind, body],
        )
        .unwrap();
    hash
}

fn insert_event(
    connection: &Connection,
    session: &str,
    sequence: u64,
    kind: &str,
    data: serde_json::Value,
) {
    let body = json!({
        "encoding": "event",
        "record": {
            "event": {
                "id": format!("event-{session}-{sequence}"),
                "sequence": sequence,
                "create_at": 1000 + sequence,
                "type": kind.split_once('/').unwrap().0,
                "name": kind.split_once('/').unwrap().1,
                "data": data,
            },
            "refs": null,
            "trace": null,
            "batch_end": sequence,
        }
    });
    connection
        .execute(
            "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id, context_render, context_state) \
             VALUES (?1, 'main', ?2, ?2, ?3, ?4, ?5, 1, 0)",
            params![session, sequence as i64, kind, body.to_string(), format!("event-{session}-{sequence}")],
        )
        .unwrap();
}

/// Every content row of `session`, as `hash|kind|body`.
fn content_rows(connection: &Connection, session: &str) -> Vec<String> {
    connection
        .prepare("SELECT hash || '|' || kind || '|' || body FROM timeline_content WHERE session_id=?1 ORDER BY hash")
        .unwrap()
        .query_map([session], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// The addresses `session`'s stored rows reach, read straight from the rows
/// with the edges the storage contract states: every `refs` member's `list`,
/// `tools`, `system` and `messages`, plus the `parent` chain of a node. It is
/// the test's own reading, not a call into the repair.
fn reachable(connection: &Connection, session: &str) -> std::collections::BTreeSet<String> {
    let mut roots = std::collections::BTreeSet::new();
    let mut statement = connection
        .prepare("SELECT body FROM timeline_events WHERE session_id=?1")
        .unwrap();
    for body in statement
        .query_map([session], |row| row.get::<_, String>(0))
        .unwrap()
    {
        let body: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        let mut stack = vec![&body];
        while let Some(value) = stack.pop() {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(refs) = object.get("refs").filter(|refs| !refs.is_null()) {
                        for key in ["list", "tools", "system"] {
                            if let Some(address) = refs.get(key).and_then(|value| value.as_str()) {
                                roots.insert(address.to_owned());
                            }
                        }
                        if let Some(messages) =
                            refs.get("messages").and_then(|value| value.as_array())
                        {
                            roots.extend(
                                messages
                                    .iter()
                                    .filter_map(|value| value.as_str())
                                    .map(str::to_owned),
                            );
                        }
                    }
                    stack.extend(object.values());
                }
                serde_json::Value::Array(values) => stack.extend(values),
                _ => {}
            }
        }
    }
    let mut closure = roots.clone();
    let mut pending: Vec<String> = roots.into_iter().collect();
    while let Some(address) = pending.pop() {
        let body: Option<String> = connection
            .query_row(
                "SELECT body FROM timeline_content WHERE session_id=?1 AND hash=?2 AND kind='node'",
                params![session, address],
                |row| row.get(0),
            )
            .ok();
        let Some(body) = body else { continue };
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(parent) = body.get("parent").and_then(|value| value.as_str())
            && closure.insert(parent.to_owned())
        {
            pending.push(parent.to_owned());
        }
    }
    closure
}

/// Reads every event of `session` with its requests rebuilt, which fails when a
/// fact names content the store does not hold. That is what every restore,
/// replay and export does, so it is where "no live address was removed" is
/// proved rather than inferred.
async fn assert_every_fact_reads(database: &Database, session: &str) -> usize {
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let branches = store.branches(session).await.unwrap();
    let mut events = 0;
    for branch in &branches {
        let head = branch.sequence;
        if head == 0 {
            continue;
        }
        events += store
            .read(&key(session, &branch.info.id), 1..=head)
            .await
            .unwrap_or_else(|failure| {
                panic!(
                    "{} {session}: a fact names content the store lost: {failure}",
                    branch.info.id
                )
            })
            .len();
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    events
}

/// The external image files the store owns, by content.
fn image_files(database: &Database) -> Vec<Vec<u8>> {
    let mut name = database.0.file_name().unwrap().to_os_string();
    name.push(".session-files");
    let directory = database.0.with_file_name(name);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<Vec<u8>> = entries
        .filter_map(|entry| std::fs::read(entry.ok()?.path().join("image")).ok())
        .collect();
    files.sort();
    files
}

fn payload() -> String {
    STANDARD.encode(IMAGE)
}

/// A v49-shaped store whose `converted` Session holds inline images the
/// conversion externalizes:
///
/// - an `input/message` holding one image block;
/// - a `model/call` whose node list is two messages, the first carrying the
///   same bytes, so the conversion rewrites that node and leaves the old leaf
///   and its parent unreachable;
/// - a `tools` and a `system` row the request names, which must stay;
/// - a node no reference reaches, which only the offline entry removes.
///
/// `shared` holds the same image bytes, so the two Sessions share one file and
/// get an ownership row each. `untouched` holds no image: the conversion does
/// not rewrite it, and only the offline entry reaches its unreachable node.
fn v49_store_with_replaced_content(database: &Database) -> Connection {
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(49);
    statements.sort_by_key(|sql| {
        !sql.starts_with("CREATE TABLE ") && !sql.starts_with("CREATE VIRTUAL TABLE ")
    });
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (49)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO agents (agent_id, profile, is_default, owner_session_id) \
             VALUES ('agent', '{}', 1, NULL)",
            [],
        )
        .ok();
    for session in ["converted", "shared", "untouched"] {
        connection
            .execute(
                "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
                 VALUES (?1, ?2, 1, 'client')",
                params![
                    session,
                    serde_json::to_string(&SessionConfig::default()).unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_owners (session_id, agent_id, protocol_version) \
                 VALUES (?1, 'agent', 4)",
                [session],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_catalog (session_id, updated_at_ms, preview, cwd) \
                 VALUES (?1, 1, NULL, '/tmp')",
                [session],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO timeline_branches (session_id, branch_id, info, sequence) \
                 VALUES (?1, 'main', '{\"id\":\"main\"}', 0)",
                [session],
            )
            .unwrap();
    }
    let encoded = STANDARD.encode(IMAGE);
    let image = json!({
        "type": "image",
        "source": {"type": "base64", "mediaType": "image/png", "data": encoded}
    });

    // A two-message list whose first node carries the image: the conversion
    // rewrites that node's message, so the old node and its parent both stop
    // being reachable.
    let first = insert_content(
        &connection,
        "converted",
        "node",
        &json!({"parent": null, "message": {"role": "user", "content": [image.clone()]}})
            .to_string(),
    );
    let second = insert_content(
        &connection,
        "converted",
        "node",
        &json!({"parent": first, "message": {"role": "assistant", "content": "kept"}}).to_string(),
    );
    let tools = insert_content(
        &connection,
        "converted",
        "tools",
        &json!([{
            "name": "probe",
            "description": "d",
            "inputSchema": {"type": "object"},
            "source": "static"
        }])
        .to_string(),
    );
    let system = insert_content(&connection, "converted", "system", "\"sys\"");
    insert_content(
        &connection,
        "converted",
        "node",
        &json!({"parent": null, "message": {"role": "user", "content": "unreachable"}}).to_string(),
    );
    insert_event(
        &connection,
        "converted",
        1,
        "input/message",
        json!({"user": {"content": [image.clone()]}}),
    );
    insert_event(
        &connection,
        "converted",
        2,
        "model/call",
        json!({
            "turn_id": "turn", "request_id": "request",
            "request": {"providerId": "mock", "modelId": "test", "messages": []}
        }),
    );
    connection
        .execute(
            "UPDATE timeline_events SET body=json_set(body, '$.record.refs', json(?1)) \
             WHERE session_id='converted' AND sequence=2",
            [json!({"list": second, "length": 2, "tools": tools, "system": system}).to_string()],
        )
        .unwrap();

    insert_content(
        &connection,
        "shared",
        "node",
        &json!({"parent": null, "message": {"role": "user", "content": "shared stale"}})
            .to_string(),
    );
    insert_event(
        &connection,
        "shared",
        1,
        "input/message",
        json!({"user": {"content": [image]}}),
    );

    // A child branch of `converted` copies `main`'s events and the content
    // rows they reach, so the same live address is referenced twice.
    connection
        .execute(
            "INSERT INTO timeline_branches (session_id, branch_id, info, sequence) \
             VALUES ('converted', 'child', '{\"id\":\"child\",\"parent\":\"main\",\"branch_point\":2,\"create_anchor\":2}', 0)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id, context_render, context_state) \
             SELECT session_id, 'child', sequence, last_sequence, kind, body, event_id, context_render, context_state \
             FROM timeline_events WHERE session_id='converted' AND branch_id='main'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE timeline_branches SET sequence=2 WHERE session_id='converted' AND branch_id='child'",
            [],
        )
        .unwrap();
    // A v16 row: one `message` content row per message, still read.
    let v16 = insert_content(
        &connection,
        "converted",
        "message",
        &json!({"role": "assistant", "content": "v16 message"}).to_string(),
    );
    insert_event(
        &connection,
        "converted",
        3,
        "model/call",
        json!({"turn_id": "turn", "request_id": "v16", "request": {"providerId": "mock", "modelId": "test", "messages": []}}),
    );
    connection
        .execute(
            "UPDATE timeline_events SET body=json_set(body, '$.record.refs', json(?1)) \
             WHERE session_id='converted' AND sequence=3",
            [json!({"messages": [v16], "tools": null, "system": null}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE timeline_branches SET sequence=3 WHERE session_id='converted' AND branch_id='main'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id, context_render, context_state) \
             SELECT session_id, 'child', sequence, last_sequence, kind, body, event_id, context_render, context_state \
             FROM timeline_events WHERE session_id='converted' AND branch_id='main' AND sequence=3",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE timeline_branches SET sequence=3 WHERE session_id='converted' AND branch_id='child'",
            [],
        )
        .unwrap();

    insert_content(
        &connection,
        "untouched",
        "node",
        &json!({"parent": null, "message": {"role": "user", "content": "plain"}}).to_string(),
    );
    insert_event(
        &connection,
        "untouched",
        1,
        "input/message",
        json!({"user": {"content": [{"type": "text", "text": "plain question"}]}}),
    );

    for session in ["converted", "shared", "untouched"] {
        let sequence: i64 = connection
            .query_row(
                "SELECT max(sequence) FROM timeline_events WHERE session_id=?1",
                [session],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE timeline_branches SET sequence=?2 WHERE session_id=?1",
                params![session, sequence],
            )
            .unwrap();
    }
    dim_app_server::storage::rebuild_text_index(&connection).unwrap();
    connection
}

/// #1697: the v50 conversion removes the rows whose addresses it replaced, in
/// the same transaction as the rewrite. Every live address still resolves, an
/// untouched Session keeps even its unreachable row, and the image bytes stay
/// reachable through the file.
#[tokio::test]
async fn the_v50_conversion_removes_the_content_it_replaced() {
    let database = Database::new();
    drop(v49_store_with_replaced_content(&database));

    let (converted_before, untouched_before) = {
        let connection = Connection::open(&database.0).unwrap();
        (
            content_rows(&connection, "converted"),
            content_rows(&connection, "untouched"),
        )
    };
    let mut exported_before = Vec::new();
    dim_app_server::storage::export_timeline(database.0.clone(), "converted", &mut exported_before)
        .await
        .unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(49));
    db.close().await.unwrap();

    assert_eq!(
        assert_every_fact_reads(&database, "converted").await,
        6,
        "both branches still rebuild every fact from its addresses"
    );
    assert_eq!(assert_every_fact_reads(&database, "shared").await, 1);

    let connection = Connection::open(&database.0).unwrap();
    let converted = content_rows(&connection, "converted");
    assert_eq!(
        content_rows(&connection, "untouched"),
        untouched_before,
        "the conversion rewrites no fact of a Session it never reached"
    );
    // The rewrite rebuilds the recorded request's list, so the node holding the
    // base64 image and the node whose `parent` named it stop being reachable.
    // Both go, and the two nodes the rewrite wrote in their place arrive, so the
    // Session's row count is unchanged while the payload is not.
    let image_row = converted_before
        .iter()
        .find(|row| row.contains(&payload()))
        .expect("the fixture holds one node with the inline image");
    let image = image_row.split('|').next().unwrap();
    let child_row = converted_before
        .iter()
        .find(|row| row.contains(&format!("\"parent\":\"{image}\"")))
        .expect("the fixture's next node names the image node");
    for replaced in [image_row, child_row] {
        assert!(
            !converted.contains(replaced),
            "the rewrite replaced {replaced} and the conversion kept it: \
             before={converted_before:?} after={converted:?}"
        );
    }
    assert!(
        converted.iter().all(|row| !row.contains(&payload())),
        "the base64 payload is gone: {converted:?}"
    );
    // A node no reference reaches but the rewrite did not replace stays: the
    // conversion removes what its own rewrite orphaned and leaves the rest to
    // the offline repair.
    let unreached = converted_before
        .iter()
        .find(|row| row.contains("unreachable"))
        .expect("the fixture holds one unreachable node");
    assert!(
        converted.contains(unreached),
        "the conversion removes no row it did not replace: {converted:?}"
    );
    let reachable = reachable(&connection, "converted");
    for hash in &reachable {
        assert!(
            converted
                .iter()
                .any(|row| row.starts_with(&format!("{hash}|"))),
            "the request names {hash}, which the conversion removed"
        );
    }
    drop(connection);

    // The bytes moved into one external file the two Sessions own, so the
    // conversion kept the image rather than dropping it.
    assert_eq!(
        image_files(&database),
        vec![IMAGE.to_vec()],
        "the shared image was written once and is still there"
    );

    let mut exported_after = Vec::new();
    dim_app_server::storage::export_timeline(database.0.clone(), "converted", &mut exported_after)
        .await
        .unwrap();
    // The conversion itself changes two facts: the image source moves from
    // base64 to the file id the store now owns. Nothing else may move, so the
    // exported event count is unchanged and the payload appears in no line.
    let before = String::from_utf8(exported_before).unwrap();
    let after = String::from_utf8(exported_after).unwrap();
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "the reclaim adds and removes no exported line"
    );
    assert!(
        !after.contains(&payload()),
        "the export no longer carries the inline payload"
    );
    // Each branch exports its own copy of the input and of the rebuilt
    // request, so the two image sources appear once per branch.
    assert_eq!(
        after.matches("\"type\":\"file\"").count(),
        4,
        "every exported image source names its file: {after}"
    );
}

/// #1697: a store that already committed the v50 conversion keeps the rows its
/// replacement made unreachable, and `dimcode daemon vacuum` removes them over
/// every Session. The removed payload is what the rewrite returns to space;
/// the file size is what `VACUUM` returns, and the two are reported apart.
#[tokio::test]
async fn the_offline_entry_repairs_a_store_that_already_committed_v50() {
    let database = Database::new();
    drop(v49_store_with_replaced_content(&database));
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let connection = Connection::open(&database.0).unwrap();
    dim_app_server::storage::initialize_session_member_index(&connection, "converted").unwrap();
    dim_app_server::storage::initialize_session_output_id_index(&connection, "converted").unwrap();
    drop(connection);
    let store = SqliteTimelineStore::new(db.clone());
    let call = |text: &str| {
        EventKind::ModelCall(ModelCallData {
            turn_id: "trace-turn".into(),
            request_id: "trace-request".into(),
            request: Arc::new(ModelRequest::new(
                "mock",
                "test",
                vec![Message::text(Role::User, text)],
            )),
            configuration: None,
            prompt: None,
            failure_chain: None,
        })
    };
    let mut traced = record(4, call("final request"));
    traced.trace = Some(TimelineTrace {
        event_id: traced.event.id.clone(),
        original: EventDraft::new(call("original request only in trace")),
        steps: vec![TraceStep {
            handler: "before".into(),
            result: Some(EventDraft::new(call("intermediate request only in trace"))),
        }],
    });
    append(&store, key("converted", "main"), vec![traced])
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    // The state an earlier build committed: the facts name new addresses, and
    // the rows holding the old image bytes are still there. Nothing reads them,
    // so restoring them changes no fact.
    let stale = json!({
        "parent": null,
        "message": {"role": "user", "content": [{
            "type": "image",
            "source": {"type": "base64", "mediaType": "image/png", "data": payload()}
        }]}
    })
    .to_string();
    let connection = Connection::open(&database.0).unwrap();
    for session in ["converted", "shared", "untouched"] {
        insert_content(&connection, session, "node", &stale);
    }
    let bytes_before: i64 = connection
        .query_row(
            "SELECT sum(length(CAST(body AS BLOB))) FROM timeline_content",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let before = content_rows(&connection, "converted");
    let mut exported_before = Vec::new();
    drop(connection);
    dim_app_server::storage::export_timeline(database.0.clone(), "converted", &mut exported_before)
        .await
        .unwrap();

    let vacuumed = vacuum_store(database.0.clone()).unwrap();
    // One stale node in each of the three Sessions, plus the node no reference
    // reached in `converted` and the one in `untouched`: the offline entry
    // covers the whole store, not one Session.
    assert_eq!(vacuumed.reclaimed.rows, 6, "{vacuumed:?}");
    assert!(
        vacuumed.reclaimed.bytes >= (stale.len() as u64) * 3,
        "the removed payload covers the bodies it deleted: {}",
        vacuumed.reclaimed.bytes
    );
    assert!(vacuumed.before > 0, "the report states the size before");

    assert_eq!(
        assert_every_fact_reads(&database, "converted").await,
        7,
        "the repair removes no address a fact names"
    );
    let connection = Connection::open(&database.0).unwrap();
    let bytes_after: i64 = connection
        .query_row(
            "SELECT sum(length(CAST(body AS BLOB))) FROM timeline_content",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        vacuumed.reclaimed.bytes,
        u64::try_from(bytes_before - bytes_after).unwrap(),
        "removed bytes count only content bodies, not their hashes or kinds"
    );
    assert!(
        !content_rows(&connection, "converted")
            .iter()
            .any(|row| row.contains(&payload())),
        "the repaired store holds no base64 payload"
    );
    for row in &before {
        if row.contains(&payload()) || row.contains("unreachable") {
            continue;
        }
        assert!(
            content_rows(&connection, "converted").contains(row),
            "{row} was reachable and must stay"
        );
    }
    assert_eq!(
        marker(&connection),
        [i64::from(STORE_FORMAT)],
        "a repair writes no format marker"
    );
    drop(connection);

    // Repeating it removes nothing: the rows are gone, and the selection that
    // removed them selects nothing.
    let again = vacuum_store(database.0.clone()).unwrap();
    assert_eq!(again.reclaimed.rows, 0, "a second run finds nothing");

    let mut exported_after = Vec::new();
    dim_app_server::storage::export_timeline(database.0.clone(), "converted", &mut exported_after)
        .await
        .unwrap();
    assert_eq!(
        exported_before, exported_after,
        "the repair changes no fact another reader sees"
    );
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let config = store
        .session_config_text("converted")
        .await
        .unwrap()
        .unwrap();
    store
        .fork_session("converted", "fork", 4, &config)
        .await
        .unwrap();
    assert_eq!(
        store
            .read(&key("converted", "main"), 1..=4)
            .await
            .unwrap()
            .iter()
            .map(|record| (
                record.event.clone(),
                serde_json::to_value(&record.trace).unwrap()
            ))
            .collect::<Vec<_>>(),
        store
            .read(&key("fork", "main"), 1..=4)
            .await
            .unwrap()
            .iter()
            .map(|record| (
                record.event.clone(),
                serde_json::to_value(&record.trace).unwrap()
            ))
            .collect::<Vec<_>>(),
        "a fork after reclamation keeps requests and both trace drafts readable"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// #1697: the repair reports a Session whose facts name content the store lost
/// instead of deleting around it, so a damaged store never shrinks into
/// looking healthy.
#[tokio::test]
async fn a_repair_refuses_a_session_that_names_missing_content() {
    let database = Database::new();
    drop(v49_store_with_replaced_content(&database));
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    let reachable = reachable(&connection, "converted");
    let live = reachable.iter().next().unwrap().clone();
    let before: Vec<String> = content_rows(&connection, "converted")
        .into_iter()
        .filter(|row| !row.starts_with(&format!("{live}|")))
        .collect();
    connection
        .execute(
            "DELETE FROM timeline_content WHERE session_id='converted' AND hash=?1",
            [&live],
        )
        .unwrap();
    drop(connection);

    let error = match vacuum_store(database.0.clone()) {
        Ok(_) => panic!("a repair must refuse a Session that names missing content"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("does not hold"),
        "the repair names the damage: {error}"
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        content_rows(&connection, "converted"),
        before,
        "a refused repair removes nothing"
    );
}

/// #1697: the repair removes rows and no external file. A Session file is
/// removed by the sweep that owns it, after its last ownership row is gone, so
/// a conversion's image survives a reclaim that only deleted unreachable rows.
#[tokio::test]
async fn a_repair_removes_rows_and_no_external_file() {
    let database = Database::new();
    drop(v49_store_with_replaced_content(&database));
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    db.close().await.unwrap();
    let files = image_files(&database);
    assert_eq!(
        files,
        vec![IMAGE.to_vec()],
        "the conversion wrote the image"
    );

    // The conversion removed the rows its own rewrite replaced; what is left for
    // the offline entry is `converted`'s unreachable node, the one `shared` was
    // given, and `untouched`'s: three Sessions the fixture left one each.
    let vacuumed = vacuum_store(database.0.clone()).unwrap();
    assert_eq!(vacuumed.reclaimed.rows, 3, "{vacuumed:?}");
    assert_eq!(
        image_files(&database),
        files,
        "the maintenance entry removes no external file"
    );
    let connection = Connection::open(&database.0).unwrap();
    let owners: i64 = connection
        .query_row("SELECT count(*) FROM session_file_owners", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(owners, 2, "the shared image keeps an owner row per Session");
}

/// #1697: the repair reads every Session before it deletes anything, so a store
/// holding one Session that names content it lost is refused as a whole: no
/// Session is shrunk, and every fact of every Session still reads. Once the
/// missing row is back the next run removes all the unreachable rows at once.
#[tokio::test]
async fn a_repair_refuses_the_whole_store_when_one_session_is_damaged() {
    let database = Database::new();
    drop(v49_store_with_replaced_content(&database));
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(49));
    db.close().await.unwrap();

    // `shared` sorts after `converted`, so a per-Session repair would have
    // deleted `converted`'s rows before reaching this damage. Its request names
    // one `tools` row the test then removes.
    let tools = {
        let connection = Connection::open(&database.0).unwrap();
        insert_content(
            &connection,
            "shared",
            "node",
            &json!({"parent": null, "message": {"role": "user", "content": "kept"}}).to_string(),
        );
        let tools = insert_content(&connection, "shared", "tools", "[]");
        insert_event(
            &connection,
            "shared",
            3,
            "model/call",
            json!({"turn_id": "t", "request_id": "r",
                   "request": {"providerId": "m", "modelId": "m", "messages": []}}),
        );
        connection
            .execute(
                "UPDATE timeline_events SET body=json_set(body, '$.record.refs', json(?1)) \
                 WHERE session_id='shared' AND sequence=3",
                [json!({"messages": [], "tools": tools}).to_string()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE timeline_branches SET sequence=3 WHERE session_id='shared' AND branch_id='main'",
                [],
            )
            .unwrap();
        tools
    };
    // A stale row in the first Session, so a per-Session repair would have real
    // work to commit before it reached the damaged one.
    let stale = json!({
        "parent": null,
        "message": {"role": "user", "content": [{
            "type": "image",
            "source": {"type": "base64", "mediaType": "image/png", "data": payload()}
        }]}
    })
    .to_string();
    let stored_before = {
        let connection = Connection::open(&database.0).unwrap();
        insert_content(&connection, "converted", "node", &stale);
        let rows: Vec<(String, Vec<String>)> = ["converted", "shared", "untouched"]
            .into_iter()
            .map(|session| (session.to_owned(), content_rows(&connection, session)))
            .collect();
        connection
            .execute(
                "DELETE FROM timeline_content WHERE session_id='shared' AND hash=?1",
                [&tools],
            )
            .unwrap();
        rows
    };

    let interrupted = match vacuum_store(database.0.clone()) {
        Ok(_) => panic!("a store naming missing content must refuse the repair"),
        Err(error) => error,
    };
    assert!(
        interrupted.to_string().contains("does not hold"),
        "the repair names the damage: {interrupted}"
    );

    // No Session was touched, the first one included: the damage was found
    // before the first delete, so the store is not partly shrunk around it.
    let connection = Connection::open(&database.0).unwrap();
    for (session, rows) in &stored_before {
        let expected: Vec<String> = rows
            .iter()
            .filter(|row| !row.starts_with(&tools))
            .cloned()
            .collect();
        assert_eq!(
            content_rows(&connection, session),
            expected,
            "a refused repair removes nothing from {session}"
        );
    }
    drop(connection);
    for session in ["converted", "untouched"] {
        assert_every_fact_reads(&database, session).await;
    }

    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute("DELETE FROM timeline_content WHERE session_id='shared'", [])
        .unwrap();
    drop(connection);
    let missing_all = vacuum_store(database.0.clone()).unwrap_err();
    assert!(
        missing_all.to_string().contains("does not hold"),
        "a Session whose content rows are all missing must still be checked: {missing_all}"
    );
    let connection = Connection::open(&database.0).unwrap();
    for (session, rows) in &stored_before {
        let expected = if session == "shared" {
            &[][..]
        } else {
            rows.as_slice()
        };
        assert_eq!(
            content_rows(&connection, session),
            expected,
            "missing all content in one Session must prevent deletion in {session}"
        );
    }
    drop(connection);

    {
        let connection = Connection::open(&database.0).unwrap();
        let (_, rows) = stored_before
            .iter()
            .find(|(session, _)| session == "shared")
            .unwrap();
        for row in rows {
            let mut fields = row.splitn(3, '|');
            let hash = fields.next().unwrap();
            let kind = fields.next().unwrap();
            let body = fields.next().unwrap();
            connection
                .execute(
                    "INSERT INTO timeline_content(session_id,hash,kind,body) VALUES ('shared',?1,?2,?3)",
                    params![hash, kind, body],
                )
                .unwrap();
        }
    }
    let finished = vacuum_store(database.0.clone()).unwrap();
    // `converted` held the stale node and one unreachable node, `shared` the
    // stale one and the node the test added, `untouched` its own: five rows, all
    // in one run once the damage is gone.
    assert_eq!(finished.reclaimed.rows, 5, "{finished:?}");
    let connection = Connection::open(&database.0).unwrap();
    for session in ["converted", "shared", "untouched"] {
        let reachable = reachable(&connection, session);
        let kept = content_rows(&connection, session);
        for row in &kept {
            assert!(
                reachable.contains(row.split('|').next().unwrap()),
                "{session} kept {row}, which nothing reaches"
            );
        }
    }
    drop(connection);
    assert_eq!(assert_every_fact_reads(&database, "untouched").await, 1);
    let again = vacuum_store(database.0.clone()).unwrap();
    assert_eq!(again.reclaimed.rows, 0, "a finished repair selects nothing");
}
