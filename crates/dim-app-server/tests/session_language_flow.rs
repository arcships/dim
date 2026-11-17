//! The reply-language carrier ([PromptSources §3]): `session.language.get` and
//! `.update` over public RPC on an App Server with native storage and
//! PromptSources, and the `dim/language` state it feeds.
//!
//! [PromptSources §3]: ../../../docs/design/plugins/PromptSources.md
#![cfg(unix)]

use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    adapters::MockTools,
    defaults,
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules},
    prompt_sources,
};
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig, Session};
use dim_agent_types::{
    EventKind, InputMessageData, MessageContent, MessagePart, ModelRequest, ToolContent,
};
use dim_app_server::{RunningServer, ServerConfig, serve};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const BLOCK: &str = "The operator set this session's reply language to ";

/// A disposable home: the database directory, a working directory and the
/// socket. Never the product home.
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = PathBuf::from("/tmp").join(format!("dim-lang-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("work")).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    fn work(&self) -> PathBuf {
        self.0.join("work")
    }
    fn database(&self) -> PathBuf {
        self.0.join("dimcode.sqlite")
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn builder(with_sources: bool) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    if with_sources {
        defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
        prompt_sources::install_with_clock(
            &mut builder,
            Arc::new(|| "2026-11-10".into()),
            Duration::from_millis(10),
        );
    } else {
        defaults::install(&mut builder);
    }
    if with_sources {
        builder.session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))));
    }
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder
}

/// Serves from `home`; `prompt_sources` selects an assembly with the
/// `dim/language` source or one without it.
async fn launch(home: &Home, with_sources: bool) -> RunningServer {
    let models = AimuxModels::from_config(
        serde_json::from_value::<ModelConfigFile>(json!({
            "defaultModel": {"providerId": "local", "modelId": "scripted"},
            "providers": [{"id": "local", "kind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "models": [{"id": "scripted", "contextWindow": 128000, "maxOutputTokens": 4096, "config": {"maxTokens": 64}}]}]
        }))
        .unwrap(),
    )
    .unwrap();
    let config = ServerConfig {
        unix_path: Some(home.0.join("server.sock")),
        websocket_address: None,
        target_home: Some(home.0.clone()),
        model_catalog: Some(Arc::new(models)),
        ..Default::default()
    };
    serve(builder(with_sources), config).await.unwrap()
}

struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
    hello: Value,
    /// Replies that arrived before the call that waits for them: two
    /// concurrent calls may answer in either order.
    early: std::collections::BTreeMap<u64, Value>,
}
impl Client {
    async fn connect(server: &RunningServer) -> Self {
        let stream = UnixStream::connect(server.unix_path.as_ref().unwrap())
            .await
            .unwrap();
        let mut client = Self {
            stream: BufReader::new(stream),
            id: 0,
            hello: Value::Null,
            early: std::collections::BTreeMap::new(),
        };
        let hello = client
            .rpc(
                "hello",
                json!({"protocolVersion": 3, "clientKind": "other"}),
            )
            .await;
        assert_eq!(hello["result"]["protocolVersion"], 3, "{hello}");
        client.hello = hello;
        client
    }
    /// The response to one request; notifications in between are skipped.
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.reply(id).await
    }
    /// Writes one request without waiting for its reply.
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let request = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        self.stream
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        self.id
    }
    async fn reply(&mut self, id: u64) -> Value {
        if let Some(value) = self.early.remove(&id) {
            return value;
        }
        let stream = &mut self.stream;
        let early = &mut self.early;
        tokio::time::timeout(Duration::from_secs(30), async move {
            loop {
                let mut line = String::new();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                let value: Value = serde_json::from_str(&line).unwrap();
                let Some(answered) = value["id"].as_u64() else {
                    continue;
                };
                if answered == id {
                    return value;
                }
                early.insert(answered, value);
            }
        })
        .await
        .expect("response deadline")
    }
    async fn create(&mut self, home: &Home, extra: Value) -> String {
        let mut params = json!({"cwd": home.work(), "tools": []});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        let created = self.rpc("session.create", params).await;
        created["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_owned()
    }
    async fn submit(&mut self, session: &str, text: &str) {
        self.submit_on(session, "main", text).await;
    }
    async fn submit_on(&mut self, session: &str, branch: &str, text: &str) {
        let submitted = self
            .rpc(
                "session.submit",
                json!({"sessionId": session, "branchId": branch, "text": text}),
            )
            .await;
        assert!(submitted.get("error").is_none(), "{submitted}");
        self.idle(session, branch).await;
    }
    /// Waits until `branch` has no turn running.
    async fn idle(&mut self, session: &str, branch: &str) {
        let idle = self
            .rpc(
                "session.idle",
                json!({"sessionId": session, "branchId": branch}),
            )
            .await;
        assert!(idle.get("error").is_none(), "{idle}");
    }
    async fn get(&mut self, session: &str) -> Value {
        self.rpc("session.language.get", json!({"sessionId": session}))
            .await
    }
    async fn update(&mut self, session: &str, language: Value) -> Value {
        self.rpc(
            "session.language.update",
            json!({"sessionId": session, "language": language}),
        )
        .await
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

fn language_of(reply: &Value) -> Option<String> {
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"]["language"].as_str().map(str::to_owned)
}

async fn events(session: &Session) -> Vec<Arc<dim_agent_types::AgentEvent>> {
    events_on(session, "main").await
}

async fn events_on(session: &Session, branch: &str) -> Vec<Arc<dim_agent_types::AgentEvent>> {
    chain_events(session, branch, ..).await.unwrap()
}

/// Every reply-language block of `main`, oldest first: its text and whether
/// it retires the state.
async fn language_blocks(session: &Session) -> Vec<(String, bool)> {
    language_blocks_on(session, "main").await
}

async fn language_blocks_on(session: &Session, branch: &str) -> Vec<(String, bool)> {
    events_on(session, branch)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.clone()),
            _ => None,
        })
        .flat_map(|input: InputMessageData| input.blocks)
        .filter_map(|block| match block {
            dim_agent_types::InputBlock::Raw { text, retire, .. }
                if text.starts_with("<dim-state source=\"dim/language\"") =>
            {
                Some((text, retire))
            }
            _ => None,
        })
        .collect()
}

