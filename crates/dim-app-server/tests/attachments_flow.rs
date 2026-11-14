//! Attachments ([design](../../../docs/design/plugins/Attachments.md),
//! Appendix D): `attachment.put` through public RPC on an App Server with
//! native storage and local execution, each in a disposable home. Outcomes
//! RPC cannot trigger reliably (an unload, a Session record gone before the
//! commit, an inactive node) are driven at the `sessionMethods` contract Rpc
//! dispatches to, over the real SessionStore.
#![cfg(unix)]

use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    adapters::MockTools,
    defaults,
    file_storage::FileStoragePlugin,
    host::SessionFilesHost,
    methods::{MethodProvider, RpcError, SessionMethods},
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig, testing::MinimalHost};
use dim_agent_types::EventKind;
use dim_app_server::{
    RunningServer, ServerConfig, serve,
    storage::{SessionStore, SessionStorePlugin, SqliteStoragePlugin},
};
use rusqlite::Connection;
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use tokio_util::sync::CancellationToken;

const MIB: usize = 1024 * 1024;

/// A disposable home: the database directory, a working directory and the
/// socket. Never the product home.
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = PathBuf::from("/tmp").join(format!("dim-att-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("work")).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    fn database(&self) -> PathBuf {
        self.0.join("dimcode.sqlite")
    }
    fn files(&self) -> PathBuf {
        self.0.join("dimcode.sqlite.session-files")
    }
    /// The entries of the Session-files directory, without FileStorage's own.
    fn entries(&self) -> Vec<String> {
        let mut entries: Vec<String> = match std::fs::read_dir(self.files()) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| !name.starts_with(".dim-file-storage-"))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => panic!("{error}"),
        };
        entries.sort();
        entries
    }
    /// The files a Session owns in the native store.
    fn owned(&self, session: &str) -> i64 {
        Connection::open(self.database())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM session_file_owners WHERE session_id = ?1",
                [session],
                |row| row.get(0),
            )
            .unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Serves from `home`: with native storage and local execution (the
/// scripted model with `read`) when `native`, otherwise the default host
/// without native storage.
async fn launch(home: &Home, native: bool) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let mut config = ServerConfig {
        unix_path: Some(home.0.join("server.sock")),
        websocket_address: None,
        ..Default::default()
    };
    if native {
        workspace_tools::install(
            &mut builder,
            WorkspaceToolsConfig {
                boundary: None,
                output_root: home.0.join("command-output"),
            },
        );
        let models = AimuxModels::from_config(
            serde_json::from_value::<ModelConfigFile>(json!({
                "defaultModel": {"providerId": "local", "modelId": "scripted"},
                "providers": [{"id": "local", "kind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "models": [{"id": "scripted", "contextWindow": 128000, "maxOutputTokens": 4096, "config": {"maxTokens": 64}}]}]
            }))
            .unwrap(),
        )
        .unwrap();
        config.target_home = Some(home.0.clone());
        config.model_catalog = Some(Arc::new(models));
    } else {
        defaults::tool_host(&mut builder, Arc::new(MockTools));
    }
    serve(builder, config).await.unwrap()
}

struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
}
impl Client {
    async fn connect(server: &RunningServer) -> Self {
        let stream = UnixStream::connect(server.unix_path.as_ref().unwrap())
            .await
            .unwrap();
        let mut client = Self {
            stream: BufReader::new(stream),
            id: 0,
        };
        let hello = client
            .rpc(
                "hello",
                json!({"protocolVersion": 3, "clientKind": "other"}),
            )
            .await;
        assert_eq!(hello["result"]["protocolVersion"], 3, "{hello}");
        client
    }
    /// The response to one request; notifications in between are skipped.
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let request = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        let stream = self.stream.get_mut();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let mut line = String::new();
                assert_ne!(self.stream.read_line(&mut line).await.unwrap(), 0);
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"] == self.id {
                    return value;
                }
            }
        })
        .await
        .expect("response deadline")
    }
    async fn create(&mut self, cwd: &std::path::Path) -> String {
        let created = self
            .rpc("session.create", json!({"cwd": cwd, "tools": ["read"]}))
            .await;
        created["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_owned()
    }
}

