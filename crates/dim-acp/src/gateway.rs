use crate::outbox::{Outbox, Reply as Responder};
use crate::{
    config::Config,
    projection::Projection,
    rpc::{self, Native, Rpc},
};
use agent_client_protocol::{Client, ConnectionTo, Error, UntypedMessage, schema::v1 as acp};
use dim_agent_types::TurnOutcome;
use dim_client::wire::{Event, EventContent, History, Progress};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
};
use tokio::sync::mpsc;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) enum Incoming {
    Request(UntypedMessage, Box<Responder>),
    Notification(UntypedMessage),
    Work(u64, Box<Work>),
}

pub(crate) enum Work {
    Initialized {
        reply: Responder,
        result: Result<Vec<crate::auth::Method>, Error>,
    },
    Authenticated {
        reply: Responder,
        provider: String,
        result: Result<Value, Error>,
    },
    Created {
        reply: Responder,
        cwd: String,
        servers: crate::mcp::Servers,
        result: Result<Created, Error>,
    },
    Prepared {
        id: String,
        preparation: u64,
        result: Result<Prepared, Error>,
    },
    Submitted {
        id: String,
        submission: String,
        result: Result<Submitted, Error>,
        sent: bool,
    },
    Cancelled {
        id: String,
        submission: Option<String>,
        result: Result<Value, Error>,
    },
    Removed {
        id: String,
        reply: Responder,
        result: Result<Value, Error>,
    },
    Configured {
        id: String,
        reply: Responder,
        mode: bool,
        revision: u64,
        result: Result<Config, Error>,
        refreshed: Option<Config>,
    },
    Answer {
        key: String,
        method: &'static str,
        args: Option<Value>,
    },
    Usage {
        id: String,
        result: Result<Value, Error>,
    },
    Recovered {
        id: String,
        result: Box<Result<(Prepared, Option<Event>), Error>>,
    },
    Listed {
        reply: Responder,
        result: Result<Vec<SessionInfo>, Error>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Created {
    session_id: String,
}

#[derive(Deserialize)]
pub(crate) struct Submitted {
    event: Event,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionInfo {
    session_id: String,
    cwd: String,
    title: Option<String>,
    updated_at: Option<u64>,
    title_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventNotice {
    session_id: String,
    branch_id: String,
    event: Event,
}

pub(crate) struct Prepared {
    history: Vec<Event>,
    through: u64,
    config: Config,
    metadata: Option<SessionInfo>,
}

struct Waiter {
    reply: Responder,
    kind: Preparation,
}
#[derive(Clone, Copy)]
enum Preparation {
    New,
    Load,
    Resume,
    History,
}
struct Loading {
    preparation: u64,
    mcp_added: bool,
    cwd: String,
    waiters: Vec<Waiter>,
    buffered: Vec<(String, Value)>,
    previous: Option<Box<Session>>,
}
struct Prompt {
    reply: Responder,
    submission: String,
    input: Option<String>,
    turn: Option<String>,
    cancelled: bool,
    cancelling: bool,
    terminal: Option<Result<Value, Error>>,
    uploading: CancellationToken,
    cancel_uncertain: bool,
    consumed: HashMap<String, String>,
    ended: HashMap<String, TurnOutcome>,
    max_tokens: HashSet<String>,
    deleted: HashSet<String>,
}
impl Prompt {
    fn accepted(&mut self, input: String) -> Option<Result<Value, Error>> {
        let deleted = self.deleted.remove(&input);
        self.deleted.clear();
        if self.turn.is_none() {
            self.turn = self.consumed.remove(&input);
        }
        self.input = Some(input);
        self.consumed.clear();
        let outcome = self.turn.as_ref().and_then(|turn| self.ended.remove(turn));
        self.ended.clear();
        self.max_tokens
            .retain(|turn| Some(turn) == self.turn.as_ref());
        if deleted {
            Some(Ok(json!({"stopReason":"cancelled"})))
        } else {
            let max_tokens = self
                .turn
                .as_ref()
                .is_some_and(|turn| self.max_tokens.contains(turn));
            outcome.map(|outcome| prompt_outcome(self.cancelled, max_tokens, &outcome))
        }
    }
}
fn prompt_outcome(
    cancelled: bool,
    max_tokens: bool,
    outcome: &TurnOutcome,
) -> Result<Value, Error> {
    if cancelled {
        return Ok(json!({"stopReason":"cancelled"}));
    }
    match outcome {
        TurnOutcome::Completed { .. } => Ok(json!({"stopReason":"end_turn"})),
        TurnOutcome::Cancelled { .. } => Ok(json!({"stopReason":"cancelled"})),
        TurnOutcome::Failed { .. } if max_tokens => Ok(json!({"stopReason":"max_tokens"})),
        TurnOutcome::Failed { error } => {
            Err(Error::new(-32001, error.clone()).data(json!({"code":"turn_failed"})))
        }
    }
}
struct Session {
    cwd: String,
    projection: Projection,
    prompt: Option<Prompt>,
    config: Config,
    configuring: bool,
    config_revision: u64,
    questions: dim_client::question::Pending,
    usage_pending: bool,
    title_revision: Option<u64>,
    subagents: crate::lody::Subagents,
    stopping: bool,
    removing: bool,
    pending_remove: Option<(&'static str, Responder)>,
    recovery: Option<Vec<(String, Value)>>,
}
enum Slot {
    Loading(Loading),
    Ready(Box<Session>),
}

pub(crate) struct Gateway {
    rpc: Rpc,
    cx: ConnectionTo<Client>,
    incoming: mpsc::Sender<Incoming>,
    tasks: TaskTracker,
    stop: CancellationToken,
    agent_id: Option<String>,
    initialized: bool,
    initializing: bool,
    creating: usize,
    sessions: HashMap<String, Slot>,
    interactions: HashMap<String, (String, CancellationToken)>,
    form: bool,
    login: Option<crate::auth::Login>,
    auth_methods: Vec<crate::auth::Method>,
    authenticating: std::collections::HashSet<String>,
    subagent_events: bool,
    recovering: bool,
    recovering_sessions: usize,
    outbox: Outbox,
    next_preparation: u64,
    mcp: crate::mcp::Bindings,
}

fn busy() -> Error {
    Error::new(-32001, "Session or gateway is busy").data(json!({"code":"session_busy"}))
}
fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::invalid_params())
}

fn establishment(cwd: &Path, roots: usize) -> Result<(), Error> {
    if !cwd.is_absolute() {
        return Err(Error::invalid_params());
    }
    if roots != 0 {
        return Err(
            Error::new(-32602, "Additional workspace directories are unsupported")
                .data(json!({"code":"unsupported_additional_directories"})),
        );
    }
    Ok(())
}

impl Gateway {
    // Keep the owned connection, task and host inputs explicit at composition.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        rpc: Rpc,
        cx: ConnectionTo<Client>,
        incoming: mpsc::Sender<Incoming>,
        tasks: TaskTracker,
        stop: CancellationToken,
        agent_id: Option<String>,
        login: Option<crate::auth::Login>,
        outbox: Outbox,
        mcp: crate::mcp::Bindings,
    ) -> Self {
        Self {
            rpc,
            cx,
            incoming,
            tasks,
            stop,
            agent_id,
            initialized: false,
            initializing: false,
            creating: 0,
            sessions: HashMap::new(),
            interactions: HashMap::new(),
            form: false,
            login,
            auth_methods: Vec::new(),
            authenticating: std::collections::HashSet::new(),
            subagent_events: false,
            recovering: false,
            recovering_sessions: 0,
            outbox,
            next_preparation: 0,
            mcp,
        }
    }

    fn spawn(&self, future: impl Future<Output = Work> + Send + 'static) {
        self.spawn_generation(self.rpc.generation(), future);
    }

    fn spawn_generation(
        &self,
        generation: u64,
        future: impl Future<Output = Work> + Send + 'static,
    ) {
        let incoming = self.incoming.clone();
        let stop = self.stop.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                () = stop.cancelled() => {},
                result = future => { let _ = incoming.send(Incoming::Work(generation,Box::new(result))).await; },
            }
        });
    }

    fn update(&self, id: &str, update: Value) -> Result<(), Error> {
        self.outbox.notify(
            &self.cx,
            "session/update",
            json!({"sessionId":id,"update":update}),
        )
    }

    fn prepare(&mut self, id: String, cwd: String, waiter: Waiter, attach: bool, mcp_added: bool) {
        self.next_preparation += 1;
        let preparation = self.next_preparation;
        let rpc = self.rpc.clone();
        let expected_cwd = (!matches!(waiter.kind, Preparation::New)).then(|| cwd.clone());
        let mcp = self.mcp.clone();
        let bind_mcp = !matches!(waiter.kind, Preparation::History);
        let previous = match self.sessions.remove(&id) {
            Some(Slot::Ready(session)) => Some(session),
            None => None,
            Some(Slot::Loading(_)) => unreachable!("preparations are coalesced at admission"),
        };
        self.sessions.insert(
            id.clone(),
            Slot::Loading(Loading {
                preparation,
                mcp_added,
                cwd,
                waiters: vec![waiter],
                buffered: Vec::new(),
                previous,
            }),
        );
        self.spawn(async move {
            let result = async {
                let prepared = read_history(&rpc, &id, attach, expected_cwd).await?;
                if bind_mcp {
                    mcp.ensure(&id).await?;
                }
                Ok(prepared)
            }
            .await;
            Work::Prepared {
                id,
                preparation,
                result,
            }
        });
    }

    fn request(&mut self, request: UntypedMessage, reply: Responder) -> Result<(), Error> {
        if self.recovering || !self.rpc.ready() {
            return reply.respond_with_error(rpc::unavailable());
        }
        if request.method == "initialize" {
            if self.initialized || self.initializing {
                return reply.respond_with_error(Error::invalid_request());
            }
            self.subagent_events = request
                .params
                .pointer("/clientCapabilities/_meta/lody/subagentEvents/version")
                == Some(&json!(1));
            let initialize: acp::InitializeRequest = match params(request.params) {
                Ok(value) => value,
                Err(error) => return reply.respond_with_error(error),
            };
            self.form = initialize
                .client_capabilities
                .elicitation
                .as_ref()
                .is_some_and(|capabilities| capabilities.supports_form());
            if ![
                "session.create",
                "session.attach",
                "session.detach",
                "session.submit",
                "session.input_lookup",
                "session.history",
                "session.stop",
            ]
            .iter()
            .all(|method| self.rpc.supports(method))
            {
                return reply.respond_with_error(Error::new(
                    -32001,
                    "App Server lacks required ACP session methods",
                ));
            }
            self.initializing = true;
            let rpc = self.rpc.clone();
            let login = self.login.clone();
            self.spawn(async move {
                Work::Initialized {
                    reply,
                    result: crate::auth::discover(
                        &rpc,
                        login.as_ref(),
                        initialize.client_capabilities.auth.terminal
                            || initialize
                                .client_capabilities
                                .meta
                                .as_ref()
                                .and_then(|meta| meta.get("terminal-auth"))
                                == Some(&json!(true)),
                    )
                    .await,
                }
            });
            return Ok(());
        }
        if !self.initialized {
            return reply.respond_with_error(Error::invalid_request());
        }
        let controls = match crate::lody::controls(&request.method, &request.params) {
            Ok(controls) => controls,
            Err(error) => return reply.respond_with_error(error),
        };
        let rpc = self.rpc.clone();
        match request.method.as_str() {
            "authenticate" => {
                let args: acp::AuthenticateRequest = match params(request.params) {
                    Ok(args) => args,
                    Err(error) => return reply.respond_with_error(error),
                };
                let Some(method) = self
                    .auth_methods
                    .iter()
                    .find(|method| {
                        method.id == args.method_id.to_string() && method.terminal.is_none()
                    })
                    .cloned()
                else {
                    return reply.respond_with_error(Error::invalid_params());
                };
                if !self.authenticating.insert(method.provider.clone()) {
                    return reply.respond_with_error(busy());
                }
                let login = self
                    .login
                    .clone()
                    .expect("advertised agent login has a host");
                let cancel = reply.cancellation();
                let stop = self.stop.clone();
                let incoming = self.incoming.clone();
                let generation = self.rpc.generation();
                self.tasks.spawn(async move {
                    let provider = method.provider.clone();
                    let result = crate::auth::authenticate(rpc, login, method, cancel, stop).await;
                    let _ = incoming
                        .send(Incoming::Work(
                            generation,
                            Box::new(Work::Authenticated {
                                reply,
                                provider,
                                result,
                            }),
                        ))
                        .await;
                });
            }
            "session/new" => {
                let request: acp::NewSessionRequest = match params(request.params) {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                let servers = match crate::mcp::servers(request.mcp_servers) {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                if let Err(error) =
                    establishment(&request.cwd, request.additional_directories.len())
                {
                    return reply.respond_with_error(error);
                }
                if self.sessions.len() + self.creating >= 128 {
                    return reply.respond_with_error(busy());
                }
                self.creating += 1;
                let agent = self.agent_id.clone();
                let managed: Vec<_> = self
                    .auth_methods
                    .iter()
                    .map(|method| method.provider.clone())
                    .collect();
                self.spawn(async move {
                    let mut cwd = request.cwd.to_string_lossy().into_owned();
                    let result = async {
                        cwd = tokio::fs::canonicalize(&request.cwd)
                            .await
                            .map_err(|_| Error::invalid_params())?
                            .into_os_string()
                            .into_string()
                            .map_err(|_| Error::invalid_params())?;
                        let mut args = json!({"cwd":cwd});
                        if let Some(mode) = controls.mode {
                            args["permissionPolicy"] = json!({"preset":mode});
                        }
                        let explicit = if let Some(model) = controls.startup {
                            if !rpc.supports("provider.catalog") {
                                return Err(Error::invalid_params());
                            }
                            let choices = crate::config::models(&rpc).await?;
                            let selected = choices
                                .into_iter()
                                .find(|choice| choice.id == model)
                                .ok_or_else(Error::invalid_params)?;
                            crate::auth::ensure(&rpc, &selected.provider, &managed).await?;
                            args["model"] =
                                json!({"provider":selected.provider,"modelId":selected.id});
                            true
                        } else {
                            false
                        };
                        if let Some(agent) = agent {
                            args["agentId"] = json!(agent);
                        }
                        if !explicit && !managed.is_empty() && rpc.supports("provider.selection") {
                            #[derive(Deserialize)]
                            struct Selection {
                                selection: Option<Selected>,
                            }
                            #[derive(Deserialize)]
                            struct Selected {
                                provider_id: String,
                            }
                            let selected: Selection =
                                rpc.typed("provider.selection", json!({"cwd":cwd})).await?;
                            if let Some(selected) = selected.selection {
                                crate::auth::ensure(&rpc, &selected.provider_id, &managed).await?;
                            }
                        }
                        rpc.typed("session.create", args).await
                    }
                    .await;
                    Work::Created {
                        reply,
                        cwd,
                        servers,
                        result,
                    }
                });
            }
            "session/load" | "session/resume" => {
                let replay = request.method == "session/load";
                let kind = if replay {
                    Preparation::Load
                } else {
                    Preparation::Resume
                };
                let parsed = if replay {
                    params::<acp::LoadSessionRequest>(request.params).and_then(|request| {
                        Ok((
                            request.session_id,
                            request.cwd,
                            request.additional_directories.len(),
                            crate::mcp::servers(request.mcp_servers)?,
                        ))
                    })
                } else {
                    params::<acp::ResumeSessionRequest>(request.params).and_then(|request| {
                        Ok((
                            request.session_id,
                            request.cwd,
                            request.additional_directories.len(),
                            crate::mcp::servers(request.mcp_servers)?,
                        ))
                    })
                };
                let (session_id, cwd, roots, servers) = match parsed {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                if let Err(error) = establishment(&cwd, roots) {
                    return reply.respond_with_error(error);
                }
                let id = session_id.to_string();
                let cwd = cwd.to_string_lossy().into_owned();
                match self.sessions.get_mut(&id) {
                    Some(Slot::Loading(loading)) => {
                        if loading.cwd != cwd {
                            return reply.respond_with_error(Error::invalid_params());
                        }
                        if loading.waiters.len() >= 128 || !self.mcp.can_coalesce(&id, &servers) {
                            return reply.respond_with_error(busy());
                        }
                        loading.waiters.push(Waiter { reply, kind });
                    }
                    Some(Slot::Ready(session)) => {
                        if session.prompt.is_some()
                            || session.removing
                            || session.stopping
                            || session.configuring
                        {
                            return reply.respond_with_error(busy());
                        }
                        if !replay && session.cwd == cwd && !self.mcp.supported() {
                            return reply.respond(session.config.response());
                        }
                        let added = match self.mcp.plan(&id, servers) {
                            Ok(added) => added,
                            Err(error) => return reply.respond_with_error(error),
                        };
                        self.prepare(id, cwd, Waiter { reply, kind }, false, added);
                    }
                    None => {
                        if self.sessions.len() >= 128 {
                            return reply.respond_with_error(busy());
                        }
                        let added = match self.mcp.plan(&id, servers) {
                            Ok(added) => added,
                            Err(error) => return reply.respond_with_error(error),
                        };
                        self.prepare(id, cwd, Waiter { reply, kind }, true, added);
                    }
                }
            }
            "_lody/session/history/read" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Request {
                    session_id: String,
                }
                let args: Request = match params(request.params) {
                    Ok(args) => args,
                    Err(error) => return reply.respond_with_error(error),
                };
                let Some(Slot::Ready(session)) = self.sessions.get(&args.session_id) else {
                    return reply
                        .respond_with_error(Error::resource_not_found(Some(args.session_id)));
                };
                if session.prompt.is_some()
                    || session.configuring
                    || session.removing
                    || session.stopping
                {
                    return reply.respond_with_error(busy());
                }
                let cwd = session.cwd.clone();
                self.prepare(
                    args.session_id,
                    cwd,
                    Waiter {
                        reply,
                        kind: Preparation::History,
                    },
                    false,
                    false,
                );
            }
            "session/fork" if self.rpc.supports("session.fork") => {
                let args: acp::ForkSessionRequest = match params(request.params) {
                    Ok(args) => args,
                    Err(error) => return reply.respond_with_error(error),
                };
                let servers = match crate::mcp::servers(args.mcp_servers) {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                if let Err(error) = establishment(&args.cwd, args.additional_directories.len()) {
                    return reply.respond_with_error(error);
                }
                if self.sessions.len() + self.creating >= 128 {
                    return reply.respond_with_error(busy());
                }
                self.creating += 1;
                self.spawn(async move {
                    let mut cwd = args.cwd.to_string_lossy().into_owned();
                    let result = async {
                        cwd = tokio::fs::canonicalize(&args.cwd)
                            .await
                            .map_err(|_| Error::invalid_params())?
                            .into_os_string()
                            .into_string()
                            .map_err(|_| Error::invalid_params())?;
                        let source = args.session_id.to_string();
                        let history = read_history(&rpc, &source, false, Some(cwd.clone())).await?;
                        let at = if let Some(turn) = controls.fork_turn {
                            history
                                .history
                                .iter()
                                .find_map(|event| match &event.content {
                                    EventContent::TurnEnd(data) if data.turn_id == turn => {
                                        Some(event.sequence)
                                    }
                                    _ => None,
                                })
                                .ok_or_else(Error::invalid_params)?
                        } else {
                            history.through
                        };
                        rpc.typed("session.fork", json!({"sessionId":source,"at":at}))
                            .await
                    }
                    .await;
                    Work::Created {
                        reply,
                        cwd,
                        servers,
                        result,
                    }
                });
            }
            "session/prompt" => {
                let request: acp::PromptRequest = match params(request.params) {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                let id = request.session_id.to_string();
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return reply.respond_with_error(Error::resource_not_found(Some(id)));
                };
                if session.prompt.is_some() || session.removing || session.stopping {
                    return reply.respond_with_error(busy());
                }
                let parts = match crate::content::parse(
                    request.prompt,
                    session.config.methods.contains("attachment.put"),
                ) {
                    Ok(parts) => parts,
                    Err(error) => return reply.respond_with_error(error),
                };
                let submission = uuid::Uuid::new_v4().to_string();
                let provider = session
                    .config
                    .model
                    .as_ref()
                    .map(|model| model.provider_id.clone());
                let managed: Vec<_> = self
                    .auth_methods
                    .iter()
                    .map(|method| method.provider.clone())
                    .collect();
                let uploading = CancellationToken::new();
                session.prompt = Some(Prompt {
                    reply,
                    submission: submission.clone(),
                    input: None,
                    turn: None,
                    cancelled: false,
                    cancelling: false,
                    terminal: None,
                    uploading: uploading.clone(),
                    cancel_uncertain: false,
                    consumed: HashMap::new(),
                    ended: HashMap::new(),
                    max_tokens: HashSet::new(),
                    deleted: HashSet::new(),
                });
                let mcp = self.mcp.clone();
                self.spawn(async move {
                    let mut sent = false;
                    let result = async {
                        mcp.ensure(&id).await?;
                        if let Some(provider) = provider { crate::auth::ensure(&rpc,&provider,&managed).await?; }
                        let user = tokio::select! {
                            () = uploading.cancelled() => return Err(Error::new(-32800, "Request cancelled before submission")),
                            user = crate::content::upload(&rpc, &id, parts) => user?,
                        };
                        if uploading.is_cancelled() { return Err(Error::new(-32800, "Request cancelled before submission")); }
                        sent = true;
                        rpc.typed("session.submit", json!({"sessionId":id,"branchId":"main","submissionId":submission,"user":user,"mode":"queue"})).await
                    }.await;
                    Work::Submitted { id, submission, result,sent }
                });
            }
            "session/close" | "session/delete" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Request {
                    session_id: String,
                }
                let args: Request = match params(request.params) {
                    Ok(args) => args,
                    Err(error) => return reply.respond_with_error(error),
                };
                let method = if request.method == "session/close" {
                    "session.close"
                } else {
                    "session.delete"
                };
                if !self.rpc.supports(method) {
                    return reply.respond_with_error(Error::method_not_found());
                }
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&args.session_id) else {
                    return reply
                        .respond_with_error(Error::resource_not_found(Some(args.session_id)));
                };
                if session.removing || session.configuring {
                    return reply.respond_with_error(busy());
                }
                session.removing = true;
                session.pending_remove = Some((method, reply));
                self.remove(&args.session_id);
            }
            "session/set_mode" | "session/set_config_option" => {
                let mode = request.method == "session/set_mode";
                let parsed = if mode {
                    params::<acp::SetSessionModeRequest>(request.params).map(|args| {
                        (
                            args.session_id.to_string(),
                            "mode".to_owned(),
                            args.mode_id.to_string(),
                        )
                    })
                } else {
                    params::<acp::SetSessionConfigOptionRequest>(request.params).and_then(|args| {
                        let acp::SessionConfigOptionValue::ValueId { value } = args.value else {
                            return Err(Error::invalid_params());
                        };
                        Ok((
                            args.session_id.to_string(),
                            args.config_id.to_string(),
                            value.to_string(),
                        ))
                    })
                };
                let (id, key, value) = match parsed {
                    Ok(args) => args,
                    Err(error) => return reply.respond_with_error(error),
                };
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return reply.respond_with_error(Error::resource_not_found(Some(id)));
                };
                if session.configuring || session.removing || session.stopping {
                    return reply.respond_with_error(busy());
                }
                session.configuring = true;
                let config = session.config.clone();
                let revision = session.config_revision;
                self.spawn(async move {
                    let result = config.clone().set(&rpc, &id, &key, &value).await;
                    let refreshed = if let Err(error) = &result {
                        match error
                            .data
                            .as_ref()
                            .and_then(|data| data["nativeAppError"].as_str())
                        {
                            Some("permission_conflict") => {
                                let current = rpc
                                    .typed(
                                        "session.permission.get",
                                        json!({"sessionId":id,"branchId":"main"}),
                                    )
                                    .await;
                                current.ok().map(|policy| {
                                    let mut config = config;
                                    config.policy = Some(policy);
                                    config
                                })
                            }
                            Some("configuration_superseded") => {
                                read_history(&rpc, &id, false, None)
                                    .await
                                    .ok()
                                    .map(|prepared| prepared.config)
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    Work::Configured {
                        id,
                        reply,
                        mode,
                        revision,
                        result,
                        refreshed,
                    }
                });
            }
            "session/list" if self.rpc.supports("session.list") => {
                let request: acp::ListSessionsRequest = match params(request.params) {
                    Ok(value) => value,
                    Err(error) => return reply.respond_with_error(error),
                };
                if request
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| !cursor.is_empty())
                {
                    return reply.respond_with_error(Error::invalid_params());
                }
                let agent = self.agent_id.clone();
                self.spawn(async move {
                    let result = async {
                        let mut args = json!({});
                        if let Some(agent) = agent {
                            args["agentId"] = json!(agent);
                        }
                        let method = if rpc.supports("session.summaries") {
                            "session.summaries"
                        } else {
                            "session.list"
                        };
                        let mut sessions: Vec<SessionInfo> = rpc.typed(method, args).await?;
                        if let Some(cwd) = request.cwd {
                            sessions.retain(|session| Path::new(&session.cwd) == cwd);
                        }
                        Ok(sessions)
                    }
                    .await;
                    Work::Listed { reply, result }
                });
            }
            _ => return reply.respond_with_error(Error::method_not_found()),
        }
        Ok(())
    }

    fn cancel(&mut self, notification: UntypedMessage) -> Result<(), Error> {
        if notification.method != "session/cancel" {
            return Ok(());
        }
        let Ok(request) = params::<acp::CancelNotification>(notification.params) else {
            return Ok(());
        };
        let id = request.session_id.to_string();
        let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
            return Ok(());
        };
        if session.removing || session.stopping {
            return Ok(());
        }
        if let Some(prompt) = &mut session.prompt {
            prompt.cancelled = true;
            prompt.uploading.cancel();
            if prompt.cancelling || prompt.input.is_none() {
                return Ok(());
            }
        }
        if self.recovering || !self.rpc.ready() {
            return Ok(());
        }
        self.cancel_input(id);
        Ok(())
    }

    fn cancel_input(&mut self, id: String) {
        let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
            return;
        };
        session.stopping = true;
        let (submission, input) = match &mut session.prompt {
            Some(prompt) => {
                prompt.cancelling = true;
                (Some(prompt.submission.clone()), prompt.input.clone())
            }
            None => (None, None),
        };
        let withdraw = input.is_some();
        let rpc = self.rpc.clone();
        self.spawn(async move {
            let result = async {
                if withdraw {
                    let value = rpc
                        .call(
                            "session.input.withdraw",
                            json!({"sessionId":id,"branchId":"main","inputId":input}),
                        )
                        .await?;
                    if value["status"] == "deleted" {
                        return Ok(value);
                    }
                }
                rpc.call("session.stop", json!({"sessionId":id,"branchId":"main"}))
                    .await
            }
            .await;
            Work::Cancelled {
                id,
                submission,
                result,
            }
        });
    }

    async fn work(&mut self, generation: u64, work: Work) -> Result<(), Error> {
        if generation != self.rpc.generation() {
            match work {
                Work::Submitted { .. } => {}
                Work::Created { reply, result, .. } => {
                    self.creating -= 1;
                    let mut error = rpc::unavailable();
                    if let Ok(created) = result {
                        error = error.data(
                            json!({"code":"daemon_unavailable","sessionId":created.session_id}),
                        );
                    }
                    return reply.respond_with_error(error);
                }
                Work::Initialized { reply, .. } => {
                    self.initializing = false;
                    return reply.respond_with_error(rpc::unavailable());
                }
                Work::Authenticated {
                    reply, provider, ..
                } => {
                    self.authenticating.remove(&provider);
                    return reply.respond_with_error(rpc::unavailable());
                }
                Work::Listed { reply, .. }
                | Work::Removed { reply, .. }
                | Work::Configured { reply, .. } => {
                    return reply.respond_with_error(rpc::unavailable());
                }
                _ => return Ok(()),
            }
        }
        match work {
            Work::Initialized { reply, result } => {
                self.initializing = false;
                match result {
                    Ok(methods) => {
                        self.auth_methods = methods;
                        self.initialized = true;
                        let mut capabilities = json!({"loadSession":true,"promptCapabilities":{"image":true,"embeddedContext":true},"sessionCapabilities":{"resume":{},"list":{},"close":{}},"_meta":{"lody":crate::lody::capabilities(self.rpc.supports("session.fork"))}});
                        if self.mcp.supported() {
                            capabilities["mcpCapabilities"] = json!({"http":true,"sse":true});
                        }
                        if self.rpc.supports("session.fork") {
                            capabilities["sessionCapabilities"]["fork"] = json!({});
                        }
                        if self.rpc.supports("session.delete") {
                            capabilities["sessionCapabilities"]["delete"] = json!({});
                        }
                        reply.respond(json!({"protocolVersion":1,"agentInfo":{"name":"dimcode","version":env!("CARGO_PKG_VERSION")},"authMethods":self.auth_methods.iter().map(|method|method.value()).collect::<Vec<_>>(),"agentCapabilities":capabilities}))?;
                    }
                    Err(error) => reply.respond_with_error(error)?,
                }
            }
            Work::Authenticated {
                reply,
                provider,
                result,
            } => {
                self.authenticating.remove(&provider);
                reply.respond_with_result(result)?;
            }
            Work::Created {
                reply,
                cwd,
                servers,
                result,
            } => {
                self.creating -= 1;
                match result {
                    Ok(created) => {
                        let added = match self.mcp.plan(&created.session_id, servers) {
                            Ok(added) => added,
                            Err(error) => return reply.respond_with_error(error),
                        };
                        self.prepare(
                            created.session_id,
                            cwd,
                            Waiter {
                                reply,
                                kind: Preparation::New,
                            },
                            true,
                            added,
                        )
                    }
                    Err(error) => reply.respond_with_error(error)?,
                }
            }
            Work::Prepared {
                id,
                preparation,
                result,
            } => {
                if !matches!(self.sessions.get(&id),Some(Slot::Loading(loading)) if loading.preparation == preparation)
                {
                    return Ok(());
                }
                let Some(Slot::Loading(mut loading)) = self.sessions.remove(&id) else {
                    return Ok(());
                };
                let result = if loading.waiters.iter().all(|waiter| {
                    !matches!(waiter.kind, Preparation::New)
                        && waiter.reply.cancellation().is_cancelled()
                }) {
                    Err(Error::request_cancelled())
                } else {
                    result
                };
                match result {
                    Ok(prepared) => {
                        let replay = loading.waiters.iter().any(|waiter| {
                            matches!(waiter.kind, Preparation::Load | Preparation::History)
                                && !waiter.reply.cancellation().is_cancelled()
                        });
                        let mut session = Session {
                            cwd: loading.cwd,
                            projection: Projection::default(),
                            prompt: None,
                            config: prepared.config,
                            configuring: false,
                            config_revision: 0,
                            questions: dim_client::question::Pending::default(),
                            usage_pending: false,
                            title_revision: None,
                            stopping: false,
                            removing: false,
                            pending_remove: None,
                            recovery: None,
                            subagents: loading
                                .previous
                                .as_mut()
                                .map(|previous| std::mem::take(&mut previous.subagents))
                                .unwrap_or_default(),
                        };
                        for (index, event) in prepared.history.into_iter().enumerate() {
                            if index % 32 == 0 {
                                tokio::task::yield_now().await;
                            }
                            fold_questions(&mut session.questions, "main", &event);
                            let updates = session.projection.event(&event);
                            let snapshots = session.subagents.event(&id, &event);
                            if self.subagent_events {
                                for snapshot in snapshots {
                                    self.outbox.notify(
                                        &self.cx,
                                        "_lody/subagents/event",
                                        snapshot,
                                    )?;
                                }
                            }
                            if replay
                                && loading
                                    .waiters
                                    .iter()
                                    .any(|waiter| !waiter.reply.cancellation().is_cancelled())
                            {
                                for update in updates {
                                    self.update(&id, update)?;
                                }
                            }
                        }
                        session.projection.through = prepared.through;
                        let state = session.config.response();
                        self.sessions
                            .insert(id.clone(), Slot::Ready(Box::new(session)));
                        if let Some(metadata) = prepared.metadata
                            && let Some(revision) = metadata.title_revision
                        {
                            self.title(&id, metadata.title, revision)?;
                        }
                        for waiter in loading.waiters {
                            if !matches!(waiter.kind, Preparation::New)
                                && waiter.reply.cancellation().is_cancelled()
                            {
                                waiter
                                    .reply
                                    .respond_with_error(Error::request_cancelled())?;
                                continue;
                            }
                            let mut response = if matches!(waiter.kind, Preparation::History) {
                                json!({})
                            } else {
                                state.clone()
                            };
                            if matches!(waiter.kind, Preparation::New) {
                                response["sessionId"] = json!(id);
                            }
                            waiter.reply.respond(response)?;
                        }
                        for (method, params) in loading.buffered {
                            self.notice(method, params)?;
                        }
                        self.questions(&id);
                        self.usage(&id);
                    }
                    Err(mut error) => {
                        let buffered = loading.buffered;
                        if loading.mcp_added {
                            let released = self.mcp.release(&id);
                            self.tasks.spawn(released);
                            if let Some(Value::Object(data)) = &mut error.data {
                                data.insert("sessionId".into(), json!(id));
                            } else {
                                error.data = Some(json!({"sessionId":id}));
                            }
                        }
                        if let Some(previous) = loading.previous {
                            self.sessions.insert(id.clone(), Slot::Ready(previous));
                        } else {
                            let rpc = self.rpc.clone();
                            let detached = id.clone();
                            self.tasks.spawn(async move {
                                let _ = rpc
                                    .call("session.detach", json!({"sessionId":detached}))
                                    .await;
                            });
                        }
                        for waiter in loading.waiters {
                            waiter.reply.respond_with_error(error.clone())?;
                        }
                        for (method, params) in buffered {
                            self.notice(method, params)?;
                        }
                    }
                }
            }
            Work::Submitted {
                id,
                submission,
                result,
                sent,
            } => {
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return Ok(());
                };
                if session
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.submission == submission)
                {
                    match result {
                        Ok(result) => {
                            let prompt = session.prompt.as_mut().unwrap();
                            if prompt.turn.is_none()
                                && !prompt.consumed.contains_key(&result.event.id)
                                && !prompt.deleted.contains(&result.event.id)
                            {
                                session
                                    .projection
                                    .silent_inputs
                                    .insert(result.event.id.clone());
                            }
                            if let Some(finish) = prompt.accepted(result.event.id) {
                                if prompt.cancelling {
                                    prompt.terminal = Some(finish);
                                } else {
                                    session
                                        .prompt
                                        .take()
                                        .unwrap()
                                        .reply
                                        .respond_with_result(finish)?;
                                }
                            }
                            if !self.recovering
                                && self.rpc.ready()
                                && session
                                    .prompt
                                    .as_ref()
                                    .is_some_and(|prompt| prompt.cancelled && !prompt.cancelling)
                            {
                                self.cancel_input(id);
                            }
                        }
                        Err(error) => {
                            if sent
                                && error
                                    .data
                                    .as_ref()
                                    .is_some_and(|data| data["code"] == "daemon_unavailable")
                            {
                                return Ok(());
                            }
                            let prompt = session.prompt.take().unwrap();
                            let rejected = !sent
                                || error.data.as_ref().is_some_and(|data| {
                                    data["code"] == "request_too_large"
                                        || matches!(
                                            data["nativeAppError"].as_str(),
                                            Some(
                                                "invalid_params"
                                                    | "session_invalid_input"
                                                    | "session_closed"
                                                    | "session_build_failed"
                                                    | "service_reloading"
                                                    | "unknown_method"
                                            )
                                        )
                                });
                            if prompt.cancelled {
                                prompt.reply.respond(json!({"stopReason":"cancelled"}))?;
                            } else if !rejected {
                                prompt.reply.respond_with_error(Error::new(-32001,"Submission outcome is unknown; load this Session before sending again").data(json!({"code":"submission_unknown","sessionId":id,"submissionId":prompt.submission})))?;
                            } else {
                                prompt.reply.respond_with_error(error)?;
                            }
                        }
                    }
                }
            }
            Work::Cancelled {
                id,
                submission,
                result,
            } => {
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return Ok(());
                };
                session.stopping = false;
                if session
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| submission.as_ref() == Some(&prompt.submission))
                {
                    let prompt = session.prompt.as_mut().unwrap();
                    prompt.cancelling = false;
                    if prompt.turn.is_none()
                        || prompt.terminal.is_some()
                        || result.is_err()
                        || result
                            .as_ref()
                            .is_ok_and(|result| result["status"] == "deleted")
                    {
                        let prompt = session.prompt.take().unwrap();
                        let mut value = json!({"stopReason":"cancelled"});
                        if result.is_err() {
                            value["_meta"] = json!({"dim":{"nativeCancellation":"unconfirmed"}});
                        }
                        prompt.reply.respond(value)?;
                    }
                }
                self.remove(&id);
            }
            Work::Removed { id, reply, result } => match result {
                Ok(_) => {
                    self.mcp.forget(&id);
                    self.release_interactions(&id);
                    if let Some(Slot::Ready(mut session)) = self.sessions.remove(&id)
                        && let Some(prompt) = session.prompt.take()
                    {
                        prompt.reply.respond(json!({"stopReason":"cancelled"}))?;
                    }
                    reply.respond(json!({}))?;
                }
                Err(error) => {
                    if let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) {
                        session.removing = false;
                    }
                    reply.respond_with_error(error)?;
                }
            },
            Work::Configured {
                id,
                reply,
                mode,
                revision,
                result,
                refreshed,
            } => {
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return reply.respond_with_error(Error::resource_not_found(Some(id)));
                };
                session.configuring = false;
                match result {
                    Ok(mut config) => {
                        if revision != session.config_revision {
                            config.model.clone_from(&session.config.model);
                            // A fresh version is still read before the next policy mutation.
                            if let (Some(policy), Some(current)) =
                                (&mut config.policy, &session.config.policy)
                            {
                                policy.policy = current.policy.clone();
                            }
                        }
                        session.config = config;
                        let options = session.config.options();
                        let current_mode = session
                            .config
                            .policy
                            .as_ref()
                            .map(|policy| policy.policy.preset);
                        reply.respond(if mode {
                            json!({})
                        } else {
                            json!({"configOptions":options})
                        })?;
                        self.update(
                            &id,
                            json!({"sessionUpdate":"config_option_update","configOptions":options}),
                        )?;
                        if mode && let Some(mode) = current_mode {
                            self.update(
                                &id,
                                json!({"sessionUpdate":"current_mode_update","currentModeId":mode}),
                            )?;
                        }
                    }
                    Err(error) => {
                        if let Some(mut config) = refreshed {
                            if revision != session.config_revision {
                                config.model.clone_from(&session.config.model);
                                if let (Some(policy), Some(current)) =
                                    (&mut config.policy, &session.config.policy)
                                {
                                    policy.policy = current.policy.clone();
                                }
                            }
                            session.config = config;
                            let options = session.config.options();
                            reply.respond_with_error(error)?;
                            self.update(&id,json!({"sessionUpdate":"config_option_update","configOptions":options}))?;
                        } else {
                            reply.respond_with_error(error)?;
                        }
                    }
                }
            }
            Work::Answer { key, method, args } => {
                if let Some((_, token)) = self.interactions.get(&key)
                    && !token.is_cancelled()
                    && let Some(args) = args
                {
                    token.cancel();
                    let rpc = self.rpc.clone();
                    self.tasks.spawn(async move {
                        let _ = rpc.call(method, args).await;
                    });
                }
            }
            Work::Usage { id, result } => {
                if let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) {
                    session.usage_pending = false;
                }
                if let Ok(value) = result
                    && self.sessions.contains_key(&id)
                {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Usage {
                        estimated_input_tokens: f64,
                        context_window: f64,
                    }
                    let usage: Usage = decode_native(value)?;
                    if usage.estimated_input_tokens.is_finite()
                        && usage.estimated_input_tokens >= 0.0
                        && usage.context_window.is_finite()
                        && usage.context_window > 0.0
                    {
                        self.update(&id,json!({"sessionUpdate":"usage_update","used":usage.estimated_input_tokens.ceil() as u64,"size":usage.context_window.floor() as u64}))?;
                    }
                }
            }
            Work::Recovered { id, result } => {
                if let Err(error) = &*result
                    && error
                        .data
                        .as_ref()
                        .is_some_and(|data| data["code"] == "daemon_unavailable")
                {
                    return self.fail_connection().await;
                }
                self.recovering_sessions -= 1;
                if self.recovering_sessions == 0 {
                    self.recovering = false;
                }
                let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) else {
                    return Ok(());
                };
                let buffered = session.recovery.take().unwrap_or_default();
                match *result {
                    Ok((prepared, input)) => {
                        let metadata = prepared.metadata;
                        session.config = prepared.config;
                        if let Some(prompt) = &mut session.prompt
                            && let Some(input) = input
                        {
                            if prompt.turn.is_none()
                                && !prompt.consumed.contains_key(&input.id)
                                && !prompt.deleted.contains(&input.id)
                            {
                                session.projection.silent_inputs.insert(input.id.clone());
                            }
                            if let Some(finish) = prompt.accepted(input.id.clone()) {
                                prompt.terminal = Some(finish);
                            }
                        }
                        let mut snapshots = Vec::new();
                        for event in &prepared.history {
                            snapshots.extend(session.subagents.event(&id, event));
                        }
                        if self.subagent_events {
                            for snapshot in snapshots {
                                self.outbox
                                    .notify(&self.cx, "_lody/subagents/event", snapshot)?;
                            }
                        }
                        for event in prepared.history {
                            self.event_notice(EventNotice {
                                session_id: id.clone(),
                                branch_id: "main".into(),
                                event,
                            })?;
                        }
                        if let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) {
                            session.projection.through = prepared.through;
                            if session.prompt.as_ref().is_some_and(|prompt| {
                                prompt.terminal.is_some() && !prompt.cancelling
                            }) {
                                let mut prompt = session.prompt.take().unwrap();
                                prompt
                                    .reply
                                    .respond_with_result(prompt.terminal.take().unwrap())?;
                            } else if session
                                .prompt
                                .as_ref()
                                .is_some_and(|prompt| prompt.input.is_none())
                            {
                                let prompt = session.prompt.take().unwrap();
                                if prompt.cancelled {
                                    prompt.reply.respond(json!({"stopReason":"cancelled","_meta":{"dim":{"nativeCancellation":"unconfirmed"}}}))?;
                                } else {
                                    prompt.reply.respond_with_error(Error::new(-32001,"Submission outcome is unknown; load this Session before sending again").data(json!({"code":"submission_unknown","sessionId":id,"submissionId":prompt.submission})))?;
                                }
                            } else if session
                                .prompt
                                .as_ref()
                                .is_some_and(|prompt| prompt.cancel_uncertain)
                            {
                                let prompt = session.prompt.take().unwrap();
                                prompt.reply.respond(json!({"stopReason":"cancelled","_meta":{"dim":{"nativeCancellation":"unconfirmed"}}}))?;
                            }
                        }
                        if let Some(metadata) = metadata
                            && let Some(revision) = metadata.title_revision
                        {
                            self.title(&id, metadata.title, revision)?;
                        }
                    }
                    Err(error) => {
                        if let Some(prompt) = session.prompt.take() {
                            if prompt.cancelled {
                                prompt.reply.respond(json!({"stopReason":"cancelled","_meta":{"dim":{"nativeCancellation":"unconfirmed"}}}))?;
                            } else {
                                prompt.reply.respond_with_error(error)?;
                            }
                        }
                        self.sessions.remove(&id);
                        self.release_interactions(&id);
                    }
                }
                for (method, params) in buffered {
                    self.notice(method, params)?;
                }
                if self.recovering_sessions == 0 {
                    self.recovering = false;
                    let cancelled: Vec<_> = self
                        .sessions
                        .iter()
                        .filter_map(|(id, slot)| match slot {
                            Slot::Ready(session)
                                if session.prompt.as_ref().is_some_and(|prompt| {
                                    prompt.cancelled && !prompt.cancel_uncertain
                                }) =>
                            {
                                Some(id.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    for id in cancelled {
                        self.cancel_input(id);
                    }
                }
            }
            Work::Listed { reply, result } => match result {
                Ok(sessions) => {
                    let sessions: Vec<Value> = sessions
                        .into_iter()
                        .map(|session| {
                            let mut row = json!({"sessionId":session.session_id,"cwd":session.cwd});
                            if let Some(title) = session.title {
                                row["title"] = json!(title);
                            }
                            if let Some(timestamp) = session.updated_at {
                                let date = time::OffsetDateTime::from_unix_timestamp_nanos(
                                    i128::from(timestamp) * 1_000_000,
                                )
                                .map_err(|_| Error::internal_error())?;
                                row["updatedAt"] = json!(
                                    date.format(&time::format_description::well_known::Rfc3339)
                                        .map_err(|_| Error::internal_error())?
                                );
                            }
                            Ok(row)
                        })
                        .collect::<Result<_, Error>>()?;
                    reply.respond(json!({"sessions":sessions}))?;
                }
                Err(error) => reply.respond_with_error(error)?,
            },
        }
        Ok(())
    }

    fn notice(&mut self, method: String, params: Value) -> Result<(), Error> {
        if method == "session.closed" {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Closed {
                session_id: String,
                reason: Option<String>,
            }
            let closed: Closed = decode_native(params)?;
            self.release_interactions(&closed.session_id);
            self.mcp.forget(&closed.session_id);
            match self.sessions.remove(&closed.session_id) {
                Some(Slot::Ready(mut session)) => {
                    if let Some(prompt) = session.prompt.take() {
                        if !prompt.cancelled
                            && let Some(reason) = closed.reason
                        {
                            prompt.reply.respond_with_error(
                                Error::new(-32001, reason).data(json!({"code":"session_closed"})),
                            )?;
                        } else {
                            prompt.reply.respond(json!({"stopReason":"cancelled"}))?;
                        }
                    }
                    if let Some((_, reply)) = session.pending_remove.take() {
                        reply.respond(json!({}))?;
                    }
                }
                Some(Slot::Loading(loading)) => {
                    for waiter in loading.waiters {
                        waiter
                            .reply
                            .respond_with_error(Error::resource_not_found(Some(
                                closed.session_id.clone(),
                            )))?;
                    }
                }
                None => {}
            }
            return Ok(());
        }
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(Slot::Loading(loading)) = id.as_ref().and_then(|id| self.sessions.get_mut(id)) {
            if loading.buffered.len() >= 256 {
                return Err(Error::new(-32001, "Native replay buffer overflow"));
            }
            loading.buffered.push((method, params));
            return Ok(());
        }
        if let Some(Slot::Ready(session)) = id.as_ref().and_then(|id| self.sessions.get_mut(id))
            && let Some(buffered) = &mut session.recovery
        {
            if buffered.len() >= 256 {
                return Err(Error::new(-32001, "Native recovery buffer overflow"));
            }
            buffered.push((method, params));
            return Ok(());
        }
        match method.as_str() {
            "session.event" => self.event_notice(decode_native(params)?)?,
            "session.model.progress" => {
                let progress: Progress = decode_native(params)?;
                if progress.branch_id != "main" {
                    return Ok(());
                }
                let id = progress.session_id.clone();
                if let Some(Slot::Ready(session)) = self.sessions.get_mut(&id) {
                    let updates = session.projection.progress(progress);
                    for update in updates {
                        self.update(&id, update)?;
                    }
                }
            }
            "approval.request" => {
                let approval: crate::interaction::Approval = decode_native(params)?;
                if !matches!(
                    self.sessions.get(&approval.session_id),
                    Some(Slot::Ready(_))
                ) {
                    return Ok(());
                }
                let key = crate::interaction::key(
                    "approval",
                    &approval.session_id,
                    &approval.branch_id,
                    &approval.request_id,
                    "",
                );
                if self.interactions.contains_key(&key) {
                    return Ok(());
                }
                let token = self.stop.child_token();
                self.interactions
                    .insert(key.clone(), (approval.session_id.clone(), token.clone()));
                let cx = self.cx.clone();
                let outbox = self.outbox.clone();
                self.spawn(async move {
                    Work::Answer {
                        key,
                        method: "approval.respond",
                        args: crate::interaction::approval(cx, outbox, token, approval).await,
                    }
                });
            }
            "approval.resolved" | "approval.cancelled" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Settled {
                    session_id: String,
                    branch_id: String,
                    request_id: String,
                }
                let settled: Settled = decode_native(params)?;
                let key = crate::interaction::key(
                    "approval",
                    &settled.session_id,
                    &settled.branch_id,
                    &settled.request_id,
                    "",
                );
                if let Some((_, token)) = self.interactions.remove(&key) {
                    token.cancel();
                }
            }
            "session.notice" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Notice {
                    session_id: String,
                    source: String,
                    kind: String,
                    data: Value,
                }
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Title {
                    title: Option<String>,
                    title_revision: u64,
                }
                let notice: Notice = decode_native(params)?;
                if notice.source == "SessionStore" && notice.kind == "title" {
                    let title: Title = decode_native(notice.data)?;
                    self.title(&notice.session_id, title.title, title.title_revision)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn fail_connection(&mut self) -> Result<(), Error> {
        self.recovering = true;
        for (_, (_, token)) in self.interactions.drain() {
            token.cancel();
        }
        for (id, slot) in &mut self.sessions {
            match slot {
                Slot::Ready(session) => {
                    for snapshot in session.subagents.disconnected(id) {
                        if self.subagent_events {
                            self.outbox
                                .notify(&self.cx, "_lody/subagents/event", snapshot)?;
                        }
                    }
                    if let Some(prompt) = session.prompt.take() {
                        if prompt.cancelled {
                            prompt.reply.respond(json!({"stopReason":"cancelled","_meta":{"dim":{"nativeCancellation":"unconfirmed"}}}))?;
                        } else {
                            prompt.reply.respond_with_error(rpc::unavailable())?;
                        }
                    }
                }
                Slot::Loading(loading) => {
                    if loading.mcp_added {
                        self.mcp.forget(id);
                    }
                    for waiter in std::mem::take(&mut loading.waiters) {
                        waiter.reply.respond_with_error(rpc::unavailable())?;
                    }
                }
            }
        }
        // The SDK drops queued replies when its foreground task returns an error.
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.outbox.drained()).await;
        Err(rpc::unavailable())
    }

    fn title(&mut self, id: &str, title: Option<String>, revision: u64) -> Result<(), Error> {
        if let Some(Slot::Ready(session)) = self.sessions.get_mut(id)
            && session.title_revision.is_none_or(|old| revision > old)
        {
            session.title_revision = Some(revision);
            self.update(
                id,
                json!({"sessionUpdate":"session_info_update","title":title}),
            )?;
        }
        Ok(())
    }

    fn release_interactions(&mut self, id: &str) {
        self.interactions.retain(|_, (session, token)| {
            if session == id {
                token.cancel();
                false
            } else {
                true
            }
        });
    }

    fn lost(&mut self) -> Result<(), Error> {
        self.recovering = true;
        self.recovering_sessions = 0;
        for token in self.interactions.drain().map(|(_, (_, token))| token) {
            token.cancel();
        }
        let slots = std::mem::take(&mut self.sessions);
        for (id, slot) in slots {
            let mut session = match slot {
                Slot::Ready(session) => session,
                Slot::Loading(loading) => {
                    if loading.mcp_added {
                        self.mcp.forget(&id);
                    }
                    for waiter in loading.waiters {
                        waiter.reply.respond_with_error(rpc::unavailable())?;
                    }
                    let Some(previous) = loading.previous else {
                        self.mcp.forget(&id);
                        continue;
                    };
                    previous
                }
            };
            if let Some(prompt) = &mut session.prompt {
                prompt.cancel_uncertain |= prompt.cancelling;
                prompt.cancelling = false;
            }
            if let Some((_, reply)) = session.pending_remove.take() {
                reply.respond_with_error(rpc::unavailable())?;
            }
            session.stopping = false;
            session.removing = false;
            session.configuring = false;
            session.usage_pending = false;
            session.recovery = Some(Vec::new());
            for snapshot in session.subagents.disconnected(&id) {
                if self.subagent_events {
                    self.outbox
                        .notify(&self.cx, "_lody/subagents/event", snapshot)?;
                }
            }
            self.sessions.insert(id, Slot::Ready(session));
        }
        Ok(())
    }

    fn restore(&mut self, same_instance: bool, generation: u64, deadline: tokio::time::Instant) {
        if !same_instance {
            eprintln!("ACP reconnected to a new App Server instance; restoring persisted sessions");
        }
        let sessions: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Ready(session) => Some((
                    id.clone(),
                    session.cwd.clone(),
                    session
                        .prompt
                        .as_ref()
                        .filter(|prompt| prompt.input.is_none())
                        .map(|prompt| prompt.submission.clone()),
                )),
                _ => None,
            })
            .collect();
        self.recovering_sessions = sessions.len();
        if sessions.is_empty() {
            self.recovering = false;
        }
        for (id, cwd, submission) in sessions {
            let rpc = self.rpc.clone();
            let mcp = self.mcp.clone();
            self.spawn_generation(generation,async move {
                let result = tokio::time::timeout_at(deadline,async {
                    let mut input = None;
                    if let Some(submission) = submission {
                        #[derive(Deserialize)] #[serde(tag="status",rename_all="lowercase")]
                        enum Lookup { Accepted { event: Box<Event> },Unknown }
                        loop {
                            match rpc.typed("session.input_lookup",json!({"sessionId":id,"branchId":"main","submissionId":submission})).await? {
                                Lookup::Accepted { event }=>{ input = Some(*event); break; }
                                Lookup::Unknown if tokio::time::Instant::now()+std::time::Duration::from_millis(250) < deadline=>tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                                Lookup::Unknown=>break,
                            }
                        }
                    }
                    let prepared = read_history(&rpc,&id,true,Some(cwd)).await?;
                    mcp.ensure(&id).await?;
                    Ok((prepared,input))
                }).await.unwrap_or_else(|_|Err(rpc::unavailable()));
                Work::Recovered { id,result: Box::new(result) }
            });
        }
    }

    fn event_notice(&mut self, notice: EventNotice) -> Result<(), Error> {
        if let Some(Slot::Ready(session)) = self.sessions.get_mut(&notice.session_id) {
            for settled in fold_questions(&mut session.questions, &notice.branch_id, &notice.event)
            {
                let call = settled.call;
                let key = crate::interaction::key(
                    "question",
                    &notice.session_id,
                    &call.branch_id,
                    &call.request_id,
                    &call.call_id,
                );
                if let Some((_, token)) = self.interactions.remove(&key) {
                    token.cancel();
                }
            }
            self.questions(&notice.session_id);
        }
        if notice.branch_id != "main" {
            return Ok(());
        }
        let Some(Slot::Ready(session)) = self.sessions.get_mut(&notice.session_id) else {
            return Ok(());
        };
        if notice.event.sequence <= session.projection.through {
            return Ok(());
        }
        let mut finish = None;
        if let Some(prompt) = &mut session.prompt {
            match &notice.event.content {
                EventContent::Input(data)
                    if data.submission_id.as_deref() == Some(&prompt.submission) =>
                {
                    finish = prompt.accepted(notice.event.id.clone());
                    session
                        .projection
                        .silent_inputs
                        .insert(notice.event.id.clone());
                }
                EventContent::InputConsumed(data)
                    if prompt.input.as_deref() == Some(&data.input_id) =>
                {
                    prompt.turn.clone_from(&session.projection.turn)
                }
                EventContent::InputConsumed(data) if prompt.input.is_none() => {
                    if let Some(turn) = &session.projection.turn {
                        prompt.consumed.insert(data.input_id.clone(), turn.clone());
                    }
                }
                EventContent::InputDeleted(data)
                    if prompt.input.as_deref() == Some(&data.input_id) =>
                {
                    finish = Some(Ok(json!({"stopReason":"cancelled"})))
                }
                EventContent::InputDeleted(data) if prompt.input.is_none() => {
                    prompt.deleted.insert(data.input_id.clone());
                }
                EventContent::ModelEnd(data)
                    if prompt.input.is_none() || prompt.turn.as_deref() == Some(&data.turn_id) =>
                {
                    if matches!(&data.outcome, dim_agent_types::ModelOutputOutcome::Failed { presentation: Some(presentation), .. } if presentation.reason == "max_tokens")
                    {
                        prompt.max_tokens.insert(data.turn_id.clone());
                    }
                }
                EventContent::TurnEnd(data) if prompt.turn.as_deref() == Some(&data.turn_id) => {
                    finish = Some(prompt_outcome(
                        prompt.cancelled,
                        prompt.max_tokens.contains(&data.turn_id),
                        &data.outcome,
                    ));
                }
                EventContent::TurnEnd(data) if prompt.input.is_none() => {
                    prompt
                        .ended
                        .insert(data.turn_id.clone(), data.outcome.clone());
                }
                _ => {}
            }
        }
        let updates = session.projection.event(&notice.event);
        let snapshots = session.subagents.event(&notice.session_id, &notice.event);
        if matches!(
            &notice.event.content,
            EventContent::Configuration(_) | EventContent::PermissionChanged(_)
        ) {
            session.config.event(&notice.event);
            session.config_revision += 1;
        }
        let config_update = matches!(
            &notice.event.content,
            EventContent::Configuration(_) | EventContent::PermissionChanged(_)
        )
        .then(|| session.config.options());
        let mode_update = match &notice.event.content {
            EventContent::PermissionChanged(data) => Some(data.policy.preset),
            _ => None,
        };
        if finish.is_some()
            && session
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.cancelling)
        {
            session.prompt.as_mut().unwrap().terminal = finish.take();
        }
        let completed =
            finish.and_then(|result| session.prompt.take().map(|prompt| (prompt.reply, result)));
        for update in updates {
            self.update(&notice.session_id, update)?;
        }
        if self.subagent_events {
            for snapshot in snapshots {
                self.outbox
                    .notify(&self.cx, "_lody/subagents/event", snapshot)?;
            }
        }
        if let Some(options) = config_update {
            self.update(
                &notice.session_id,
                json!({"sessionUpdate":"config_option_update","configOptions":options}),
            )?;
        }
        if let Some(mode) = mode_update {
            self.update(
                &notice.session_id,
                json!({"sessionUpdate":"current_mode_update","currentModeId":mode}),
            )?;
        }
        if let Some((reply, result)) = completed {
            reply.respond_with_result(result)?;
        }
        if matches!(
            &notice.event.content,
            EventContent::TurnEnd(_) | EventContent::Configuration(_)
        ) {
            self.usage(&notice.session_id);
        }
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        let Some(Slot::Ready(session)) = self.sessions.get_mut(id) else {
            return;
        };
        if session.stopping {
            return;
        }
        let Some((method, reply)) = session.pending_remove.take() else {
            return;
        };
        let id = id.to_owned();
        let rpc = self.rpc.clone();
        self.spawn(async move {
            Work::Removed {
                result: rpc.call(method, json!({"sessionId":id})).await,
                id,
                reply,
            }
        });
    }

    fn questions(&mut self, id: &str) {
        let Some(Slot::Ready(session)) = self.sessions.get(id) else {
            return;
        };
        if !self.form || !session.config.methods.contains("session.question.respond") {
            return;
        }
        let calls: Vec<_> = session.questions.calls().cloned().collect();
        for call in calls {
            let key = crate::interaction::key(
                "question",
                id,
                &call.branch_id,
                &call.request_id,
                &call.call_id,
            );
            if self.interactions.contains_key(&key) {
                continue;
            }
            let token = self.stop.child_token();
            self.interactions
                .insert(key.clone(), (id.to_owned(), token.clone()));
            let cx = self.cx.clone();
            let outbox = self.outbox.clone();
            let id = id.to_owned();
            self.spawn(async move {
                Work::Answer {
                    key,
                    method: "session.question.respond",
                    args: crate::interaction::question(cx, outbox, token, &id, &call).await,
                }
            });
        }
    }

    fn usage(&mut self, id: &str) {
        let Some(Slot::Ready(session)) = self.sessions.get_mut(id) else {
            return;
        };
        if session.usage_pending || !session.config.methods.contains("session.context.usage") {
            return;
        }
        session.usage_pending = true;
        let id = id.to_owned();
        let rpc = self.rpc.clone();
        self.spawn(async move {
            Work::Usage {
                result: rpc
                    .call(
                        "session.context.usage",
                        json!({"sessionId":id,"branchId":"main"}),
                    )
                    .await,
                id,
            }
        });
    }

    pub(crate) async fn run(
        mut self,
        mut incoming: mpsc::Receiver<Incoming>,
        mut native: mpsc::Receiver<Native>,
    ) -> Result<(), Error> {
        loop {
            tokio::select! {
                () = self.cx.incoming_closed() => return Ok(()),
                () = self.stop.cancelled() => return Ok(()),
                message = incoming.recv() => match message {
                    Some(Incoming::Request(request, reply)) => self.request(request, *reply)?,
                    Some(Incoming::Notification(notification)) => self.cancel(notification)?,
                    Some(Incoming::Work(generation,work)) => self.work(generation,*work).await?,
                    None => return Ok(()),
                },
                message = native.recv() => match message {
                    Some(Native::Notice { method, params }) => self.notice(method, params)?,
                    Some(Native::Lost) => self.lost()?,
                    Some(Native::Restored { same_instance,generation,deadline }) if generation == self.rpc.generation() && self.rpc.ready() => self.restore(same_instance,generation,deadline),
                    Some(Native::Restored { .. })=>{},
                    Some(Native::Closed) | None => return self.fail_connection().await,
                },
            }
        }
    }
}

