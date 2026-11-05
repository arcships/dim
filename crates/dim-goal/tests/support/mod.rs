// Every test binary uses the helpers it needs; the rest are shared.
#![allow(dead_code)]
pub mod parts;
pub mod texts;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::HistoryRenderer,
    host::{
        CompactionStrategyHost, ContextModelHost, IdentityContext, ModelDriver, ModelHost,
        PrepareContextHost,
    },
};
use dim_agent_runtime::{
    Agent, AgentBuilder, AgentConfig, BoxFuture, Feature, Names, Session, Timeline,
    TimelineIdentity, testing::chain_events,
};
use dim_agent_types::{BranchCreateData, EventKind, ModelRequest, SystemPrompt, SystemPromptPart};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// The branch Timelines the Goal parts run against, by branch id: a flow
/// writes the facts it would otherwise have to wait for a model to produce.
#[derive(Default)]
pub struct Probe {
    timelines: Mutex<HashMap<String, Arc<Timeline>>>,
}

impl Probe {
    pub fn timeline(&self, branch: &str) -> Arc<Timeline> {
        self.timelines
            .lock()
            .unwrap()
            .get(branch)
            .cloned()
            .unwrap_or_else(|| panic!("branch {branch} has no Timeline"))
    }
    pub fn has(&self, branch: &str) -> bool {
        self.timelines.lock().unwrap().contains_key(branch)
    }
    /// Commits one fact on `branch`, as the runtime would.
    pub fn emit(&self, branch: &str, kind: EventKind) -> Arc<dim_agent_types::AgentEvent> {
        self.timeline(branch)
            .append(kind.into())
            .unwrap_or_else(|error| panic!("append on {branch}: {error}"))
    }
}

struct ProbePart {
    probe: Arc<Probe>,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
}

impl ProbePart {
    fn new(names: &Names<'_>, probe: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            timeline: names.key()?,
            identity: names.key()?,
        })
    }
}

impl DynPlugin for ProbePart {
    fn name(&self) -> &str {
        "GoalProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            self.probe
                .timelines
                .lock()
                .unwrap()
                .insert(identity.branch_id.clone(), timeline);
            Ok(Effect::Done)
        })
    }
}

/// One product home with Goal (unless a flow assembles without it) and the
/// capabilities it depends on, one Session on it, and the probe over its
/// branches.
pub struct Fixture {
    pub server: RunningServer,
    pub agent: Agent,
    pub client: Client,
    /// The Session `start` created; `None` after a restart, which serves the
    /// same home without creating another one.
    pub session: Option<Session>,
    pub id: String,
    pub probe: Arc<Probe>,
    home: PathBuf,
    goal: bool,
}

impl Fixture {
    pub async fn start() -> Self {
        Self::start_with(Arc::new(EchoModel)).await
    }

    /// [`Self::start`] with a model of the flow's own.
    pub async fn start_with(model: Arc<dyn ModelDriver>) -> Self {
        Self::serve_in(home(), model, true, true).await
    }

    /// The product profile without Goal: its methods do not exist.
    pub async fn start_without_goal() -> Self {
        Self::serve_in(home(), Arc::new(EchoModel), false, true).await
    }

