//! Timeline export acceptance (ADR 005 D3; timeline-export design §13):
//! `dim-app-server --export-timeline` reads a store a real App Server wrote,
//! live and from copies, without changing it.
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use dim_agent_runtime::{Timeline, TimelineKey, TimelineStore};
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore};
use rusqlite::OpenFlags;
use rutis::host::CtxHost;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
};

const SUMMARIZER: &str = "You summarize a coding session";
const CREDENTIAL: &str = "credential-marker-7f3a";
const SETTING: &str = "settings-marker-91c2";

/// An OpenAI-compatible provider: the first conversation request reads a
/// file, the summarizer gets a summary, every other request an answer.
#[derive(Default)]
struct Provider {
    requests: AtomicUsize,
}
async fn completion(State(provider): State<Arc<Provider>>, Json(body): Json<Value>) -> String {
    let chunk = |delta: Value, finish: &str| {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"id":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        )
    };
    if body.to_string().contains(SUMMARIZER) {
        return chunk(json!({"content":"SUMMARY"}), "stop");
    }
    if provider.requests.fetch_add(1, Ordering::SeqCst) == 0 {
        return chunk(
            json!({"tool_calls":[{"index":0,"id":"call-read","type":"function","function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}}]}),
            "tool_calls",
        );
    }
    chunk(json!({"content":"ok"}), "stop")
}

async fn launch_product(directory: &Path) -> (tokio::process::Child, PathBuf) {
    let socket = directory.join("ipc/server.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args(["--product-home", directory.join("home").to_str().unwrap()])
        .args(["--unix", socket.to_str().unwrap(), "--no-ws"])
        .args(["--home", directory.join("state").to_str().unwrap()])
        .env("HOME", directory)
        .current_dir(directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "server startup failed");
    (child, socket)
}
async fn connect(socket: &Path) -> Client {
    let mut client = Client {
        transport: Transport::Unix(BufReader::new(UnixStream::connect(socket).await.unwrap())),
        id: 0,
        notices: vec![],
        instance: Value::Null,
        features: Value::Null,
    };
    assert_ok(
        &client
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await,
    );
    client
}
/// Submits `text` on `branch` and returns its sequence once idle.
async fn turn(client: &mut Client, sid: &Value, branch: &str, text: &str) -> u64 {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":branch,"text":text}),
            )
            .await,
    );
    idle(client, sid, branch).await
}
async fn idle(client: &mut Client, sid: &Value, branch: &str) -> u64 {
    let idle = client
        .rpc("session.idle", json!({"sessionId":sid,"branchId":branch}))
        .await;
    assert_ok(&idle);
    idle["result"]["sequence"].as_u64().unwrap()
}

/// Runs the binary's export mode.
async fn export(database: &Path, session: &str) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args(["--export-timeline", session, "--database"])
        .arg(database)
        .output()
        .await
        .unwrap()
}
/// The lines of a successful export.
async fn exported(database: &Path, session: &str) -> Vec<Value> {
    let output = export(database, session).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
/// A failed export that wrote nothing; its error line.
async fn rejected(database: &Path, session: &str) -> String {
    let output = export(database, session).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}
/// The branch and event lines of `session` as the TimelineStore reads them.
async fn stored(database: &Path, session: &str) -> Vec<Value> {
    let db = SqliteStorage::open(database.to_owned()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let branches = store.branches(session).await.unwrap();
    let mut lines: Vec<Value> = branches
        .iter()
        .map(|branch| json!({"kind":"branch","info":branch.info,"head":branch.sequence}))
        .collect();
    for branch in &branches {
        let key = TimelineKey {
            session_id: session.into(),
            branch_id: branch.info.id.clone(),
        };
        let events = store
            .read_segments(session, vec![(key.branch_id.clone(), 1..=branch.sequence)])
            .await
            .unwrap()
            .remove(0);
        for record in events {
            let trace = store.trace(&key, record.event.sequence).await.unwrap();
            assert_eq!(json!(trace), json!(record.trace));
            lines.push(
                json!({"kind":"event","branch_id":key.branch_id,"event":record.event,
                "trace":record.trace,"batch_end":record.batch_end}),
            );
        }
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    lines
}
fn kind(line: &Value) -> String {
    format!(
        "{}/{}",
        line["event"]["type"].as_str().unwrap(),
        line["event"]["name"].as_str().unwrap()
    )
}
/// Copies `database` to `path` with SQLite's backup API.
fn backup(database: &Path, path: &Path) {
    rusqlite::Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .backup(rusqlite::MAIN_DB, path, None)
        .unwrap();
}
/// A copy of `database` changed by `sql`.
fn changed(database: &Path, path: &Path, sql: &str) {
    std::fs::copy(database, path).unwrap();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}
/// Removes what v26 adds from a copy, so that its schema is the v25 one: an
/// older daemon's store, which the read-only export still reads.
/// Every format before v30 has no Session origin column; the table is rebuilt
/// at its v27 text, which the format gate compares character by character.
fn without_origin_column(database: &Path) {
    rusqlite::Connection::open(database)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             CREATE TEMP TABLE sessions_rows AS SELECT * FROM agent_sessions;
             DROP TABLE agent_sessions;
             CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id));
             CREATE INDEX agent_sessions_by_leader ON agent_sessions(leader_session_id);
             INSERT INTO agent_sessions(session_id, config, created_at_ms, leader_session_id) SELECT session_id, config, created_at_ms, leader_session_id FROM sessions_rows;
             DROP TABLE sessions_rows;",
        )
        .unwrap();
}

/// Every format before v31 has no reading switches on `timeline_events`: the
/// table is rebuilt at its v30 text, which the format gate compares character
/// by character, and its other indexes are recreated.
fn without_switch_columns(database: &Path) {
    let connection = rusqlite::Connection::open(database).unwrap();
    let indexes: Vec<String> = connection
        .prepare(
            "SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name='timeline_events' \
             AND name NOT IN ('timeline_events_render', 'timeline_events_state', \
             'timeline_events_positions') AND sql IS NOT NULL",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             CREATE TEMP TABLE events_rows AS SELECT session_id, branch_id, sequence, last_sequence, kind, body, event_id FROM timeline_events;
             DROP TABLE timeline_events;
             CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id));
             INSERT INTO timeline_events SELECT * FROM events_rows;
             DROP TABLE events_rows;",
        )
        .unwrap();
    for index in indexes {
        connection.execute_batch(&index).unwrap();
    }
}

