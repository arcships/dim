//! SessionStore title metadata through its public flows (S5b,
//! design/appserver/session-title-metadata.md §16): the `sessionMetadata`
//! service as a Session part injects it, the way SessionTitle's row does,
//! title notices on attached v3 clients and the v3 list fields, on
//! disposable native stores.
use super::deletion::{Store, session, turn};
use super::*;
use dim_app_server::{
    session_typescript::SessionTypeScriptConfig, storage::testing::hold_title_write,
};
use futures_util::FutureExt;
use rutis::host::CtxHost;
use rutis_bridge::session::{HostDispatch, Reply};
use std::sync::OnceLock;

/// `sessionMetadata` as a Session part injects it by name. `serve` takes no
/// extra process rows, so the part is the test's consumer.
#[derive(Clone, Default)]
struct Metadata(Arc<OnceLock<Arc<dyn HostDispatch>>>);
struct MetadataConsumer {
    metadata: Metadata,
    service: rutis::host::Key<dyn HostDispatch>,
}
impl MetadataConsumer {
    fn new(names: &dim_agent_runtime::Names<'_>, metadata: Metadata) -> rutis::Result<Self> {
        Ok(Self {
            metadata,
            service: names.named("sessionMetadata")?,
        })
    }
}
impl rutis::host::DynPlugin for MetadataConsumer {
    fn name(&self) -> &str {
        "title-metadata-consumer"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.service.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let service = ctx.require_at(&self.service)?;
            let _ = self.metadata.0.set(service);
            Ok(rutis::host::Effect::Done)
        })
    }
}
impl Metadata {
    /// Starts a call with one request object: the reply future, or the
    /// bridge error of a malformed call.
    fn start(&self, method: &str, request: Value) -> Reply {
        self.0
            .get()
            .expect("a Session injected sessionMetadata")
            .invoke(method, json!([request]).into())
    }
    /// The reply envelope.
    async fn call(&self, method: &str, request: Value) -> Value {
        envelope(self.start(method, request).unwrap()).await
    }
    /// Starts a call and awaits its envelope in a task of its own, as a
    /// client awaits a reply.
    fn spawn(&self, method: &str, request: Value) -> tokio::task::JoinHandle<Value> {
        tokio::spawn(envelope(self.start(method, request).unwrap()))
    }
    async fn read(&self, session: &str) -> Value {
        self.call("read", json!({"sessionId": session})).await
    }
    async fn set(&self, session: &str, expected: u64, title: Value) -> Value {
        self.call("setTitle", set_request(session, expected, title))
            .await
    }
}
/// Awaits a started call's reply future: the envelope.
async fn envelope(reply: rutis_bridge::session::Value) -> Value {
    match reply {
        rutis_bridge::session::Value::Reference(reply) if reply.is_future() => {
            reply.wait_async().await.unwrap().json().unwrap()
        }
        reply => panic!("sessionMetadata replies asynchronously: {:?}", reply.json()),
    }
}
fn set_request(session: &str, expected: u64, title: Value) -> Value {
    json!({"sessionId": session, "expectedRevision": expected, "title": title})
}
fn ok(value: Value) -> Value {
    json!({"ok": true, "value": value})
}
/// The error code of a failed envelope.
fn code(reply: &Value) -> &str {
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"]["message"].is_string(), "{reply}");
    reply["error"]["code"].as_str().unwrap()
}
fn title(text: &str, source: &str) -> Value {
    json!({"text": text, "source": source})
}
/// The title fields of a v3 list entry and of a title notice's data.
fn fields(title: Option<(&str, &str)>, revision: u64) -> Value {
    json!({
        "title": title.map(|(text, _)| text),
        "titleSource": title.map(|(_, source)| source),
        "titleRevision": revision,
    })
}