/// `(JSON-RPC code, appError)` of a failed reply.
fn failure(reply: &Value) -> (i64, &str) {
    (
        reply["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("{reply}")),
        reply["error"]["data"]["appError"]
            .as_str()
            .unwrap_or_default(),
    )
}

/// Appendix D item 1: the reply is SessionStore's, the path is under the
/// database directory, and a scripted model reads it with `read` without an
/// approval request. Every put is a new file; a given name is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_put_stores_a_session_file_the_model_reads_without_approval() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home.0.join("work")).await;
    let methods = client
        .rpc("session.methods", json!({"sessionId": sid}))
        .await;
    assert!(
        methods["result"]["methods"]
            .as_array()
            .unwrap()
            .contains(&json!({"name": "attachment.put", "mutating": true})),
        "{methods}"
    );

    let text = "first line\nsecond line: naïve café ✓\n";
    let reply = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": text}))
        .await;
    let id = reply["result"]["attachmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("{reply}"))
        .to_owned();
    let path = home.files().join(&id).join("paste.txt");
    assert_eq!(
        reply["result"],
        json!({"attachmentId": id, "path": path, "bytes": text.len()})
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": sid, "branchId": "main", "text": format!("tool:read {}", json!({"path": path}))}),
        )
        .await;
    assert!(submitted.get("error").is_none(), "{submitted}");
    let idle = client
        .rpc(
            "session.idle",
            json!({"sessionId": sid, "branchId": "main"}),
        )
        .await;
    assert!(idle.get("error").is_none(), "{idle}");
    let history = chain_events(
        &server.agent().unwrap().get_session(&sid).unwrap(),
        "main",
        ..,
    )
    .await
    .unwrap();
    assert!(
        !history
            .iter()
            .any(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
    );
    let results: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results.clone()),
            _ => None,
        })
        .flatten()
        .map(|call| call.result)
        .collect();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_ne!(results[0].is_error, Some(true), "{:?}", results[0]);
    assert!(
        serde_json::to_string(&results[0].content)
            .unwrap()
            .contains("second line: naïve café ✓"),
        "{:?}",
        results[0]
    );

    let again = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": text}))
        .await;
    let again_id = again["result"]["attachmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("{again}"));
    assert_ne!(again_id, id);
    assert_eq!(
        again["result"]["path"],
        json!(home.files().join(again_id).join("paste.txt"))
    );
    let named = client
        .rpc(
            "attachment.put",
            json!({"sessionId": sid, "name": "notes.md", "text": "# notes"}),
        )
        .await;
    let named_id = named["result"]["attachmentId"].as_str().unwrap();
    assert_eq!(
        named["result"]["path"],
        json!(home.files().join(named_id).join("notes.md"))
    );
    assert_eq!(home.owned(&sid), 3);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Appendix D item 2 and the error mapping of A.2 that RPC reaches:
/// Attachments' own checks, SessionStore's name rule and storage failure,
/// and a closed Session. A rejected put stores nothing; 10 MiB is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_put_rejects_bad_requests_and_reports_store_failures() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home.0.join("work")).await;

    std::fs::write(home.files(), b"not a directory").unwrap();
    let reply = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": "x"}))
        .await;
    assert_eq!(failure(&reply), (-32000, "storage_failed"));
    assert_eq!(home.owned(&sid), 0);
    std::fs::remove_file(home.files()).unwrap();
    let retried = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": "x"}))
        .await;
    assert!(retried.get("error").is_none(), "{retried}");

    let entries = home.entries();
    for params in [
        json!({"sessionId": sid}),
        json!({"sessionId": sid, "text": 5}),
        json!({"sessionId": sid, "text": "x", "name": 7}),
        json!({"sessionId": sid, "text": "x", "lines": 1}),
    ] {
        let reply = client.rpc("attachment.put", params.clone()).await;
        assert_eq!(failure(&reply), (-32602, "invalid_params"), "{params}");
    }
    let empty = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": ""}))
        .await;
    assert_eq!(failure(&empty), (-32000, "attachment_invalid"));
    for name in ["../x", ".hidden", "AUTH.JSON"] {
        let reply = client
            .rpc(
                "attachment.put",
                json!({"sessionId": sid, "name": name, "text": "x"}),
            )
            .await;
        assert_eq!(failure(&reply), (-32000, "attachment_invalid"), "{name}");
    }
    let over = client
        .rpc(
            "attachment.put",
            json!({"sessionId": sid, "text": "a".repeat(10 * MIB + 1)}),
        )
        .await;
    assert_eq!(failure(&over), (-32000, "attachment_too_large"));
    assert_eq!(home.entries(), entries);
    assert_eq!(home.owned(&sid), 1);

    let limit = client
        .rpc(
            "attachment.put",
            json!({"sessionId": sid, "text": "a".repeat(10 * MIB)}),
        )
        .await;
    assert_eq!(limit["result"]["bytes"], 10 * MIB, "{}", limit["error"]);

    let closed = client.rpc("session.close", json!({"sessionId": sid})).await;
    assert!(closed.get("error").is_none(), "{closed}");
    let reply = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": "x"}))
        .await;
    assert_eq!(failure(&reply), (-32000, "session_closed"));
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Appendix D item 3: without native storage the host does not install
/// Attachments, so the name lies in no recorded namespace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_without_native_storage_answers_unknown_method() {
    let home = Home::new();
    let server = launch(&home, false).await;
    let mut client = Client::connect(&server).await;
    let created = client
        .rpc("session.create", json!({"cwd": std::env::temp_dir()}))
        .await;
    let sid = created["result"]["sessionId"].as_str().unwrap();
    let reply = client
        .rpc("attachment.put", json!({"sessionId": sid, "text": "x"}))
        .await;
    assert_eq!(failure(&reply), (-32601, "unknown_method"));
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The native store and an Agent with Attachments in one process, without
/// Rpc, so a test controls the calling load's token. `root` holds the store
/// and the node's FileStorage; the Agent's host has the store's
/// `sessionFiles` as a process value.
struct Contract {
    root: rutis::Ctx,
    store: Arc<SessionStore>,
    agent: MinimalHost,
}
impl Contract {
    async fn mount(home: &Home, file_storage: bool) -> Self {
        let root = rutis::Ctx::root().unwrap();
        if file_storage {
            (&root.plugin(FileStoragePlugin::new(vec![home.0.clone()])))
                .await
                .unwrap();
        }
        (&root.plugin(SqliteStoragePlugin::new(home.database())))
            .await
            .unwrap();
        let catalog = dim_app_server::catalog();
        let build = rutis_loader::resolve::Build::default();
        let names = dim_agent_runtime::Names::copy(&catalog, &build);
        (&root.plugin(SessionStorePlugin::new(&names, 0).unwrap()))
            .await
            .unwrap();
        let store = root.get::<SessionStore>().unwrap();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(store.timeline_store());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        dim_agent_attachments::install(&mut builder);
        builder.process_value(root.get::<SessionFilesHost>().unwrap());
        let agent = MinimalHost::start(&builder).await.unwrap();
        Self { root, store, agent }
    }
    async fn session(&self) -> (String, Arc<MethodProvider>) {
        let session = self
            .agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let methods = session
            .session_service::<SessionMethods>()
            .unwrap()
            .provider("attachment.put")
            .unwrap();
        (session.id().to_owned(), methods)
    }
    async fn close(self) {
        self.agent.close().await.unwrap();
        self.root.shutdown().await.unwrap();
    }
}
/// One `attachment.put` admitted under the calling load's token `stopping`.
async fn put(
    methods: &Arc<MethodProvider>,
    session: &str,
    stopping: CancellationToken,
) -> Result<Value, RpcError> {
    methods
        .accept(
            "attachment.put".into(),
            json!({"sessionId": session, "text": "pasted"}),
            None,
            stopping,
            |_| {},
        )
        .unwrap()
        .reply
        .await
        .unwrap()
}
/// `(JSON-RPC code, appError)` of a failed put.
fn rejected(reply: Result<Value, RpcError>) -> (i32, String) {
    let error = reply.expect_err("the put was rejected");
    (error.code, error.data.unwrap().app_error)
}

/// A.2 `cancelled`: an unload fired the call's token before the put
/// committed. The drained call answers `service_reloading` and nothing is
/// stored, so a retry is safe and stores the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_during_a_put_answers_service_reloading_and_stores_nothing() {
    let home = Home::new();
    let contract = Contract::mount(&home, true).await;
    let (sid, methods) = contract.session().await;
    let unloading = CancellationToken::new();
    unloading.cancel();
    assert_eq!(
        rejected(put(&methods, &sid, unloading).await),
        (-32000, "service_reloading".into())
    );
    assert_eq!(home.owned(&sid), 0);
    assert_eq!(home.entries(), Vec::<String>::new());
    let retried = put(&methods, &sid, CancellationToken::new()).await.unwrap();
    assert!(retried["attachmentId"].is_string(), "{retried}");
    assert_eq!(home.owned(&sid), 1);
    contract.close().await;
}

