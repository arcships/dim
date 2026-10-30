//! `session.delete` through the wire (S5c §3, acceptance 1, 2 and 9) on
//! disposable native stores.
use super::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Tables that hold rows of a Session, in the order a delete clears them.
const SESSION_TABLES: [&str; 9] = [
    "session_file_owners",
    "timeline_snapshots",
    "timeline_events",
    "timeline_branches",
    "timeline_content",
    "session_metadata",
    "session_catalog",
    "session_owners",
    "agent_sessions",
];

/// A disposable database directory below the system temporary directory.
pub(super) struct Store(pub PathBuf);
impl Store {
    pub fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("dim-s5c-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    pub fn config(&self) -> ServerConfig {
        ServerConfig {
            database_path: Some(self.0.join("dimcode.sqlite")),
            ..Default::default()
        }
    }
    pub fn connection(&self) -> Connection {
        let connection = Connection::open(self.0.join("dimcode.sqlite")).unwrap();
        connection.busy_timeout(Duration::from_secs(10)).unwrap();
        connection
    }
    /// Rows per table that name `session`.
    pub fn rows(&self, session: &str) -> Vec<(&'static str, i64)> {
        let connection = self.connection();
        SESSION_TABLES
            .into_iter()
            .map(|table| {
                let count = connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE session_id=?1"),
                        [session],
                        |row| row.get(0),
                    )
                    .unwrap();
                (table, count)
            })
            .filter(|(_, count)| *count > 0)
            .collect()
    }
    pub fn output(&self, session: &str) -> PathBuf {
        self.0.join("command-output").join(session)
    }
    pub fn file(&self, file_id: &str) -> PathBuf {
        self.0.join("dimcode.sqlite.session-files").join(file_id)
    }
    /// A Session file `session` owns, as S5a's put leaves it.
    pub fn own_file(&self, session: &str) -> String {
        let file_id = uuid::Uuid::new_v4().to_string();
        std::fs::create_dir_all(self.file(&file_id)).unwrap();
        std::fs::write(self.file(&file_id).join("paste.txt"), b"pasted").unwrap();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO session_files (file_id, node, writer) VALUES (?1, NULL, NULL)",
                [&file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_file_owners (session_id, file_id) VALUES (?1, ?2)",
                [session, &file_id],
            )
            .unwrap();
        file_id
    }
    pub fn file_rows(&self, file_id: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM session_files WHERE file_id=?1",
                [file_id],
                |row| row.get(0),
            )
            .unwrap()
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn session(client: &mut Client) -> String {
    let created = client.rpc("session.create", create(json!(["wait"]))).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}
pub(super) async fn turn(client: &mut Client, session: &str, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main","text":text}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":"main"}),
            )
            .await,
    );
}
pub(super) async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deadline: {what}"));
}
fn listed(reply: &Value, session: &str) -> bool {
    reply["result"]
        .as_array()
        .unwrap()
        .iter()
        .any(|info| info["sessionId"] == session)
}
fn exists(path: &Path) -> bool {
    path.try_exists().unwrap()
}