async fn serve_titles(store: &Store) -> (RunningServer, dim_agent_runtime::Agent, Metadata) {
    let metadata = Metadata::default();
    let consumer = metadata.clone();
    let (server, agent) = launch_with(Arc::new(MockTools), store.config(), move |builder| {
        builder.require_feature(
            Feature::new("title-metadata-consumer")
                .session("title-metadata-consumer", move |names| {
                    Ok(Box::new(MetadataConsumer::new(names, consumer.clone())?))
                }),
        );
    })
    .await;
    (server, agent, metadata)
}
async fn attach(client: &mut Client, session: &str) {
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": session}))
            .await,
    );
}
/// The data of every title notice the client received for `session`, in
/// arrival order. The idle reply waits for this subscription's delivery
/// fence, which follows every notice emitted before the call.
async fn title_notices(client: &mut Client, session: &str) -> Vec<Value> {
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": session, "branchId": "main"}),
            )
            .await,
    );
    std::mem::take(&mut client.notices)
        .into_iter()
        .filter(|value| value["method"] == "session.notice")
        .map(|value| {
            assert_eq!(value["params"]["sessionId"], session, "{value}");
            assert_eq!(value["params"]["source"], "SessionStore", "{value}");
            assert_eq!(value["params"]["kind"], "title", "{value}");
            value["params"]["data"].clone()
        })
        .collect()
}
/// The title fields of `session`'s entry in a list reply.
fn listed(reply: &Value, session: &str) -> Value {
    let entry = reply["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == session)
        .unwrap_or_else(|| panic!("{session} is listed: {reply}"));
    json!({
        "title": entry["title"],
        "titleSource": entry["titleSource"],
        "titleRevision": entry["titleRevision"],
    })
}
async fn listed_everywhere(client: &mut Client, session: &str) -> Value {
    let list = client.rpc("session.list", json!({})).await;
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_eq!(listed(&list, session), listed(&summaries, session));
    listed(&list, session)
}

/// §16 items 1, 3, 4, 7 and 11: revision-checked writes, one notice per
/// commit in commit order on both transports, none for a conflict, an
/// unknown Session or a malformed call, and the v3 list fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_title_commits_revisions_and_announces_each_commit_once() {
    for _ in 0..3 {
        let store = Store::new();
        let (server, agent, metadata) = serve_titles(&store).await;
        let mut ipc = Client::new_version(&server, false, 3).await;
        let mut ws = Client::new_version(&server, true, 3).await;
        let sid = session(&mut ipc).await;
        attach(&mut ipc, &sid).await;
        attach(&mut ws, &sid).await;
        assert_eq!(listed_everywhere(&mut ipc, &sid).await, fields(None, 0));
        assert_eq!(
            metadata.read(&sid).await,
            ok(json!({"revision": 0, "title": null}))
        );

        let auto = title("Parser fix", "auto");
        assert_eq!(
            metadata.set(&sid, 0, auto.clone()).await,
            ok(json!({"revision": 1}))
        );
        assert_eq!(
            code(&metadata.set(&sid, 0, title("Stale", "manual")).await),
            "conflict"
        );
        let renamed = title("Renamed", "manual");
        assert_eq!(
            metadata.set(&sid, 1, renamed.clone()).await,
            ok(json!({"revision": 2}))
        );
        assert_eq!(
            metadata.read(&sid).await,
            ok(json!({"revision": 2, "title": renamed}))
        );
        // The same text again is a new revision; a clear keeps counting.
        assert_eq!(
            metadata.set(&sid, 2, renamed.clone()).await,
            ok(json!({"revision": 3}))
        );
        assert_eq!(
            metadata.set(&sid, 3, Value::Null).await,
            ok(json!({"revision": 4}))
        );
        assert_eq!(code(&metadata.set(&sid, 2, auto.clone()).await), "conflict");

        // Never created: `not_found`, and nothing is written.
        assert_eq!(code(&metadata.read("missing").await), "not_found");
        assert_eq!(
            code(&metadata.set("missing", 0, auto.clone()).await),
            "not_found"
        );
        // Malformed calls are bridge errors; an omitted title is not a clear.
        for (method, request) in [
            ("setTitle", json!({"sessionId": sid, "expectedRevision": 4})),
            (
                "setTitle",
                json!({"sessionId": sid, "expectedRevision": 4, "title": {"text": "x"}}),
            ),
            ("read", json!({"sessionId": sid, "branchId": "main"})),
            ("rename", json!({"sessionId": sid})),
        ] {
            assert!(
                metadata.start(method, request.clone()).is_err(),
                "{request}"
            );
        }
        assert!(
            metadata
                .0
                .get()
                .unwrap()
                .invoke("read", json!([]).into())
                .is_err()
        );
        assert_eq!(
            metadata.read(&sid).await,
            ok(json!({"revision": 4, "title": null}))
        );

        let expected = vec![
            fields(Some(("Parser fix", "auto")), 1),
            fields(Some(("Renamed", "manual")), 2),
            fields(Some(("Renamed", "manual")), 3),
            fields(None, 4),
        ];
        for client in [&mut ipc, &mut ws] {
            assert_eq!(title_notices(client, &sid).await, expected);
        }
        assert_eq!(listed_everywhere(&mut ws, &sid).await, fields(None, 4));
        assert!(agent.get_session(&sid).is_some());
        drop((ipc, ws, agent));
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// §16 item 2: concurrent writers. Every commit is announced once, in
/// increasing revision order, and conflicts announce nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_title_writers_announce_every_commit_in_revision_order() {
    const WRITERS: u64 = 4;
    const EACH: u64 = 5;
    for _ in 0..5 {
        let store = Store::new();
        let (server, _, metadata) = serve_titles(&store).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let sid = session(&mut client).await;
        attach(&mut client, &sid).await;
        let writers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let (metadata, sid) = (metadata.clone(), sid.clone());
                tokio::spawn(async move {
                    let mut committed = 0;
                    while committed < EACH {
                        let read = metadata.read(&sid).await;
                        let revision = read["value"]["revision"].as_u64().unwrap();
                        let text = format!("writer {writer}, title {committed}");
                        let reply = metadata.set(&sid, revision, title(&text, "manual")).await;
                        if reply["ok"] == true {
                            assert!(reply["value"]["revision"].as_u64().unwrap() > revision);
                            committed += 1;
                        } else {
                            assert_eq!(code(&reply), "conflict");
                        }
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.await.unwrap();
        }
        let total = WRITERS * EACH;
        let current = metadata.read(&sid).await;
        assert_eq!(current["value"]["revision"], total);
        let notices = title_notices(&mut client, &sid).await;
        let revisions: Vec<u64> = notices
            .iter()
            .map(|data| data["titleRevision"].as_u64().unwrap())
            .collect();
        assert_eq!(revisions, (1..=total).collect::<Vec<_>>());
        let last = &current["value"]["title"]["text"];
        assert_eq!(notices.last().unwrap()["title"], *last);
        assert_eq!(
            listed_everywhere(&mut client, &sid).await,
            notices[notices.len() - 1]
        );
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// §16 items 5 and 6: a failed transaction reports `storage`, writes and
/// announces nothing; a call whose reply is dropped while the write is in
/// progress still commits and announces once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_writes_announce_nothing_and_dropped_calls_still_settle() {
    for _ in 0..3 {
        let store = Store::new();
        let (server, _, metadata) = serve_titles(&store).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let sid = session(&mut client).await;
        attach(&mut client, &sid).await;
        let before = title("Before", "auto");
        assert_eq!(
            metadata.set(&sid, 0, before.clone()).await,
            ok(json!({"revision": 1}))
        );

        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_title_insert BEFORE INSERT ON session_metadata \
                 BEGIN SELECT RAISE(ABORT, 'injected title failure'); END; \
                 CREATE TRIGGER fail_title_update BEFORE UPDATE ON session_metadata \
                 BEGIN SELECT RAISE(ABORT, 'injected title failure'); END",
            )
            .unwrap();
        let failed = metadata.set(&sid, 1, title("Lost", "manual")).await;
        assert_eq!(code(&failed), "storage");
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("injected title failure"),
            "{failed}"
        );
        assert_eq!(
            metadata.read(&sid).await,
            ok(json!({"revision": 1, "title": before}))
        );
        // Startup requires the exact schema, so the triggers go before any
        // restart of this store.
        store
            .connection()
            .execute_batch("DROP TRIGGER fail_title_insert; DROP TRIGGER fail_title_update")
            .unwrap();

        // The client awaits the reply future, then drops it while the
        // write is held before its transaction.
        let mut held = hold_title_write(&sid);
        let reply = metadata
            .start("setTitle", set_request(&sid, 1, title("Dropped", "manual")))
            .unwrap();
        let (polled, awaiting) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let mut reply = std::pin::pin!(envelope(reply));
            assert!(reply.as_mut().now_or_never().is_none());
            polled.send(()).unwrap();
            reply.await
        });
        held.arrived().await;
        awaiting.await.unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        drop(held);
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let read = metadata.read(&sid).await;
                if read["value"]["revision"] == 2 {
                    break read;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the dropped write settles");
        assert_eq!(
            settled,
            ok(json!({"revision": 2, "title": title("Dropped", "manual")}))
        );
        assert_eq!(
            title_notices(&mut client, &sid).await,
            vec![
                fields(Some(("Before", "auto")), 1),
                fields(Some(("Dropped", "manual")), 2),
            ]
        );
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// §16 item 12: unload. Writes admitted before SessionStore starts closing,
/// one in its turn and one waiting for it, commit and reply before SQLite
/// closes; a call after closing starts fails `storage` at once and writes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_settles_admitted_title_writes_and_refuses_later_ones() {
    for _ in 0..3 {
        let store = Store::new();
        let (server, agent, metadata) = serve_titles(&store).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let sid = session(&mut client).await;
        drop((client, agent));

        let mut held = hold_title_write(&sid);
        let running = metadata.spawn("setTitle", set_request(&sid, 0, title("Running", "auto")));
        held.arrived().await;
        let waiting = metadata.spawn("setTitle", set_request(&sid, 1, title("Waiting", "manual")));
        server.shutdown();
        let stopped = tokio::spawn(server.join());

        // Probes name no stored Session, so they write nothing either way.
        // Admitted, they wait behind the held write and then fail
        // `not_found`; once SessionStore is closing they fail at once, so the
        // first probe to finish shows that closing has started.
        let missing = || set_request("missing", 0, title("Probe", "auto"));
        let mut probes = vec![];
        tokio::time::timeout(Duration::from_secs(10), async {
            while !probes.iter().any(tokio::task::JoinHandle::is_finished) {
                probes.push(metadata.spawn("setTitle", missing()));
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("SessionStore refuses calls once it is closing");
        let late = tokio::time::timeout(
            Duration::from_secs(10),
            metadata.set(&sid, 2, title("Late", "manual")),
        )
        .await
        .expect("a call after closing starts fails at once");
        assert_eq!(code(&late), "storage");
        assert!(!running.is_finished() && !waiting.is_finished());
        assert!(!stopped.is_finished(), "shutdown waits for admitted writes");

        drop(held);
        assert_eq!(running.await.unwrap(), ok(json!({"revision": 1})));
        assert_eq!(waiting.await.unwrap(), ok(json!({"revision": 2})));
        // The probes started before closing settle; every later one was
        // refused.
        let mut codes = vec![];
        for probe in probes {
            codes.push(code(&probe.await.unwrap()).to_owned());
        }
        let admitted = codes.iter().take_while(|code| *code == "not_found").count();
        assert!(admitted < codes.len(), "{codes:?}");
        assert!(
            codes[admitted..].iter().all(|code| code == "storage"),
            "{codes:?}"
        );
        stopped.await.unwrap().unwrap();

        let (server, agent, metadata) = serve_titles(&store).await;
        let mut client = Client::new_version(&server, false, 3).await;
        // Another Session gives the test its consumer part.
        session(&mut client).await;
        assert_eq!(
            metadata.read(&sid).await,
            ok(json!({"revision": 2, "title": title("Waiting", "manual")}))
        );
        drop((client, agent));
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// §16 items 7 and 8: v2 lists stay frozen; a fork starts untitled; a title
/// survives a restart and is read and written while its Session is not
/// loaded; a delete removes it with the Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn titles_survive_restart_skip_forks_and_go_with_their_session() {
    let store = Store::new();
    let (server, agent, metadata) = serve_titles(&store).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut legacy = Client::new_version(&server, false, 2).await;
    let sid = session(&mut client).await;
    turn(&mut client, &sid, "first").await;
    let legacy_sid = session(&mut legacy).await;
    let kept = title("Kept", "manual");
    assert_eq!(
        metadata.set(&sid, 0, kept.clone()).await,
        ok(json!({"revision": 1}))
    );
    assert_eq!(
        metadata.set(&legacy_sid, 0, title("Legacy", "auto")).await,
        ok(json!({"revision": 1}))
    );
    let old = legacy.rpc("session.list", json!({})).await;
    let entry = old["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == legacy_sid)
        .unwrap();
    for field in ["title", "titleSource", "titleRevision"] {
        assert!(entry.get(field).is_none(), "{entry}");
    }

    let at = agent.get_session(&sid).unwrap().sequence("main").unwrap();
    let fork = client
        .rpc("session.fork", json!({"sessionId": sid, "at": at}))
        .await;
    assert_ok(&fork);
    let fid = fork["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(
        metadata.read(&fid).await,
        ok(json!({"revision": 0, "title": null}))
    );
    assert_eq!(listed_everywhere(&mut client, &fid).await, fields(None, 0));
    assert_eq!(
        listed_everywhere(&mut client, &sid).await,
        fields(Some(("Kept", "manual")), 1)
    );
    drop((client, legacy, agent));
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent, metadata) = serve_titles(&store).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        listed_everywhere(&mut client, &sid).await,
        fields(Some(("Kept", "manual")), 1)
    );
    // Another Session gives the test its consumer part; `sid` stays unloaded.
    session(&mut client).await;
    assert_eq!(
        metadata.read(&sid).await,
        ok(json!({"revision": 1, "title": kept}))
    );
    assert_eq!(
        metadata.set(&sid, 1, title("Unloaded", "auto")).await,
        ok(json!({"revision": 2}))
    );
    assert!(agent.get_session(&sid).is_none());
    assert_eq!(
        listed_everywhere(&mut client, &sid).await,
        fields(Some(("Unloaded", "auto")), 2)
    );

    assert_eq!(
        client
            .rpc("session.delete", json!({"sessionId": sid}))
            .await["result"],
        json!({})
    );
    assert!(store.rows(&sid).is_empty(), "{:?}", store.rows(&sid));
    assert_eq!(code(&metadata.read(&sid).await), "not_found");
    assert_eq!(
        code(&metadata.set(&sid, 2, title("Gone", "manual")).await),
        "not_found"
    );
    assert_eq!(listed_everywhere(&mut client, &fid).await, fields(None, 0));
    drop((client, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// §16 item 10: a TypeScript Session row running in Node injects
/// `sessionMetadata` by name, reads and sets its Session's title, and the
/// attached client receives the notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_typescript_row_reads_and_sets_the_title_through_session_metadata() {
    let store = Store::new();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let rows = SessionTypeScriptConfig {
        python: None,
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![json!({
            "id": "titles",
            "name": root.join("runtime/fixtures/session-metadata.ts"),
            "provides": {},
            "config": {},
        })],
    };
    let row = rows.rows[0].clone();
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            session_typescript: Some(rows),
            ..store.config()
        },
        |_| {},
    )
    .await;
    super::session_typescript::plugin_active(&server, &row).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = session(&mut client).await;
    attach(&mut client, &sid).await;
    let call = async |client: &mut Client, method: &str, params: Value| {
        let mut params = params;
        params["sessionId"] = json!(sid);
        let reply = client
            .rpc(&format!("session.titles.{method}"), params)
            .await;
        assert_ok(&reply);
        reply["result"].clone()
    };
    assert_eq!(
        call(&mut client, "read", json!({})).await,
        ok(json!({"revision": 0, "title": null}))
    );
    let typed = title("From TypeScript", "manual");
    assert_eq!(
        call(
            &mut client,
            "set",
            json!({"expectedRevision": 0, "title": typed})
        )
        .await,
        ok(json!({"revision": 1}))
    );
    assert_eq!(
        code(
            &call(
                &mut client,
                "set",
                json!({"expectedRevision": 0, "title": null})
            )
            .await
        ),
        "conflict"
    );
    assert_eq!(
        call(&mut client, "read", json!({})).await,
        ok(json!({"revision": 1, "title": typed}))
    );
    assert_eq!(
        title_notices(&mut client, &sid).await,
        vec![fields(Some(("From TypeScript", "manual")), 1)]
    );
    assert_eq!(
        listed_everywhere(&mut client, &sid).await,
        fields(Some(("From TypeScript", "manual")), 1)
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