fn without_v26_columns(database: &Path) {
    rusqlite::Connection::open(database)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             CREATE TEMP TABLE agents_rows AS SELECT * FROM agents;
             DROP TABLE agents;
             CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)));
             CREATE UNIQUE INDEX agents_default_unique ON agents(is_default) WHERE is_default = 1;
             INSERT INTO agents(agent_id, profile, is_default) SELECT agent_id, profile, is_default FROM agents_rows;
             DROP TABLE agents_rows;
             CREATE TEMP TABLE sessions_rows AS SELECT * FROM agent_sessions;
             DROP TABLE agent_sessions;
             CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0));
             INSERT INTO agent_sessions(session_id, config, created_at_ms) SELECT session_id, config, created_at_ms FROM sessions_rows;
             DROP TABLE sessions_rows;",
        )
        .unwrap();
}

fn marker(database: &Path) -> i64 {
    rusqlite::Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT version FROM agent_store_format", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeline_export_reads_a_live_product_home_and_its_copies() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = PathBuf::from("/tmp")
            .join(format!("dim-export-{}", uuid::Uuid::new_v4().simple()));
        for path in ["home", "work", "copies"] {
            std::fs::create_dir_all(directory.join(path)).unwrap();
        }
        std::fs::set_permissions(directory.join("home"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::write(directory.join("work/note.txt"), "note contents\n").unwrap();
        let database = directory.join("state/dimcode.sqlite");
        let (mut daemon, socket) = launch_product(&directory).await;
        let mut client = connect(&socket).await;

        let capabilities = dim_agent_types::CatalogCapabilities::with_limits(4_400, 200);
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000004","displayName":"Window","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"auth":{"type":"bearer"},"credential":{"action":"set","value":CREDENTIAL}},"models":[{"modelId":"small","displayName":"small","status":"active","capabilities":capabilities}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc("provider.select", json!({"providerId":"018ec73f-0000-7000-8000-000000000004","modelId":"small"}))
                .await,
        );
        let catalog = client.rpc("mcp.catalog", json!({})).await;
        assert_ok(&catalog);
        assert_ok(&client.rpc("mcp.save", json!({"source":"user","id":"marker",
            "expectedVersion":catalog["result"]["userRevision"],
            "configuration":{"command":"never-run-this-test-command","env":{"TOKEN":SETTING},"enabled":false}})).await);

        let created = client
            .rpc("session.create", json!({"cwd":directory.join("work"),"tools":["read","branch"]}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        let session = sid.as_str().unwrap().to_owned();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        turn(&mut client, &sid, "main", "read the note").await;
        turn(&mut client, &sid, "main", &format!("large {}", "x".repeat(2400))).await;
        turn(&mut client, &sid, "main", "small").await;
        let compacted = client
            .rpc("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&compacted);
        assert_eq!(compacted["result"]["status"], "completed", "{compacted}");
        let at = idle(&mut client, &sid, "main").await;
        let branched = client
            .rpc(
                "session.branch_create",
                json!({"sessionId":sid,"parentBranchId":"main","at":at,"prompt":"child"}),
            )
            .await;
        assert_ok(&branched);
        let child = branched["result"]["branch"]["branchId"].as_str().unwrap().to_owned();
        idle(&mut client, &sid, &child).await;

        let copy = directory.join("copies/backup.sqlite");
        backup(&database, &copy);
        let live = exported(&database, &session).await;
        assert_eq!(exported(&copy, &session).await, live);
        let text = live.iter().map(Value::to_string).collect::<String>();
        assert!(!text.contains(CREDENTIAL) && !text.contains(SETTING));
        assert!(rejected(&database, "missing-session").await.contains("Session missing-session is not in"));

        assert_eq!(
            live[0],
            json!({"kind":"header","export":1,"session_id":session,"store_format":31,"sensitive":true})
        );
        let branches: Vec<_> = live.iter().filter(|line| line["kind"] == "branch").collect();
        assert_eq!(branches.len(), 2);
        let info = &branches[0]["info"];
        assert_eq!((&info["id"], &info["parent"]), (&json!(child), &json!("main")));
        assert!(info["branch_point"].is_u64() && info["create_anchor"].as_u64() > Some(at));
        assert_eq!(branches[1]["info"], json!({"id":"main"}));
        let events: Vec<_> = live.iter().filter(|line| line["kind"] == "event").collect();
        assert_eq!(
            live.last().unwrap(),
            &json!({"kind":"end","branches":2,"events":events.len()})
        );
        assert_eq!(live.len(), 1 + branches.len() + events.len() + 1);
        for branch in &branches {
            let id = &branch["info"]["id"];
            let sequences: Vec<_> = events
                .iter()
                .filter(|line| &line["branch_id"] == id)
                .map(|line| line["event"]["sequence"].as_u64().unwrap())
                .collect();
            assert_eq!(sequences, (1..=branch["head"].as_u64().unwrap()).collect::<Vec<_>>());
        }
        let main: Vec<_> = events.iter().filter(|line| line["branch_id"] == "main").collect();
        for expected in ["model/call", "step/tool-use", "step/tool-result", "branch/create"] {
            assert!(main.iter().any(|line| kind(line) == expected), "{expected}");
        }
        assert!(main.iter().any(|line| kind(line) == "context/compaction"
            && line["event"]["data"]["status"] == "completed"));
        let create = main
            .iter()
            .find(|line| line["event"]["sequence"] == branches[0]["info"]["create_anchor"])
            .unwrap();
        assert_eq!(kind(create), "branch/create");
        assert_eq!(create["event"]["data"]["branch"], child);
        assert!(events.iter().any(|line| line["branch_id"] == child && kind(line) == "model/call"));

        assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
        assert!(daemon.wait().await.unwrap().success());
        let bytes = std::fs::read(&database).unwrap();
        let stopped = exported(&database, &session).await;
        assert_eq!(std::fs::read(&database).unwrap(), bytes);
        assert_eq!(stopped[0], live[0]);
        assert_eq!(stopped.len(), live.len() + 2);
        let stored_markers: (i64, i64) = rusqlite::Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM providers WHERE record LIKE ?1), (SELECT count(*) FROM settings_documents WHERE document LIKE ?2)",
                [format!("%{CREDENTIAL}%"), format!("%{SETTING}%")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_markers, (1, 1));

        let readonly = directory.join("copies/readonly.sqlite");
        std::fs::copy(&database, &readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(exported(&readonly, &session).await, stopped);

        for (version, dropped, end_index) in [
            (13, "DROP TABLE session_file_owners; DROP TABLE session_files; DROP TABLE session_metadata;", true),
            (17, "", true),
            (19, "", true),
            (21, "", true),
            (22, "", true),
            (23, "", true),
            (24, "", true),
            (25, "", false),
            (26, "", false),
            (27, "", false),
            (28, "", false),
            (29, "", false),
        ] {
            let older = directory.join(format!("copies/v{version}.sqlite"));
            let catalog = if version < 23 {
                "DROP TABLE session_catalog;"
            } else {
                ""
            };
            // Every format before v28 lacks the grants table.
            let grants = if version < 28 {
                "DROP TABLE session_grants;"
            } else {
                ""
            };
            // Every format before v29 lacks the output ownership table.
            let output = if version < 29 {
                "DROP TABLE session_output_owners;"
            } else {
                ""
            };
            // v25 dropped the end-sequence index; every earlier version has it.
            let end_index = if end_index {
                "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence);"
            } else {
                ""
            };
            changed(&database, &older, &format!("{catalog} {grants} {output} {dropped} {end_index} DROP TABLE agent_store_format;
                CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {version}));
                INSERT INTO agent_store_format VALUES ({version});"));
            without_switch_columns(&older);
            if version < 30 {
                without_origin_column(&older);
            }
            if version < 27 {
                without_v26_columns(&older);
            }
            let bytes = std::fs::read(&older).unwrap();
            let read = exported(&older, &session).await;
            assert_eq!(read[0]["store_format"], version, "v{version}");
            assert_eq!(read[1..], stopped[1..], "v{version}");
            assert_eq!(std::fs::read(&older).unwrap(), bytes, "v{version}");
            assert_eq!(marker(&older), version, "v{version}");
        }

        for (name, sql) in [
            ("newer.sqlite", "DROP TABLE agent_store_format;
                CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 32));
                INSERT INTO agent_store_format VALUES (32);"),
            ("changed.sqlite", "CREATE TABLE extra (id TEXT)"),
        ] {
            let path = directory.join("copies").join(name);
            changed(&database, &path, sql);
            let bytes = std::fs::read(&path).unwrap();
            assert!(rejected(&path, &session).await.contains("unsupported Timeline storage format"), "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
        }
        assert!(rejected(&directory.join("copies/absent.sqlite"), &session).await.contains("No dimcode database at"));

        assert_eq!(stopped[1..stopped.len() - 1], stored(&database, &session).await[..]);
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// Tags every input message, so its events carry a before trace.
struct TagInput {
    timeline: rutis::host::Key<Timeline>,
}
impl TagInput {
    fn new(names: &dim_agent_runtime::Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for TagInput {
    fn name(&self) -> &str {
        "TagInput"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            timeline.before(ctx, "tag-input", &["input/message"], |mut draft| {
                draft.tags.get_or_insert_default().push("tagged".into());
                Ok(draft)
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeline_export_carries_before_traces() {
    let home = PathBuf::from("/tmp").join(format!("dim-export-{}", uuid::Uuid::new_v4().simple()));
    let database = home.join("dimcode.sqlite");
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder.require_feature(
        Feature::new("TagInput").branch("TagInput", |names| Ok(Box::new(TagInput::new(names)?))),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(home.join("server.sock")),
            database_path: Some(database.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].clone();
    let session = sid.as_str().unwrap().to_owned();
    turn(&mut client, &sid, "main", "hello").await;

    let live = exported(&database, &session).await;
    let inputs: Vec<_> = live
        .iter()
        .filter(|line| line["kind"] == "event" && kind(line) == "input/message")
        .collect();
    assert!(!inputs.is_empty());
    for line in inputs {
        let event_id = &line["event"]["id"];
        assert_eq!(line["event"]["tags"], json!(["tagged"]));
        assert_eq!(line["trace"]["event_id"], *event_id);
        assert!(line["trace"]["original"].get("tags").is_none());
        assert_eq!(line["trace"]["steps"][0]["handler"], "tag-input");
    }
    server.shutdown();
    server.join().await.unwrap();
    let stopped = exported(&database, &session).await;
    assert_eq!(stopped[2], live[2]);
    assert_eq!(
        stopped[1..stopped.len() - 1],
        stored(&database, &session).await[..]
    );
    std::fs::remove_dir_all(home).unwrap();
}