/// Acceptance 1: a delete closes a loaded Session with a running turn,
/// notifies its attached client, and leaves no row, output or file of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_closes_a_running_session_and_removes_what_it_stored() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let features = client.features.as_array().unwrap();
    assert!(features.contains(&json!("session.delete")));
    let sid = session(&mut client).await;
    let kept = session(&mut client).await;
    turn(&mut client, &kept, "kept").await;
    let kept_rows = store.rows(&kept);
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"tool:wait {\"millis\":60000}"}),
            )
            .await,
    );
    let session = agent.get_session(&sid).unwrap();
    eventually("running turn", || session.state("main").unwrap().running).await;
    std::fs::create_dir_all(store.output(&sid).join("task")).unwrap();
    std::fs::write(store.output(&sid).join("task/exec.log"), b"output").unwrap();
    std::fs::create_dir_all(store.output(&kept)).unwrap();
    let file = store.own_file(&sid);
    assert!(!store.rows(&sid).is_empty());

    let mut other = Client::new_version(&server, false, 3).await;
    assert_ok(&other.rpc("session.delete", json!({"sessionId":sid})).await);
    let closed = client.notice("session.closed").await;
    assert_eq!(closed["params"]["sessionId"], sid.as_str());
    assert!(agent.get_session(&sid).is_none());
    assert!(store.rows(&sid).is_empty(), "{:?}", store.rows(&sid));
    assert!(!exists(&store.output(&sid)));
    eventually("swept file", || {
        store.file_rows(&file) == 0 && !exists(&store.file(&file))
    })
    .await;
    assert_eq!(store.rows(&kept), kept_rows);
    assert!(exists(&store.output(&kept)));

    let list = other.rpc("session.list", json!({})).await;
    assert!(!listed(&list, &sid) && listed(&list, &kept));
    let summaries = other.rpc("session.summaries", json!({})).await;
    assert!(!listed(&summaries, &sid));
    for (method, params) in [
        ("session.attach", json!({"sessionId":sid})),
        (
            "session.submit",
            json!({"sessionId":sid,"branchId":"main","text":"late"}),
        ),
    ] {
        assert_eq!(
            other.rpc(method, params).await["error"]["data"]["appError"],
            "session_closed",
            "{method}"
        );
    }
    for id in [sid.clone(), uuid::Uuid::new_v4().to_string()] {
        assert_eq!(
            other.rpc("session.delete", json!({"sessionId":id})).await["result"],
            json!({})
        );
    }
    assert_eq!(
        other.rpc("session.delete", json!({})).await["error"]["code"],
        -32602
    );
    assert_eq!(
        other
            .rpc("session.delete", json!({"sessionId":kept,"extra":true}))
            .await["error"]["code"],
        -32602
    );
    std::fs::create_dir_all(store.output(&kept).join("inner")).unwrap();
    for id in [format!("{kept}/inner"), format!("{kept}/."), "..".into()] {
        assert_eq!(
            other.rpc("session.delete", json!({"sessionId":id})).await["error"]["data"]["appError"],
            "session_invalid_input",
            "{id}"
        );
    }
    // An empty id is invalid input at the boundary, before any path check.
    assert_eq!(
        other.rpc("session.delete", json!({"sessionId":""})).await["error"]["data"]["appError"],
        "invalid_params"
    );
    assert!(exists(&store.output(&kept).join("inner")));
    let mut v2 = Client::new_version(&server, false, 2).await;
    assert_eq!(
        v2.rpc("session.delete", json!({"sessionId":kept})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    let v2_session = v2.rpc("session.create", create(json!([]))).await;
    let v2_session = v2_session["result"]["sessionId"].as_str().unwrap();
    assert_eq!(
        other
            .rpc("session.delete", json!({"sessionId":v2_session}))
            .await["error"]["data"]["appError"],
        "session_protocol_version"
    );
    assert!(!store.rows(&kept).is_empty() && !store.rows(v2_session).is_empty());
    drop((client, other, v2, session, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// Acceptance 2 and 9: Sessions that are not loaded, fail validation or
/// have no readable owner are deleted by id, and a Session whose output
/// is already gone loads and is deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unloaded_and_damaged_sessions_are_deleted_by_id() {
    let store = Store::new();
    let (server, _) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut ids = vec![];
    for text in ["dormant", "damaged", "ownerless", "outputless"] {
        let id = session(&mut client).await;
        turn(&mut client, &id, text).await;
        ids.push(id);
    }
    let [dormant, damaged, ownerless, outputless] = ids.try_into().unwrap();
    std::fs::create_dir_all(store.output(&dormant)).unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let connection = store.connection();
    connection
        .execute(
            "UPDATE timeline_events SET body='{' WHERE session_id=?1 AND sequence=1",
            [&damaged],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM session_owners WHERE session_id=?1",
            [&ownerless],
        )
        .unwrap();
    drop(connection);

    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for id in [&damaged, &ownerless] {
        assert!(
            client.rpc("session.attach", json!({"sessionId":id})).await["error"].is_object(),
            "{id} does not load"
        );
    }
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":outputless}))
            .await,
    );
    for id in [&dormant, &damaged, &ownerless, &outputless] {
        assert_eq!(
            client.rpc("session.delete", json!({"sessionId":id})).await["result"],
            json!({}),
            "{id}"
        );
        assert!(store.rows(id).is_empty(), "{id}: {:?}", store.rows(id));
        assert!(agent.get_session(id).is_none());
    }
    assert!(!exists(&store.output(&dormant)));
    let list = client.rpc("session.list", json!({})).await;
    assert!(list["result"].as_array().unwrap().is_empty(), "{list}");
    drop((client, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// §4: every database in a directory shares `command-output/`. A delete
/// sent to a daemon whose database has no rows of the id replies `{}` and
/// leaves the directory to the database that has them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_leaves_another_databases_command_output() {
    let store = Store::new();
    let config = |name: &str| ServerConfig {
        database_path: Some(store.0.join(name)),
        ..Default::default()
    };
    let (a, _) = launch_with_agent(Arc::new(MockTools), config("a.sqlite")).await;
    let (b, agent) = launch_with_agent(Arc::new(MockTools), config("b.sqlite")).await;
    let mut to_a = Client::new_version(&a, false, 3).await;
    let mut to_b = Client::new_version(&b, false, 3).await;
    let sid = session(&mut to_b).await;
    turn(&mut to_b, &sid, "in b").await;
    std::fs::create_dir_all(store.output(&sid)).unwrap();
    std::fs::write(store.output(&sid).join("exec.log"), b"output").unwrap();

    assert_eq!(
        to_a.rpc("session.delete", json!({"sessionId":sid})).await["result"],
        json!({})
    );
    assert!(exists(&store.output(&sid)), "b's output stays");
    assert!(agent.get_session(&sid).is_some());
    turn(&mut to_b, &sid, "still in b").await;

    assert_ok(&to_b.rpc("session.delete", json!({"sessionId":sid})).await);
    assert!(!exists(&store.output(&sid)));
    assert!(agent.get_session(&sid).is_none());
    drop((to_a, to_b, agent));
    for server in [a, b] {
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// §3.5 and D5: a delete that fails after it closed a loaded leader replies
/// -32603 and leaves the leader as `session.close` does: its rows stay, and
/// it is neither listed nor loaded again in this process, so it never runs
/// without the relations its close ended. Its member stays a Session.
/// After a restart the leader loads, and a retry deletes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_delete_leaves_the_session_closed_until_a_retry() {
    use std::os::unix::fs::PermissionsExt;
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["echo", "wait", "agent"],
                "permissionPolicy": {"allowedTools": ["echo", "wait", "agent"]}}),
        )
        .await;
    assert_ok(&created);
    let lead = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let agent_id = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request =
        json!({"action":"request","agentId":agent_id,"prompt":"tool:wait {\"millis\":60000}"});
    turn(&mut client, &lead, &format!("tool:agent {request}")).await;
    let member = agent
        .get_session(&lead)
        .unwrap()
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationDelivered(data) => match &data.delivery {
                dim_agent_types::CollaborationDelivery::Ok { session_id, .. } => {
                    Some(session_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("the request was delivered");

    let locked = store.output(&lead).join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("exec.log"), b"output").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        std::fs::write(locked.join("probe"), b"")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied,
        "file permissions must bind the test's user"
    );
    let failed = client
        .rpc("session.delete", json!({"sessionId":lead}))
        .await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(failed["error"]["code"], -32603, "{failed}");

    assert!(!store.rows(&lead).is_empty(), "no row was removed");
    assert!(agent.get_session(&lead).is_none());
    for (method, params) in [
        ("session.attach", json!({"sessionId":lead})),
        ("session.fork", json!({"sessionId":lead,"at":0})),
    ] {
        assert_eq!(
            client.rpc(method, params).await["error"]["data"]["appError"],
            "session_closed",
            "{method}"
        );
    }
    let list = client.rpc("session.list", json!({})).await;
    assert!(!listed(&list, &lead) && listed(&list, &member), "{list}");
    let member_session = agent.get_session(&member).unwrap();
    assert!(!member_session.state("main").unwrap().running);

    drop((client, member_session, agent));
    server.shutdown();
    server.join().await.unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
    assert!(agent.get_session(&lead).is_some());
    assert_eq!(
        client
            .rpc("session.delete", json!({"sessionId":lead}))
            .await["result"],
        json!({})
    );
    assert!(store.rows(&lead).is_empty(), "{:?}", store.rows(&lead));
    assert!(!exists(&store.output(&lead)));
    assert!(!store.rows(&member).is_empty(), "a delete does not cascade");
    drop((client, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// Holds the next Session build in its apply once armed: the Session's rows
/// are committed and listed while its runtime entry is still building.
#[derive(Clone, Default)]
struct HeldBuild {
    armed: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Plugin for HeldBuild {
    type Deps = ();
    fn name(&self) -> &str {
        "HeldBuild"
    }
    async fn apply(&self, _: &rutis::Ctx, (): ()) -> rutis::Result {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// §7: a creation holds the new id's admission gate until the Session is
/// published. A delete of a Session still building, its rows committed and
/// listed, waits for the build and closes what it published, for a
/// `session.create` and a `session.fork`; it never leaves a loaded Session
/// without rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_waits_for_a_building_session() {
    let store = Store::new();
    let held = HeldBuild::default();
    let plugin = held.clone();
    let (server, agent) = launch_with(Arc::new(MockTools), store.config(), move |builder| {
        builder.require_feature(
            Feature::new("HeldBuild").session("HeldBuild", move |_| Ok(Box::new(plugin.clone()))),
        );
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let source = session(&mut client).await;
    turn(&mut client, &source, "source").await;
    let at = agent
        .get_session(&source)
        .unwrap()
        .sequence("main")
        .unwrap();
    for (method, params) in [
        ("session.create", create(json!(["wait"]))),
        ("session.fork", json!({"sessionId":source,"at":at})),
    ] {
        held.armed.store(true, Ordering::SeqCst);
        let mut creator = Client::new_version(&server, false, 3).await;
        let request = creator.send(method, params).await;
        held.entered.notified().await;
        let list = client.rpc("session.list", json!({})).await;
        let building = list["result"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|info| info["sessionId"].as_str())
            .find(|id| *id != source)
            .unwrap_or_else(|| panic!("{method}: the building Session is listed: {list}"))
            .to_owned();
        assert!(agent.get_session(&building).is_none(), "still building");

        let mut deleter = Client::new_version(&server, false, 3).await;
        let delete = deleter
            .send("session.delete", json!({"sessionId":building}))
            .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), deleter.response(delete))
                .await
                .is_err(),
            "{method}: the delete waits for the build"
        );
        held.release.notify_one();
        let created = creator.response(request).await;
        assert_eq!(
            created["result"]["sessionId"],
            building.as_str(),
            "{created}"
        );
        assert_eq!(deleter.response(delete).await["result"], json!({}));
        assert!(agent.get_session(&building).is_none(), "{method}");
        assert!(
            store.rows(&building).is_empty(),
            "{:?}",
            store.rows(&building)
        );
        assert_eq!(
            client
                .rpc("session.attach", json!({"sessionId":building}))
                .await["error"]["data"]["appError"],
            "session_closed",
            "{method}"
        );
    }
    assert!(!store.rows(&source).is_empty());
    drop((client, agent));
    server.shutdown();
    server.join().await.unwrap();
}