/// The mode each reply-language input was written with.
async fn language_modes(session: &Session) -> Vec<Option<dim_agent_types::InputMode>> {
    events(session)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.clone()),
            _ => None,
        })
        .filter(|input| {
            input.blocks.iter().any(|block| {
                matches!(block, dim_agent_types::InputBlock::Raw { text, .. }
                    if text.starts_with("<dim-state source=\"dim/language\""))
            })
        })
        .map(|input| input.mode)
        .collect()
}

async fn requests_on(session: &Session, branch: &str) -> Vec<Arc<ModelRequest>> {
    events_on(session, branch)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .collect()
}

fn request_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.clone()),
                    MessagePart::ToolResult { content, .. } => match content {
                        ToolContent::Text(text) => Some(text.clone()),
                        ToolContent::Blocks(_) => None,
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn last_request_text(session: &Session) -> String {
    last_request_text_on(session, "main").await
}

async fn last_request_text_on(session: &Session, branch: &str) -> String {
    let sent = requests_on(session, branch).await;
    request_text(sent.last().unwrap_or_else(|| panic!("no model request")))
}

fn turn_starts(events: &[Arc<dim_agent_types::AgentEvent>]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count()
}

/// The stored `language` member of the Session record, read around the daemon.
fn stored_language(home: &Home, session: &str) -> Option<String> {
    let config: String = Connection::open(home.database())
        .unwrap()
        .query_row(
            "SELECT config FROM agent_sessions WHERE session_id=?1",
            [session],
            |row| row.get(0),
        )
        .unwrap();
    let config: Value = serde_json::from_str(&config).unwrap();
    config["language"].as_str().map(str::to_owned)
}

fn set_stored_language(home: &Home, session: &str, language: &str) {
    Connection::open(home.database())
        .unwrap()
        .execute(
            "UPDATE agent_sessions SET config=json_set(config, '$.language', ?2) \
             WHERE session_id=?1",
            rusqlite::params![session, language],
        )
        .unwrap();
}

fn live_session(server: &RunningServer, id: &str) -> Session {
    server.agent().unwrap().get_session(id).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_selection_delivers_the_designed_block_at_creation() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"language": "zh-CN"})).await;

    let read = client.get(&sid).await;
    assert_eq!(language_of(&read), Some("zh-CN".into()), "{read}");
    assert_eq!(stored_language(&home, &sid), Some("zh-CN".into()));

    client.submit(&sid, "hello").await;
    let session = live_session(&server, &sid);
    let blocks = language_blocks(&session).await;
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(
        blocks[0].0,
        format!("<dim-state source=\"dim/language\">{BLOCK}Simplified Chinese.</dim-state>")
    );
    assert!(!blocks[0].1, "a delivered block is not a retire block");
    let text = last_request_text(&session).await;
    assert_eq!(
        text.matches(&format!("{BLOCK}Simplified Chinese.")).count(),
        1,
        "{text}"
    );

    let methods = client
        .rpc("session.methods", json!({"sessionId": sid}))
        .await;
    let listed = methods["result"]["methods"].as_array().unwrap();
    for (name, mutating) in [
        ("session.language.get", false),
        ("session.language.update", true),
    ] {
        assert!(
            listed.contains(&json!({"name": name, "mutating": mutating})),
            "{name} in {methods}"
        );
    }

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_selection_delivers_no_block() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({})).await;

    let read = client.get(&sid).await;
    assert_eq!(language_of(&read), None);
    assert!(read["result"].get("outcome").is_none(), "{read}");

    client.submit(&sid, "hello").await;
    let session = live_session(&server, &sid);
    assert!(language_blocks(&session).await.is_empty());
    assert!(!last_request_text(&session).await.contains("dim/language"));

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_reaches_the_next_request_without_a_wake() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({})).await;
    client.submit(&sid, "first").await;

    let session = live_session(&server, &sid);
    let before = events(&session).await;
    let changed = client.update(&sid, json!("fr")).await;
    assert_eq!(
        changed["result"],
        json!({"language": "fr", "outcome": "adopted"}),
        "{changed}"
    );
    let after = events(&session).await;
    assert_eq!(
        turn_starts(&before),
        turn_starts(&after),
        "the update starts no turn"
    );
    assert_eq!(
        language_modes(&session).await,
        vec![Some(dim_agent_types::InputMode::Append)]
    );

    client.submit(&sid, "second").await;
    let text = last_request_text(&session).await;
    assert_eq!(
        text.matches(&format!("{BLOCK}French.")).count(),
        1,
        "{text}"
    );
    assert!(!text.contains("Simplified Chinese"), "{text}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_unchanged_value_adds_no_block() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"language": "zh-CN"})).await;
    client.submit(&sid, "first").await;

    let session = live_session(&server, &sid);
    let before = language_blocks(&session).await;
    let changed = client.update(&sid, json!("zh-CN")).await;
    assert_eq!(changed["result"]["outcome"], "unchanged", "{changed}");
    assert_eq!(language_blocks(&session).await, before);

    client.submit(&sid, "second").await;
    let text = last_request_text(&session).await;
    assert_eq!(
        text.matches(&format!("{BLOCK}Simplified Chinese.")).count(),
        1,
        "{text}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_retires_the_block() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"language": "fr"})).await;
    client.submit(&sid, "first").await;

    let changed = client.update(&sid, json!({"action": "clear"})).await;
    assert_eq!(
        changed["result"],
        json!({"language": null, "outcome": "adopted"}),
        "{changed}"
    );
    assert_eq!(stored_language(&home, &sid), None);

    let session = live_session(&server, &sid);
    let blocks = language_blocks(&session).await;
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(blocks[1].1, "the second block retires the state");
    assert_eq!(
        blocks[1].0,
        "<dim-state source=\"dim/language\" cleared=\"true\"></dim-state>"
    );

    client.submit(&sid, "second").await;
    let text = last_request_text(&session).await;
    assert_eq!(text.matches("cleared=\"true\"").count(), 1, "{text}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_reads_the_record() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({})).await;
    let changed = client.update(&sid, json!("fr")).await;
    assert_eq!(changed["result"]["outcome"], "adopted", "{changed}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The first restore path: the record the update wrote.
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let read = client.get(&sid).await;
    assert_eq!(language_of(&read), Some("fr".into()), "{read}");
    client.submit(&sid, "after restore").await;
    let session = live_session(&server, &sid);
    assert_eq!(
        last_request_text(&session)
            .await
            .matches(&format!("{BLOCK}French."))
            .count(),
        1
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The second: a write made around the daemon.
    set_stored_language(&home, &sid, "de");
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let read = client.get(&sid).await;
    assert_eq!(language_of(&read), Some("de".into()), "{read}");
    client.submit(&sid, "after the out-of-band write").await;
    let session = live_session(&server, &sid);
    assert_eq!(
        last_request_text(&session)
            .await
            .matches(&format!("{BLOCK}German."))
            .count(),
        1
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_never_selects_keeps_the_base_rule() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    // An older client sends neither the create member nor the methods.
    let sid = client.create(&home, json!({})).await;
    client.submit(&sid, "hello").await;
    let session = live_session(&server, &sid);
    assert!(language_blocks(&session).await.is_empty());
    assert_eq!(language_of(&client.get(&sid).await), None);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // An assembly without PromptSources answers explicitly: no default, no
    // block, and the record keeps whatever the client declared.
    let server = launch(&home, false).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"language": "fr"})).await;
    let read = client.get(&sid).await;
    assert_eq!(failure(&read), (-32000, "language_unavailable"));
    let changed = client.update(&sid, json!("de")).await;
    assert_eq!(failure(&changed), (-32000, "language_unavailable"));
    assert_eq!(stored_language(&home, &sid), Some("fr".into()));

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

async fn through(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId": session, "branchId": branch, "after": 0, "limit": 256}),
        )
        .await;
    assert!(page.get("error").is_none(), "{page}");
    page["result"]["through"].as_u64().unwrap()
}

/// Opens a child of `parent` at its last rest point.
async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = through(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": session, "parentBranchId": parent, "at": at, "prompt": prompt}),
        )
        .await;
    assert!(created.get("error").is_none(), "{created}");
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loaded_side_branch_adopts_an_update_without_a_wake() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"tools": ["branch"]})).await;
    client.submit(&sid, "first").await;
    let side = open_line(&mut client, &sid, "main", "side work").await;
    client.idle(&sid, &side).await;

    let session = live_session(&server, &sid);
    let turns = turn_starts(&events_on(&session, &side).await);
    let sent = requests_on(&session, &side).await.len();
    let changed = client.update(&sid, json!("fr")).await;
    assert_eq!(changed["result"]["outcome"], "adopted", "{changed}");
    // The reply waits for the branch's adoption: the update itself starts no
    // turn, so the single request that follows belongs to the submission and
    // must already carry the value.
    assert_eq!(
        turn_starts(&events_on(&session, &side).await),
        turns,
        "the update starts no turn"
    );

    client.submit_on(&sid, &side, "second").await;
    let requests = requests_on(&session, &side).await;
    assert_eq!(
        requests.len(),
        sent + 1,
        "exactly one request follows the update"
    );
    let text = last_request_text_on(&session, &side).await;
    assert_eq!(
        text.matches(&format!("{BLOCK}French.")).count(),
        1,
        "{text}"
    );
    assert_eq!(
        turn_starts(&events_on(&session, &side).await),
        turns + 1,
        "only the submission's turn runs"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Waits twenty checks (the fixture's 10 ms period) and then asserts the
/// branch stayed asleep and its next request carries exactly one block.
async fn a_branch_keeps_one_block(
    client: &mut Client,
    session: &Session,
    sid: &str,
    side: &str,
    name: &str,
) {
    client.idle(sid, side).await;
    let turns = turn_starts(&events_on(session, side).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        turn_starts(&events_on(session, side).await),
        turns,
        "the check starts no turn"
    );
    client.submit_on(sid, side, "second").await;
    let text = last_request_text_on(session, side).await;
    assert_eq!(
        text.matches(&format!("{BLOCK}{name}.")).count(),
        1,
        "{text}"
    );
}

/// A branch whose history already carries the value holds exactly one
/// block: its check compares the loaded entry with the one its snapshot
/// replayed under the same key and starts no turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_opened_after_the_value_was_applied_holds_one_block() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client
        .create(&home, json!({"tools": ["branch"], "language": "ja"}))
        .await;
    client.submit(&sid, "first").await;
    let session = live_session(&server, &sid);

    // (a) Chosen at creation: the branch replays main's opening block.
    let first = open_line(&mut client, &sid, "main", "side work").await;
    a_branch_keeps_one_block(&mut client, &session, &sid, &first, "Japanese").await;

    // (b) Changed, applied by a main turn, then the branch opens: it
    // replays the update's block from main's history.
    let changed = client.update(&sid, json!("ko")).await;
    assert_eq!(changed["result"]["outcome"], "adopted", "{changed}");
    client.submit(&sid, "after the update").await;
    let second = open_line(&mut client, &sid, "main", "later work").await;
    a_branch_keeps_one_block(&mut client, &session, &sid, &second, "Korean").await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_inherits_the_value_it_was_created_with() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({"language": "zh-CN"})).await;
    let forked = client
        .rpc("session.fork", json!({"sessionId": sid, "at": 0}))
        .await;
    let fork = forked["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{forked}"))
        .to_owned();
    assert_eq!(language_of(&client.get(&fork).await), Some("zh-CN".into()));
    assert_eq!(stored_language(&home, &fork), Some("zh-CN".into()));

    client.submit(&fork, "hello from the fork").await;
    let fork_session = live_session(&server, &fork);
    assert_eq!(
        last_request_text(&fork_session)
            .await
            .matches(&format!("{BLOCK}Simplified Chinese."))
            .count(),
        1
    );

    // A change on main does not rewrite the fork it was cut from.
    let changed = client.update(&sid, json!("de")).await;
    assert_eq!(changed["result"]["outcome"], "adopted", "{changed}");
    assert_eq!(language_of(&client.get(&sid).await), Some("de".into()));
    assert_eq!(language_of(&client.get(&fork).await), Some("zh-CN".into()));

    // A fork cut after the update carries the updated value.
    let later = client
        .rpc("session.fork", json!({"sessionId": sid, "at": 0}))
        .await;
    let later = later["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{later}"))
        .to_owned();
    assert_eq!(language_of(&client.get(&later).await), Some("de".into()));
    assert_eq!(stored_language(&home, &later), Some("de".into()));
    client.submit(&later, "hello after the update").await;
    let later_session = live_session(&server, &later);
    assert_eq!(
        last_request_text(&later_session)
            .await
            .matches(&format!("{BLOCK}German."))
            .count(),
        1
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_methods_are_listed_and_belong_to_the_session() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let features = client.hello["result"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(features.contains(&"session.language"), "{features:?}");

    let sid = client.create(&home, json!({})).await;
    assert_eq!(language_of(&client.get(&sid).await), None);
    let unknown = client
        .rpc(
            "session.language.get",
            json!({"sessionId": "no-such-session"}),
        )
        .await;
    assert_eq!(failure(&unknown), (-32000, "session_closed"));

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_requests_change_nothing() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({})).await;
    for language in [
        json!("xx"),
        json!("zh-cn"),
        json!(5),
        json!({"action": "reset"}),
        json!({"action": "clear", "extra": true}),
    ] {
        let reply = client.update(&sid, language.clone()).await;
        assert_eq!(failure(&reply), (-32602, "invalid_params"), "{language}");
    }
    let extra = client
        .rpc(
            "session.language.update",
            json!({"sessionId": sid, "language": "fr", "extra": true}),
        )
        .await;
    assert_eq!(failure(&extra), (-32602, "invalid_params"));
    let missing = client
        .rpc("session.language.update", json!({"sessionId": sid}))
        .await;
    assert_eq!(failure(&missing), (-32602, "invalid_params"));

    assert_eq!(language_of(&client.get(&sid).await), None);
    assert_eq!(stored_language(&home, &sid), None);
    assert!(
        language_blocks(&live_session(&server, &sid))
            .await
            .is_empty()
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_updates_agree_on_one_value() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let sid = client.create(&home, json!({})).await;
    client.submit(&sid, "first").await;

    let first = client
        .send(
            "session.language.update",
            json!({"sessionId": sid, "language": "fr"}),
        )
        .await;
    let second = client
        .send(
            "session.language.update",
            json!({"sessionId": sid, "language": "de"}),
        )
        .await;
    let first = client.reply(first).await;
    let second = client.reply(second).await;
    assert!(first.get("error").is_none(), "{first}");
    assert!(second.get("error").is_none(), "{second}");

    let read = language_of(&client.get(&sid).await);
    let stored = stored_language(&home, &sid);
    assert_eq!(read, stored, "the cell and the record agree");
    let name = match read.as_deref() {
        Some("fr") => "French",
        Some("de") => "German",
        other => panic!("unexpected value {other:?}"),
    };

    let live = live_session(&server, &sid);
    let blocks = language_blocks(&live).await;
    let (last, retire) = blocks.last().expect("a language block");
    assert!(!retire, "{blocks:?}");
    assert!(last.contains(&format!("{BLOCK}{name}.")), "{blocks:?}");

    client.submit(&sid, "second").await;
    let text = last_request_text(&live).await;
    assert!(text.contains(&format!("{BLOCK}{name}.")), "{text}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
