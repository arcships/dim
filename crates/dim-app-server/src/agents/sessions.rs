//! The `sessions` process service, see
//! `docs/design/appserver/sessions-service.md`: named, data-only access to
//! Sessions for process plugins in any language. The typed methods live on
//! [`AgentIndex`]; Agents' directory part, which never reloads, serves them by
//! name and looks up the index on each call.
use super::AgentIndex;
use crate::protocol::{PROTOCOL_VERSION, SessionId, session_cwd};
use dim_agent_capabilities::{
    input_queue::{InputQueue, Withdrawal},
    permission::PermissionState,
};
use dim_agent_runtime::{AgentError, Publication, Session};
use dim_agent_types::{
    AgentEvent, ContentBlock, EventKind, InputBlock, InputMessageData, InputMode,
    InputWithdrawData, ModelId, ModelSelection, PermissionPolicy, PermissionPreset, UserInput,
};
use rutis::Ctx;
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use std::{future::Future, sync::Arc};

/// A branch of a Session.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRef {
    pub session_id: String,
    pub branch_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchRequest {
    /// Agent id; absent: the default Agent.
    #[serde(default)]
    pub agent: Option<String>,
    pub cwd: String,
    /// Replaces the Agent profile's model.
    #[serde(default)]
    pub model: Option<ModelId>,
    /// Capped by the bound; absent: the starter's policy, else read-only.
    #[serde(default)]
    pub policy: Option<PermissionPolicy>,
    /// The branch whose current policy bounds the launch; absent: the host's
    /// Session defaults.
    #[serde(default)]
    pub starter: Option<SessionRef>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchReply {
    pub session_id: String,
    /// The effective policy the Session was created with.
    pub policy: PermissionPolicy,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputWrite {
    pub session_id: String,
    pub branch_id: String,
    pub mode: InputMode,
    /// `<source>:<uuid>` by convention; writes are not deduplicated.
    pub submission_id: String,
    /// Model-facing blocks; the first is the caller's marker.
    pub raw_blocks: Vec<RawBlock>,
    /// The user part, placed after the blocks; empty: none.
    pub content: Vec<ContentBlock>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBlock {
    pub text: String,
    #[serde(default)]
    pub key: Option<BlockKey>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockKey {
    pub r#use: KeyUse,
    pub name: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyUse {
    /// Stays in effect until a later block with the same key.
    Live,
    /// Delivered once; the key leaves effect.
    Retire,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputReceipt {
    pub input_id: String,
    pub sequence: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupRequest {
    pub session_id: String,
    pub branch_id: String,
    pub submission_id: String,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum LookupReply {
    #[serde(rename_all = "camelCase")]
    Accepted {
        input_id: String,
        sequence: u64,
    },
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryRequest {
    pub session_id: String,
    pub branch_id: String,
    /// Exclusive lower bound that ends a batch; absent: the branch start.
    #[serde(default)]
    pub after: Option<u64>,
    /// At most the published head; absent: the head.
    #[serde(default)]
    pub through: Option<u64>,
    /// A fixed-prefix read: `through` is the published head, which must
    /// reach this sequence.
    #[serde(default)]
    pub at_least: Option<u64>,
    /// Events per page before the page runs to the end of its batch;
    /// 1 to 256, absent: 256.
    #[serde(default)]
    pub limit: Option<usize>,
}
/// The default and largest `history` page: paging bounds each reply that
/// crosses the bridge (sessions service D11).
const HISTORY_PAGE_LIMIT: usize = 256;
/// The branch's own events in `(after, through]`, ending on a batch boundary.
#[derive(Debug, Serialize)]
pub struct HistoryPage {
    pub through: u64,
    pub events: Vec<Arc<AgentEvent>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithdrawRequest {
    pub session_id: String,
    pub branch_id: String,
    /// The `inputId` the caller received when it wrote the input.
    pub input_id: String,
    /// Copied into `input/withdraw` for clients to show.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopRequest {
    pub session_id: String,
    pub branch_id: String,
    /// Default: "Stopped by automation".
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionsErrorCode {
    Rejected,
    AgentUnknown,
    SessionClosed,
    BranchMissing,
    Reloading,
    FutureSequence,
    Ambiguous,
    PublicationUnknown,
    Failed,
}
/// A `sessions` failure, carried as data (session-access A.6).
#[derive(Debug, Clone)]
pub struct SessionsError {
    pub code: SessionsErrorCode,
    pub message: String,
}
impl SessionsError {
    fn new(code: SessionsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn rejected(message: impl Into<String>) -> Self {
        Self::new(SessionsErrorCode::Rejected, message)
    }
    fn reloading(what: &str) -> Self {
        Self::new(SessionsErrorCode::Reloading, format!("{what} is reloading"))
    }
    /// The caller may retry, after `lookup` or `history` where a write may
    /// have committed.
    pub fn retryable(&self) -> bool {
        matches!(
            self.code,
            SessionsErrorCode::Reloading | SessionsErrorCode::PublicationUnknown
        )
    }
}
impl From<AgentError> for SessionsError {
    fn from(error: AgentError) -> Self {
        let code = match error {
            AgentError::Closed(_) => SessionsErrorCode::SessionClosed,
            AgentError::UnknownBranch(_) => SessionsErrorCode::BranchMissing,
            AgentError::Reloading(_) => SessionsErrorCode::Reloading,
            AgentError::InvalidInput(_) => SessionsErrorCode::Rejected,
            _ => SessionsErrorCode::Failed,
        };
        Self::new(code, error.to_string())
    }
}

/// Waits until `sequence` is published, so a read right after the reply sees
/// it. A branch that closes or fails first leaves the write unconfirmed.
async fn published(publication: &Publication, sequence: u64) -> Result<(), SessionsError> {
    let _ = publication
        .watch()
        .wait_for(|&(published, closed)| published >= sequence || closed)
        .await;
    if publication.sequence() < sequence {
        return Err(SessionsError::new(
            SessionsErrorCode::PublicationUnknown,
            "committed, but publication was not confirmed; look it up after recovery",
        ));
    }
    Ok(())
}

/// A failed wait for a branch to settle (`stop`, `idle`): apart from a closed
/// Session, an unknown branch or a reloading capability, it is the branch's
/// dispatch failure.
fn settlement(error: AgentError) -> SessionsError {
    match error {
        AgentError::Closed(_) | AgentError::UnknownBranch(_) | AgentError::Reloading(_) => {
            error.into()
        }
        error => SessionsError::new(SessionsErrorCode::Failed, error.to_string()),
    }
}

/// What a branch holds for one `submission_id`.
pub(crate) enum Lookup {
    Accepted(Arc<AgentEvent>),
    Unknown,
    Ambiguous,
}
/// Finds the `input/message` with `submission_id` on a branch: first waits
/// until nothing committed is unpublished, so `Unknown` is final for every
/// write that returned before the call, then scans the published pages. On
/// a branch that keeps committing, the wait lasts until its commits pause
/// (sessions service §6.3).
/// `sessions.lookup` and Rpc's `session.input_lookup` share it.
pub(crate) async fn find_input(
    session: &Session,
    branch: &str,
    submission_id: &str,
) -> dim_agent_runtime::Result<Lookup> {
    let publication = session.publication(branch)?;
    let mut published = publication.watch();
    loop {
        published.borrow_and_update();
        if !publication.has_pending() || published.changed().await.is_err() {
            break;
        }
    }
    if let Some(error) = publication.failure() {
        return Err(error);
    }
    let through = session.sequence(branch)?;
    let mut after = 0;
    let mut found = None;
    // ponytail: O(n) native scan; add a store-owned index only if lookup latency requires it.
    while after < through {
        let events = session.local_page(branch, after, 256, through).await?;
        after = events
            .last()
            .ok_or_else(|| AgentError::Storage("published input page is missing".into()))?
            .sequence;
        for event in events {
            if matches!(&event.kind, EventKind::InputMessage(data)
                if data.submission_id.as_deref() == Some(submission_id))
            {
                if found.is_some() {
                    return Ok(Lookup::Ambiguous);
                }
                found = Some(event);
            }
        }
    }
    Ok(found.map_or(Lookup::Unknown, Lookup::Accepted))
}

/// The launch cap (sessions service §6.1): the lower preset, the union of the
/// disallowed tools, and only the allowed tools both sides allow
/// unconditionally. It is never wider than either side.
fn cap(requested: &PermissionPolicy, bound: &PermissionPolicy) -> PermissionPolicy {
    fn rank(preset: PermissionPreset) -> u8 {
        match preset {
            PermissionPreset::ReadOnly => 0,
            PermissionPreset::WorkspaceWrite => 1,
            PermissionPreset::FullAccess => 2,
        }
    }
    fn allows(policy: &PermissionPolicy, tool: &String) -> bool {
        policy.preset == PermissionPreset::FullAccess || policy.allowed_tools.contains(tool)
    }
    let mut disallowed_tools = Vec::new();
    for tool in requested
        .disallowed_tools
        .iter()
        .chain(&bound.disallowed_tools)
    {
        if !disallowed_tools.contains(tool) {
            disallowed_tools.push(tool.clone());
        }
    }
    let mut allowed_tools = Vec::new();
    for tool in requested.allowed_tools.iter().chain(&bound.allowed_tools) {
        if allows(requested, tool)
            && allows(bound, tool)
            && !disallowed_tools.contains(tool)
            && !allowed_tools.contains(tool)
        {
            allowed_tools.push(tool.clone());
        }
    }
    PermissionPolicy {
        preset: std::cmp::min_by_key(requested.preset, bound.preset, |preset| rank(*preset)),
        allowed_tools,
        disallowed_tools,
    }
}

impl AgentIndex {
    /// Ends a call with `reloading` when this index generation unloads; the
    /// `sessions` service and its callers stay (§8.3).
    async fn until_unloaded<T>(
        &self,
        call: impl Future<Output = Result<T, SessionsError>>,
    ) -> Result<T, SessionsError> {
        tokio::select! {
            biased;
            _ = self.stopping.cancelled() => Err(SessionsError::reloading("the Agents index")),
            result = call => result,
        }
    }

    /// Creates a non-interactive Session whose policy is the request capped
    /// by the starter branch's current policy, or by the host's Session
    /// defaults without a starter.
    pub async fn launch(
        self: &Arc<Self>,
        request: LaunchRequest,
    ) -> Result<LaunchReply, SessionsError> {
        let (host, config, policy) = self
            .until_unloaded(async {
                if request.policy.as_ref().is_some_and(|policy| {
                    policy
                        .allowed_tools
                        .iter()
                        .chain(&policy.disallowed_tools)
                        .any(|name| name.trim().is_empty())
                }) {
                    return Err(SessionsError::rejected("Tool names must not be empty"));
                }
                let host = self.host(request.agent.as_deref()).ok_or_else(|| {
                    let id = request.agent.as_deref().unwrap_or(self.default_agent_id());
                    match self.unavailable_agent(id) {
                        Some(reason) => SessionsError::new(SessionsErrorCode::Failed, reason),
                        None => SessionsError::new(
                            SessionsErrorCode::AgentUnknown,
                            format!("unknown agent: {id}"),
                        ),
                    }
                })?;
                let cwd = session_cwd(&request.cwd)
                    .await
                    .map_err(|error| SessionsError::rejected(error.message))?;
                // Read once under a lease; a starter that cannot be read fails
                // the launch rather than falling back to a possibly wider bound.
                let bound = match &request.starter {
                    Some(starter) => {
                        let lease = self.acquire(&starter.session_id).await?;
                        lease
                            .session
                            .get::<PermissionState>(&starter.branch_id)?
                            .ok_or_else(|| SessionsError::reloading("the starter's Permission"))?
                            .policy
                    }
                    None => self.session_defaults.permission_policy.clone(),
                };
                let requested = match request.policy {
                    Some(policy) => policy,
                    None if request.starter.is_some() => bound.clone(),
                    None => PermissionPolicy::default(),
                };
                let policy = cap(&requested, &bound);
                let mut config = super::session_config(&host.profile, &self.session_defaults);
                config.cwd = cwd;
                config.permission_policy = policy.clone();
                config.interactive = false;
                if self.selects_models {
                    let selection = self
                        .model_selection()
                        .ok_or_else(|| SessionsError::reloading("model selection"))?;
                    config.model = selection
                        .0
                        .resolve(
                            request.model.or_else(|| host.profile.model.clone()),
                            Some(config.cwd.clone().into()),
                            self.stopping.clone(),
                        )
                        .await
                        .map_err(|error| {
                            SessionsError::new(
                                SessionsErrorCode::Failed,
                                format!("model resolution: {error}"),
                            )
                        })?;
                } else if let Some(model) = request.model {
                    config.model = ModelSelection {
                        provider_id: model.provider_id,
                        model_id: model.model_id,
                        capability_snapshot: None,
                        config: None,
                        metadata: None,
                    };
                }
                Ok((host, config, policy))
            })
            .await?;
        // Clients see a loaded Session once its protocol version is
        // registered: creation and registration finish together even when
        // the caller stops waiting. Once admitted, the reply is the creation's
        // own outcome, not the index unload: a `reloading` reply would invite
        // a retry while this task may still create the first Session (§6.1).
        let index = self.clone();
        let session = tokio::spawn(async move {
            let session = index
                .create_session(&host, config, PROTOCOL_VERSION)
                .await?;
            index.retain_versions();
            Ok::<_, AgentError>(session)
        })
        .await
        .map_err(|error| SessionsError::new(SessionsErrorCode::Failed, error.to_string()))?
        .map_err(|error| match error {
            AgentError::Reloading(_) => SessionsError::from(error),
            error => SessionsError::new(SessionsErrorCode::Failed, error.to_string()),
        })?;
        Ok(LaunchReply {
            session_id: session.id().to_owned(),
            policy,
        })
    }

    /// Writes `input/message` and replies once it is published.
    pub async fn input(&self, write: InputWrite) -> Result<InputReceipt, SessionsError> {
        self.until_unloaded(async {
            if write.submission_id.is_empty() {
                return Err(SessionsError::rejected("submissionId must not be empty"));
            }
            let lease = self.acquire(&write.session_id).await?;
            let publication = lease.session.publication(&write.branch_id)?;
            let blocks = write
                .raw_blocks
                .into_iter()
                .map(|block| InputBlock::Raw {
                    text: block.text,
                    retire: block
                        .key
                        .as_ref()
                        .is_some_and(|key| key.r#use == KeyUse::Retire),
                    key: block.key.map(|key| key.name),
                })
                .collect();
            let event = lease
                .session
                .input(
                    InputMessageData {
                        submission_id: Some(write.submission_id),
                        user: (!write.content.is_empty()).then_some(UserInput {
                            content: write.content,
                        }),
                        blocks,
                        mode: Some(write.mode),
                    },
                    &write.branch_id,
                )
                .await?;
            published(&publication, event.sequence).await?;
            Ok(InputReceipt {
                input_id: event.id.clone(),
                sequence: event.sequence,
            })
        })
        .await
    }

    /// Finds a committed input by its `submissionId` after an uncertain write.
    pub async fn lookup(&self, request: LookupRequest) -> Result<LookupReply, SessionsError> {
        self.until_unloaded(async {
            if request.submission_id.is_empty() {
                return Err(SessionsError::rejected("submissionId must not be empty"));
            }
            let lease = self.acquire(&request.session_id).await?;
            match find_input(&lease.session, &request.branch_id, &request.submission_id).await? {
                Lookup::Accepted(event) => Ok(LookupReply::Accepted {
                    input_id: event.id.clone(),
                    sequence: event.sequence,
                }),
                Lookup::Unknown => Ok(LookupReply::Unknown),
                Lookup::Ambiguous => Err(SessionsError::new(
                    SessionsErrorCode::Ambiguous,
                    "more than one input has this submissionId",
                )),
            }
        })
        .await
    }

    /// Pages the branch's own events through a fixed sequence. A caller pages
    /// with `after` set to the last returned sequence until it reaches
    /// `through`.
    pub async fn history(&self, request: HistoryRequest) -> Result<HistoryPage, SessionsError> {
        self.until_unloaded(async {
            let limit = request.limit.unwrap_or(HISTORY_PAGE_LIMIT);
            if !(1..=HISTORY_PAGE_LIMIT).contains(&limit) {
                return Err(SessionsError::rejected(format!(
                    "limit must be between 1 and {HISTORY_PAGE_LIMIT}; read more pages by passing the last sequence as after"
                )));
            }
            if request.at_least.is_some() && request.through.is_some() {
                return Err(SessionsError::rejected(
                    "atLeast and through are mutually exclusive",
                ));
            }
            let lease = self.acquire(&request.session_id).await?;
            let head = lease.session.sequence(&request.branch_id)?;
            let through = match (request.at_least, request.through) {
                (Some(bound), _) | (_, Some(bound)) if bound > head => {
                    return Err(SessionsError::new(
                        SessionsErrorCode::FutureSequence,
                        format!("sequence {bound} is beyond the published head {head}"),
                    ));
                }
                (None, Some(through)) => through,
                _ => head,
            };
            let after = request.after.unwrap_or(0);
            if after > through {
                return Err(SessionsError::rejected("after exceeds through"));
            }
            let events = lease
                .session
                .local_page(&request.branch_id, after, limit, through)
                .await?;
            Ok(HistoryPage { through, events })
        })
        .await
    }

    /// Withdraws an input the caller wrote, by its `inputId`, through the
    /// branch's InputQueue, and returns InputQueue's decision. A deletion
    /// replies once `input/deleted` is published.
    pub async fn withdraw(&self, request: WithdrawRequest) -> Result<Withdrawal, SessionsError> {
        self.until_unloaded(async {
            let lease = self.acquire(&request.session_id).await?;
            let publication = lease.session.publication(&request.branch_id)?;
            let (withdrawal, _) = lease
                .session
                .service::<InputQueue>(&request.branch_id)?
                .withdraw(
                    InputWithdrawData {
                        input_id: request.input_id,
                        reason: request.reason,
                    },
                    None,
                )
                .await?;
            if let Withdrawal::Deleted { sequence } = withdrawal {
                published(&publication, sequence).await?;
            }
            Ok(withdrawal)
        })
        .await
    }

    /// Interrupts the branch's turn and replies once it has settled, with
    /// the dispatched sequence.
    pub async fn stop(&self, request: StopRequest) -> Result<u64, SessionsError> {
        self.until_unloaded(async {
            let lease = self.acquire(&request.session_id).await?;
            lease
                .session
                .stop(
                    &request.branch_id,
                    request
                        .reason
                        .unwrap_or_else(|| "Stopped by automation".into()),
                )
                .await
                .map_err(settlement)
        })
        .await
    }

    /// Waits for the branch's first idle point after the call starts and
    /// returns its dispatched sequence. A pending approval or question keeps
    /// the turn open, so the wait lasts until it is answered or `stop` ends
    /// the turn; there is no timeout.
    pub async fn idle(&self, at: SessionRef) -> Result<u64, SessionsError> {
        self.until_unloaded(async {
            let lease = self.acquire(&at.session_id).await?;
            let session = &lease.session;
            let idle = tokio::select! {
                idle = session.idle(&at.branch_id) => idle,
                _ = session.closed() => Err(AgentError::Closed(session.id().into())),
            };
            if let Some(fault) = session.failure() {
                return Err(SessionsError::new(
                    SessionsErrorCode::Failed,
                    fault.to_string(),
                ));
            }
            // A close settles the turn, so the facade could report the
            // close's cutoff as an idle point.
            if session.is_closing() {
                return Err(SessionsError::new(
                    SessionsErrorCode::SessionClosed,
                    format!("{} is closed", session.id()),
                ));
            }
            idle.map_err(settlement)
        })
        .await
    }
}

/// `sessions` by name for every language: each call decodes one request
/// object, calls the current index and replies with one envelope,
/// `{ok: true, value}` or `{ok: false, error: {code, message, retryable}}`.
pub(crate) struct SessionsService(pub(crate) Ctx);
impl HostDispatch for SessionsService {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({
            "launch": "async", "input": "async", "lookup": "async",
            "history": "async", "withdraw": "async", "stop": "async", "idle": "async",
            "delete": "async",
        }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        // Looked up per call, so an index reload never reloads a caller.
        let index = self.0.get::<AgentIndex>();
        let method = method.to_owned();
        let args = args.json();
        Ok(Value::future(async move {
            let result = match (index, args) {
                (None, _) => Err(SessionsError::reloading("the Agents index")),
                (_, Err(error)) => Err(SessionsError::rejected(error.to_string())),
                (Some(index), Ok(args)) => call(&index, &method, args).await,
            };
            Ok(match result {
                Ok(value) => json!({"ok": true, "value": value}),
                Err(error) => json!({"ok": false, "error": {
                    "code": error.code,
                    "message": error.message,
                    "retryable": error.retryable(),
                }}),
            }
            .into())
        }))
    }
}

fn request<T: DeserializeOwned>(args: serde_json::Value) -> Result<T, SessionsError> {
    let serde_json::Value::Array(args) = args else {
        return Err(SessionsError::rejected("expected one request object"));
    };
    let Ok([request]) = <[serde_json::Value; 1]>::try_from(args) else {
        return Err(SessionsError::rejected("expected one request object"));
    };
    serde_json::from_value(request).map_err(|error| SessionsError::rejected(error.to_string()))
}
fn reply(value: impl Serialize) -> Result<serde_json::Value, SessionsError> {
    Ok(serde_json::to_value(value).expect("sessions replies are JSON data"))
}

async fn call(
    index: &Arc<AgentIndex>,
    method: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, SessionsError> {
    match method {
        "launch" => reply(index.launch(request(args)?).await?),
        "input" => reply(index.input(request(args)?).await?),
        "lookup" => reply(index.lookup(request(args)?).await?),
        "history" => reply(index.history(request(args)?).await?),
        "withdraw" => reply(match index.withdraw(request(args)?).await? {
            Withdrawal::Deleted { sequence } => json!({"status": "deleted", "sequence": sequence}),
            Withdrawal::NotHeld => json!({"status": "notHeld"}),
        }),
        "stop" => reply(json!({"sequence": index.stop(request(args)?).await?})),
        "idle" => reply(json!({"sequence": index.idle(request(args)?).await?})),
        // S5c's delete (§6.8): admission gate, close, then the Session's rows
        // and files. Like Rpc's `session.delete` it runs to its own outcome
        // across an index reload, so a reload never leaves it half done.
        "delete" => {
            let SessionId { session_id } = request(args)?;
            index.delete(&session_id).await?;
            reply(json!({}))
        }
        _ => Err(SessionsError::rejected(format!(
            "unknown sessions method {method}"
        ))),
    }
}