fn fold_questions(
    pending: &mut dim_client::question::Pending,
    branch: &str,
    event: &Event,
) -> Vec<dim_client::question::Settled> {
    match &event.content {
        EventContent::ToolCall(data) => {
            pending.asked(branch, &event.id, data);
            Vec::new()
        }
        EventContent::ToolResult(data) => pending.answered(branch, data),
        _ => Vec::new(),
    }
}

fn decode_native<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|error| {
        Error::internal_error()
            .data(json!({"code":"invalid_native_notification","detail":error.to_string()}))
    })
}

async fn read_history(
    rpc: &Rpc,
    id: &str,
    attach: bool,
    expected_cwd: Option<String>,
) -> Result<Prepared, Error> {
    let metadata = if let Some(cwd) = expected_cwd {
        let method = if rpc.supports("session.summaries") {
            "session.summaries"
        } else {
            "session.list"
        };
        let sessions: Vec<SessionInfo> = rpc.typed(method, json!({})).await?;
        let session = sessions
            .into_iter()
            .find(|session| session.session_id == id)
            .ok_or_else(|| Error::resource_not_found(Some(id.to_owned())))?;
        let requested = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|_| Error::invalid_params())?;
        if Path::new(&session.cwd) != requested {
            return Err(Error::invalid_params());
        }
        Some(session)
    } else {
        None
    };
    if attach {
        rpc.call("session.attach", json!({"sessionId":id})).await?;
    }
    let mut history = Vec::new();
    let mut after = 0;
    let mut through = None;
    loop {
        let mut args = json!({"sessionId":id,"branchId":"main","after":after,"limit":256});
        if let Some(through) = through {
            args["through"] = json!(through);
        }
        let page: History = rpc.typed("session.history", args).await?;
        let next = page
            .next(after, through)
            .map_err(|_| Error::internal_error().data(json!({"code":"invalid_history_cursor"})))?;
        through = Some(page.through);
        for event in page.events {
            history.push(decode_native(event)?);
        }
        if let Some(next) = next {
            after = next;
        } else {
            let config = Config::read(rpc, id, &history).await?;
            return Ok(Prepared {
                history,
                through: page.through,
                config,
                metadata,
            });
        }
    }
}