/// A.2 `not_found`: the Session's record is gone before the commit, here by
/// a creation rollback under the loaded Session, so nothing is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_record_gone_before_the_commit_answers_session_closed() {
    let home = Home::new();
    let contract = Contract::mount(&home, true).await;
    let (sid, methods) = contract.session().await;
    contract
        .store
        .timeline_store()
        .rollback_session(&sid)
        .await
        .unwrap();
    assert_eq!(
        rejected(put(&methods, &sid, CancellationToken::new()).await),
        (-32000, "session_closed".into())
    );
    assert_eq!(home.owned(&sid), 0);
    assert_eq!(home.entries(), Vec::<String>::new());
    let _ = contract.agent.close().await;
    contract.root.shutdown().await.unwrap();
}

/// A.2 `node_unavailable`: the node's FileStorage is not active, so nothing
/// is stored and Attachments stays loaded to serve a later retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inactive_node_storage_answers_node_unavailable() {
    let home = Home::new();
    let contract = Contract::mount(&home, false).await;
    let (sid, methods) = contract.session().await;
    assert_eq!(
        rejected(put(&methods, &sid, CancellationToken::new()).await),
        (-32000, "node_unavailable".into())
    );
    assert_eq!(home.owned(&sid), 0);
    assert!(!home.files().exists());
    assert!(
        contract
            .agent
            .get_session(&sid)
            .unwrap()
            .session_service::<SessionMethods>()
            .is_ok_and(|methods| methods.provider("attachment.put").is_some())
    );
    contract.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_image_is_a_session_reference_and_reads_are_scoped() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let session = client.create(&home.0.join("work")).await;
    let other = client.create(&home.0.join("work")).await;
    let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR42mP4zwAE/xkgFAAb8gP9PpddpAAAAABJRU5ErkJggg==").unwrap();
    let source = home.0.join("input.png");
    std::fs::write(&source, &bytes).unwrap();
    let imported = client
        .rpc(
            "attachment.import",
            json!({"sessionId":session,"path":source,"image":true}),
        )
        .await;
    assert!(imported.get("error").is_none(), "{imported}");
    let image = &imported["result"]["image"];
    assert_eq!(image["type"], "file");
    assert_eq!(image["mediaType"], "image/png");
    assert!(image.get("data").is_none());
    std::fs::remove_file(source).unwrap();
    let read = client
        .rpc(
            "attachment.read",
            json!({"sessionId":session,"fileId":image["fileId"],"offset":0}),
        )
        .await;
    assert_eq!(
        STANDARD
            .decode(read["result"]["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    assert!(
        client
            .rpc(
                "attachment.read",
                json!({"sessionId":other,"fileId":image["fileId"],"offset":0})
            )
            .await
            .get("error")
            .is_some()
    );
    assert!(
        client
            .rpc(
                "session.submit",
                json!({"sessionId":other,"user":{"content":[{"type":"image","source":image}]}})
            )
            .await
            .get("error")
            .is_some()
    );
    server.shutdown();
}