    async fn serve_in(home: PathBuf, model: Arc<dyn ModelDriver>, goal: bool, fresh: bool) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
        let probe = Arc::new(Probe::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        if goal {
            dim_goal::install(&mut builder);
        }
        defaults::install(&mut builder);
        if goal {
            builder
                .require_part(dim_goal::SESSION_PART)
                .require_part(dim_goal::BRANCH_PART);
        }
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model)))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::tool_host(&mut builder, Arc::new(MockTools));
        let part = probe.clone();
        builder.require_feature(Feature::new("GoalProbe").branch("GoalProbe", move |names| {
            Ok(Box::new(ProbePart::new(names, part.clone())?))
        }));
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(home.join("server.sock")),
                product_home: Some(home.clone()),
                target_home: Some(home.join("rust")),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|error| panic!("serve: {error}"));
        let mut client = Client::connect(&ConnectionConfig {
            endpoint: server.unix_path.as_ref().unwrap().clone(),
            server: Server {
                program: "dimcode".into(),
                embedded: true,
            },
            profile: None,
            client_kind: "other",
            client_name: "Goal flow",
        })
        .await
        .unwrap();
        let mut id = String::new();
        let mut session = None;
        if fresh {
            let models = [
                json!({"modelId":"fixture","displayName":"fixture","status":"active",
                "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}),
            ];
            client
                .rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000005","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":models,"enabled":true}))
                .await
                .unwrap();
            client
                .rpc(
                    "provider.select",
                    json!({"providerId":"018ec73f-0000-7000-8000-000000000005","modelId":"fixture"}),
                )
                .await
                .unwrap();
            let tools: &[&str] = if goal { &["goal_update", "wait"] } else { &[] };
            let created = client
                .rpc(
                    "session.create",
                    json!({
                        "cwd": home,
                        "tools": tools,
                        "permissionPolicy": {"allowedTools": tools},
                    }),
                )
                .await
                .unwrap();
            id = created["sessionId"].as_str().unwrap().to_owned();
            if goal {
                wait_goal_ready(&mut client, &id).await;
                tokio::time::timeout(Duration::from_secs(15), async {
                    while !probe.has("main") {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("main is mounted");
            }
            let agent = server.agent().unwrap();
            session = agent.get_session(&id);
        }
        let agent = server.agent().unwrap();
        Self {
            server,
            agent,
            client,
            session,
            id,
            probe,
            home,
            goal,
        }
    }

    /// Serves the same home again, with `model` as its model host: the
    /// Sessions' facts and the settings document survive.
    pub async fn restart(self, model: Arc<dyn ModelDriver>) -> Self {
        let Fixture {
            server,
            client,
            home,
            id,
            goal,
            ..
        } = self;
        drop(client);
        server.shutdown();
        let _ = server.join().await;
        let mut next = Self::serve_in(home, model, goal, false).await;
        next.id = id;
        next
    }

    /// The Session `start` created, which a restart leaves behind.
    pub fn default_session(&self) -> &Session {
        self.session
            .as_ref()
            .expect("the fixture's own Session; a restarted fixture has none")
    }

    /// Creates one more Session, as a flow does.
    pub async fn new_session(&mut self, tools: &[&str]) -> String {
        let created = self
            .client
            .rpc(
                "session.create",
                json!({
                    "cwd": self.home,
                    "tools": tools,
                    "permissionPolicy": {"allowedTools": tools},
                }),
            )
            .await
            .unwrap();
        let id = created["sessionId"].as_str().unwrap().to_owned();
        if self.goal {
            wait_goal_ready(&mut self.client, &id).await;
        }
        id
    }

    /// The goal limit the Settings document holds.
    pub async fn set_limit(&mut self, limit: u64) {
        let read = self
            .client
            .rpc("settings.read", json!({"source": "goal", "scope": "user"}))
            .await
            .unwrap();
        self.client
            .rpc(
                "settings.update",
                json!({"source":"goal","scope":"user",
                    "expectedRevision":read["revision"],
                    "document":{"maxContinuations":limit}}),
            )
            .await
            .unwrap();
    }

    /// One Goal change method, as its callers write one.
    pub async fn goal(&mut self, method: &str, params: Value) -> Result<u64, dim_client::Error> {
        let id = self.id.clone();
        self.goal_on(&id, method, params).await
    }

    /// [`Self::goal`] for a Session of the flow's own.
    pub async fn goal_on(
        &mut self,
        session: &str,
        method: &str,
        params: Value,
    ) -> Result<u64, dim_client::Error> {
        let mut params = params;
        params["sessionId"] = json!(session);
        let reply = self
            .client
            .rpc(&format!("session.goal.{method}"), params)
            .await?;
        Ok(reply["sequence"].as_u64().expect("a sequence"))
    }

    /// One operator input on a Session of the flow's own.
    pub async fn submit_on(&mut self, session: &str, branch: &str, text: &str) {
        self.client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":branch,"text":text}),
            )
            .await
            .unwrap();
    }

    /// Submits the worker's `goal_update` on the Session's main branch.
    pub async fn submit_report_on(&mut self, session: &str, status: &str, summary: &str) {
        self.submit_on(
            session,
            "main",
            &format!(
                "tool:goal_update {}",
                json!({"status":status,"summary":summary})
            ),
        )
        .await;
    }

    /// Waits until the Session's `branch` execution is idle.
    pub async fn idle_on(&mut self, session: &str, branch: &str) {
        self.client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":branch}),
            )
            .await
            .unwrap();
    }

    /// Forks `session` at `at`, returning the new Session's id.
    pub async fn fork_on(&mut self, session: &str, at: u64) -> String {
        let fork = self
            .client
            .rpc("session.fork", json!({"sessionId":session,"at":at}))
            .await
            .unwrap();
        fork["sessionId"].as_str().unwrap().to_owned()
    }

    /// The Goal blocks on the fixture's own `branch`, in order.
    pub async fn writes(&self, branch: &str) -> Vec<Block> {
        goal_writes(self.default_session(), branch).await
    }

    /// Waits until a Goal write on the fixture's own `branch` names `needle`.
    pub async fn wait_for(&self, branch: &str, needle: &str) -> Block {
        wait_goal(self.default_session(), branch, needle).await
    }

    /// The branch's committed facts after `after` on the fixture's own Session.
    pub async fn events(&self, branch: &str, after: u64) -> Vec<Arc<dim_agent_types::AgentEvent>> {
        branch_events(self.default_session(), branch, after + 1..)
            .await
            .unwrap()
    }

    /// The Goal writes on `branch` once the branch is quiescent.
    ///
    /// A write method replies after its commit, but a read stops at the
    /// published head, which follows that commit; a flow that has just
    /// written can still read zero facts. Wait for the first write, then for
    /// the count to stop changing.
    pub async fn settled_writes(&self, branch: &str) -> Vec<Block> {
        let mut last = Vec::new();
        let mut stable = 0;
        for _ in 0..600 {
            let writes = self.writes(branch).await;
            if !writes.is_empty() && writes.len() == last.len() {
                stable += 1;
                if stable >= 5 {
                    return writes;
                }
            } else {
                stable = 0;
            }
            last = writes;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        last
    }

    pub async fn close(self) {
        drop(self.client);
        drop(self.session);
        self.server.shutdown();
        let _ = self.server.join().await;
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Polls `session.goal.pause` until the Goal method table answers.
async fn wait_goal_ready(client: &mut Client, session: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let reply = client
                .rpc(
                    "session.goal.pause",
                    json!({"sessionId": session, "branchId": ""}),
                )
                .await;
            match reply {
                // The method exists and accepts the change.
                Ok(_) => break,
                Err(error) if dim_client::app_error(&error) == Some("invalid_params") => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("the built-in Goal part activates");
}

/// One Goal input the branch holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: String,
    pub sequence: u64,
    /// The `<dim-goal ...>` marker line.
    pub marker: String,
    pub text: String,
    pub blocks: usize,
    pub mode: dim_agent_types::InputMode,
}

impl Block {
    /// The decoded value after `label`, up to the block's last blank line.
    pub fn value(&self, label: &str) -> String {
        block_value(&self.text, label)
    }
    pub fn body(&self) -> Vec<&str> {
        self.text.lines().collect()
    }
}

/// The value a change block carries after its `label` line, decoded by the
/// block encoding of Goal A.5.
pub fn block_value(text: &str, label: &str) -> String {
    let start = text.find(&format!("\n{label}\n")).unwrap() + label.len() + 2;
    let end = text.rfind("\n\n").unwrap();
    unescape(&text[start..end])
}

pub fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

fn block_of(event: &dim_agent_types::AgentEvent) -> Option<Block> {
    let EventKind::InputMessage(data) = &event.kind else {
        return None;
    };
    let dim_agent_types::InputBlock::Raw { text, .. } = data.blocks.first()?;
    text.starts_with("<dim-goal ").then(|| Block {
        id: event.id.clone(),
        sequence: event.sequence,
        marker: text.lines().next().unwrap().to_owned(),
        text: text.clone(),
        blocks: data.blocks.len(),
        mode: data.effective_mode(),
    })
}

/// Goal's blocks on `branch`, in order: each input's first `<dim-goal>`
/// marker line, then the input's whole text.
pub async fn goal_writes(session: &Session, branch: &str) -> Vec<Block> {
    branch_events(session, branch, ..)
        .await
        .unwrap_or_else(|error| panic!("the facts of {branch}: {error}"))
        .iter()
        .filter_map(|event| block_of(event))
        .collect()
}

/// Waits until a Goal write on `branch` contains `needle`; returns it.
pub async fn wait_goal(session: &Session, branch: &str, needle: &str) -> Block {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(events) = branch_events(session, branch, ..).await
                && let Some(block) = events
                    .iter()
                    .filter_map(|event| block_of(event))
                    .find(|block| block.text.contains(needle))
            {
                return block;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no Goal write with {needle} on {branch}"))
}

/// The full text of the last Goal block on `branch` whose marker contains
/// `needle`.
pub async fn goal_block(session: &Session, branch: &str, needle: &str) -> String {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .filter_map(|event| block_of(event))
        .find(|block| block.marker.contains(needle))
        .unwrap_or_else(|| panic!("no Goal block with {needle} on {branch}"))
        .text
}

/// The raw blocks of the last Goal input on `branch` whose marker contains
/// `needle`, in order.
pub async fn goal_blocks(session: &Session, branch: &str, needle: &str) -> Vec<String> {
    let events = branch_events(session, branch, ..)
        .await
        .unwrap_or_else(|error| panic!("no Goal input with {needle} on {branch}: {error}"));
    events
        .iter()
        .rev()
        .find_map(|event| {
            let EventKind::InputMessage(data) = &event.kind else {
                return None;
            };
            let dim_agent_types::InputBlock::Raw { text, .. } = data.blocks.first()?;
            text.lines()
                .next()
                .is_some_and(|line| text.starts_with("<dim-goal ") && line.contains(needle))
                .then(|| {
                    data.blocks
                        .iter()
                        .map(|block| {
                            let dim_agent_types::InputBlock::Raw { text, .. } = block;
                            text.clone()
                        })
                        .collect()
                })
        })
        .unwrap_or_else(|| panic!("no Goal input with {needle} on {branch}"))
}

/// Sequences of the `turn/end` events on `branch`.
pub async fn turn_ends(session: &Session, branch: &str) -> Vec<u64> {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnEnd(_)))
        .map(|event| event.sequence)
        .collect()
}

/// Waits until a local event on `branch` matches. A branch that is still
/// loading has no published events yet.
pub async fn wait_event(session: &Session, branch: &str, matches: impl Fn(&EventKind) -> bool) {
    for _ in 0..750 {
        let Ok(events) = branch_events(session, branch, ..).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        if events.iter().any(|event| matches(&event.kind)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let events = branch_events(session, branch, ..).await.unwrap_or_default();
    panic!(
        "event did not arrive: {:#?}",
        events
            .iter()
            .map(|event| serde_json::to_string(&event.kind).unwrap())
            .collect::<Vec<_>>()
    );
}

/// The `branch/create` events on `branch` in order, with their sequences.
pub async fn branch_creates(session: &Session, branch: &str) -> Vec<(u64, BranchCreateData)> {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreate(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .collect()
}

/// The user text of the first input on `branch`. A branch that is still being
/// built has no published events yet.
pub async fn first_user_text(session: &Session, branch: &str) -> String {
    for _ in 0..750 {
        let Ok(events) = branch_events(session, branch, ..).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        for event in events {
            if let EventKind::InputMessage(data) = &event.kind
                && let Some(user) = &data.user
                && let Some(dim_agent_types::ContentBlock::Text { text }) = user.content.first()
            {
                return text.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no user input on {branch}")
}

/// Every tool result text on `branch`, with whether it was an error.
pub async fn tool_texts(session: &Session, branch: &str) -> Vec<(String, bool)> {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(&data.results),
            _ => None,
        })
        .flatten()
        .map(|result| {
            let text = match &result.result.content {
                dim_agent_types::ToolContent::Text(text) => text.clone(),
                dim_agent_types::ToolContent::Blocks(blocks) => {
                    serde_json::to_string(blocks).unwrap()
                }
            };
            (text, result.result.is_error == Some(true))
        })
        .collect()
}

/// Waits until a tool result with exactly `text` is on `branch`.
pub async fn wait_tool_text(session: &Session, branch: &str, text: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if tool_texts(session, branch)
                .await
                .iter()
                .any(|(got, _)| got == text)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no tool result {text:?} on {branch}"));
}

/// The last model request on `branch`, as the model received it.
pub async fn last_request(session: &Session, branch: &str) -> Arc<ModelRequest> {
    chain_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("a model call")
}

/// The system text of the last model call on `branch`.
pub async fn last_system(session: &Session, branch: &str) -> String {
    let request = last_request(session, branch).await;
    system_of(request.system.as_deref().expect("a system text"))
}

fn system_of(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                SystemPromptPart::Text { text, .. } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The `goal_update` tool's declaration, as the model receives it.
pub async fn assert_goal_tool(session: &Session, branch: &str) {
    let request = last_request(session, branch).await;
    let tools = request.tools.as_deref().expect("a request with tools");
    let tool = tools
        .iter()
        .find(|tool| tool.name == "goal_update")
        .unwrap_or_else(|| panic!("goal_update is offered: {tools:#?}"));
    assert_eq!(tool.description, texts::TOOL_DESCRIPTION);
    assert_eq!(
        serde_json::to_value(&tool.input_schema).unwrap(),
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["completed", "blocked"]},
                "summary": {"type": "string", "minLength": 1, "maxLength": 2000},
            },
            "required": ["status", "summary"],
            "additionalProperties": false,
        }),
        "the goal_update schema"
    );
    assert_eq!(tool.source, dim_agent_types::ToolSource::Extension);
    assert_eq!(tool.risk.as_deref(), Some("session"));
}

/// The evaluation a `branch/create` on `branch` names.
pub async fn evaluation_of(session: &Session, branch: &str) -> String {
    let creates = branch_creates(session, branch).await;
    let creation = &creates.first().expect("the evaluator's creation").1;
    let dim_agent_types::BranchSource::Host { operation_id } = &creation.source else {
        panic!("a host creation: {creation:#?}")
    };
    operation_id
        .strip_prefix("goal:evaluate:")
        .expect("Goal's operation id")
        .to_owned()
}

/// The facts `session` holds on `branch` within `range`, oldest first.
pub async fn branch_events<R: std::ops::RangeBounds<u64> + Send>(
    session: &Session,
    branch: &str,
    range: R,
) -> dim_agent_runtime::Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    use dim_agent_runtime::{
        TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy, TimelineQueryScope,
    };
    use std::ops::Bound;
    let start = match range.start_bound() {
        Bound::Included(start) => *start,
        Bound::Excluded(start) => start + 1,
        Bound::Unbounded => 1,
    };
    let end = match range.end_bound() {
        Bound::Included(end) => Some(*end),
        Bound::Excluded(end) => Some(end.saturating_sub(1)),
        Bound::Unbounded => None,
    };
    let mut events = Vec::new();
    let mut after = start.saturating_sub(1);
    loop {
        let reply = session
            .query(
                branch,
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from_event: None,
                    from_sequence: (after > 0).then_some(after),
                    order_by: TimelineQueryOrderBy::Sequence,
                    direction: TimelineQueryDirection::Ascending,
                    include_publishing: false,
                    kinds: Vec::new(),
                    flags: Vec::new(),
                    event_id: None,
                    predicate: None,
                    predicates: Vec::new(),
                    text: None,
                    limit: std::num::NonZeroU64::new(4096).unwrap(),
                    offset: 0,
                },
            )
            .await?;
        let short = reply.events.len() < 4096;
        events.extend(
            reply
                .events
                .into_iter()
                .filter(|event| end.is_none_or(|end| event.sequence <= end)),
        );
        match events.last() {
            Some(last) if !short => after = last.sequence,
            _ => break,
        }
        if end.is_some_and(|end| events.last().unwrap().sequence >= end) {
            break;
        }
    }
    Ok(events)
}

fn home() -> PathBuf {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let home = std::env::temp_dir().join(format!("dim-goal-{}", &id[..16]));
    let socket = home.join("server.sock");
    assert!(
        socket.as_os_str().len() <= 103,
        "test home socket path is {} bytes, over the 103-byte Unix socket limit; \
         point TMPDIR at a shorter directory: {}",
        socket.as_os_str().len(),
        socket.display()
    );
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    home
}

pub fn cleanup(home: &Path) {
    let _ = std::fs::remove_dir_all(home);
}

pub fn assert_ok(value: &Value) {
    assert!(value.get("error").is_none(), "{value}");
}

/// A Session input as an event, the way a user writes one.
pub fn user_input(text: &str, mode: dim_agent_types::InputMode) -> EventKind {
    EventKind::InputMessage(dim_agent_types::InputMessageData {
        user: Some(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text { text: text.into() }],
        }),
        mode: Some(mode),
        ..Default::default()
    })
}
