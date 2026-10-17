//! C5's named TreeCommands service and public branch-service references.
use super::TreeCommands;
use dim_agent_runtime::{
    AgentError, Branch, BranchLifecycle, BranchOptions, BranchService, Dispatcher, PublicService,
    SessionScope, Timeline, TimelineIdentity,
};
use dim_agent_types::InputMessageData;
use rutis::{CordisError, Ctx};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, host_key_in, settle};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One declared service from one original branch row, never an aggregate.
pub struct ForeignBranchService {
    dispatch: Arc<dyn HostDispatch>,
    methods: Box<[(String, bool)]>,
    cancelled: CancellationToken,
}
impl BranchService for ForeignBranchService {}
impl PublicService for ForeignBranchService {}
impl ForeignBranchService {
    /// Method names and async modes come from the validated row declaration.
    pub fn new(
        dispatch: Arc<dyn HostDispatch>,
        methods: impl IntoIterator<Item = (String, bool)>,
        cancelled: CancellationToken,
    ) -> Self {
        Self {
            dispatch,
            methods: methods.into_iter().collect(),
            cancelled,
        }
    }

    fn reference(self: &Arc<Self>) -> Value {
        Value::Record(
            self.methods
                .iter()
                .map(|(name, asynchronous)| {
                    let owner = self.clone();
                    let method = name.clone();
                    let asynchronous = *asynchronous;
                    (
                        name.clone(),
                        Value::callback(move |args| {
                            if owner.cancelled.is_cancelled() {
                                return Err(Error::Value("branch service provider ended".into()));
                            }
                            if !asynchronous {
                                return owner.dispatch.invoke(&method, args);
                            }
                            let owner = owner.clone();
                            let method = method.clone();
                            Ok(Value::future(async move {
                                tokio::select! {
                                    biased;
                                    _ = owner.cancelled.cancelled() => Err(Error::Value("branch service provider ended".into())),
                                    result = async { settle(owner.dispatch.invoke(&method, args)?).await } => result,
                                }
                            }))
                        }),
                    )
                })
                .collect(),
        )
    }
}

fn success(value: Value) -> Value {
    Value::Record([("ok".into(), json!(true).into()), ("value".into(), value)].into())
}

fn failure(code: &str, message: impl ToString) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message.to_string()}}).into()
}

fn native_error(error: AgentError) -> Value {
    let code = match &error {
        AgentError::InvalidInput(_) => "invalid_input",
        AgentError::UnknownBranch(_) => "unknown_branch",
        AgentError::Closed(_) => "closed",
        AgentError::Cancelled(_) => "cancelled",
        AgentError::Reloading(_) => "reloading",
        _ => "failed",
    };
    failure(code, error)
}

fn data<T: Serialize>(result: dim_agent_runtime::Result<T>) -> Value {
    match result {
        Ok(value) => success(
            serde_json::to_value(value)
                .expect("native C5 data serializes")
                .into(),
        ),
        Err(error) => native_error(error),
    }
}

fn request<T: DeserializeOwned>(args: Value) -> Result<T, Value> {
    let [argument]: [Value; 1] = args
        .list()
        .map_err(|error| failure("invalid_params", error))?
        .try_into()
        .map_err(|_| failure("invalid_params", "expected one request argument"))?;
    decode(argument)
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, Value> {
    serde_json::from_value(
        value
            .json()
            .map_err(|error| failure("invalid_params", error))?,
    )
    .map_err(|error| failure("invalid_params", error))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchId {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceName {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    parent: String,
    at: u64,
    options: BranchOptions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Through {
    through: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Local {
    start: u64,
    through: u64,
}

/// References owned by the original branch row, including its subscriptions.
pub fn branch_binding(
    ctx: &Ctx,
    part: &str,
    identity: Arc<TimelineIdentity>,
    timeline: Arc<Timeline>,
    dispatcher: Arc<Dispatcher>,
    lifecycle: Arc<BranchLifecycle>,
) -> Value {
    let cancelled = ctx.cancellation_token();
    let subscriber = ctx.clone();
    let owner = part.to_owned();
    let stop = cancelled.clone();
    let subscribe = Value::callback(move |args| {
        if stop.is_cancelled() {
            return Ok(failure("closed", "branch row ended"));
        }
        let args = match args.list() {
            Ok(args) => args,
            Err(error) => return Ok(failure("invalid_params", error)),
        };
        let [keys, callback]: [Value; 2] = match args.try_into() {
            Ok(args) => args,
            Err(_) => return Ok(failure("invalid_params", "expected keys and a callback")),
        };
        let keys = match decode::<Vec<String>>(keys) {
            Ok(keys) => keys,
            Err(error) => return Ok(error),
        };
        let callback = match callback.reference() {
            Ok(callback) if callback.is_function() => callback,
            _ => return Ok(failure("invalid_params", "expected a callback function")),
        };
        let keys: Vec<_> = keys.iter().map(String::as_str).collect();
        let name = owner.clone();
        let cancelled = stop.clone();
        Ok(data(dispatcher.subscribe(&subscriber, &owner, &keys, move |event| {
            let callback = callback.clone();
            let cancelled = cancelled.clone();
            let name = name.clone();
            Box::pin(async move {
                let reply = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => return Err(AgentError::Cancelled(format!("{name} callback"))),
                    reply = async {
                        let event = serde_json::to_value(event).expect("native event serializes");
                        settle(callback.call_async(Value::List(vec![event.into()])).await?).await
                    } => reply,
                }.map_err(|error| AgentError::fault(&name, error.to_string()))?;
                match reply {
                    Value::Undefined | Value::Data(serde_json::Value::Null) => Ok(()),
                    _ => Err(AgentError::fault(&name, "Dispatcher callback must return unit")),
                }
            })
        })))
    });

    let committed = timeline.clone();
    let stop = cancelled.clone();
    let committed = Value::callback(move |args| {
        if stop.is_cancelled() {
            return Ok(failure("closed", "branch row ended"));
        }
        match args.list() {
            Ok(args) if args.is_empty() => Ok(data(Ok(committed.committed()))),
            _ => Ok(failure("invalid_params", "committed takes no arguments")),
        }
    });
    let history = timeline.clone();
    let stop = cancelled.clone();
    let history = Value::callback(move |args| {
        let timeline = history.clone();
        let cancelled = stop.clone();
        Ok(Value::future(async move {
            let request = match request::<Through>(args) {
                Ok(request) => request,
                Err(error) => return Ok(error),
            };
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => Ok(failure("closed", "branch row ended")),
                history = timeline.history(Some(request.through)) => Ok(data(history)),
            }
        }))
    });
    let stop = cancelled.clone();
    let local = Value::callback(move |args| {
        let timeline = timeline.clone();
        let cancelled = stop.clone();
        Ok(Value::future(async move {
            let request = match request::<Local>(args) {
                Ok(request) => request,
                Err(error) => return Ok(error),
            };
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => Ok(failure("closed", "branch row ended")),
                history = timeline.local(request.start..=request.through) => Ok(data(history)),
            }
        }))
    });
    let active = lifecycle.clone();
    let stop = cancelled.clone();
    let activated = Value::future(async move {
        let active = tokio::select! {
            biased;
            _ = stop.cancelled() => false,
            active = active.activated() => active,
        };
        Ok(json!(active).into())
    });
    let closing = Value::future(async move {
        tokio::select! {
            _ = cancelled.cancelled() => {},
            _ = lifecycle.closing() => {},
        }
        Ok(Value::Undefined)
    });
    Value::Record(
        [
            (
                "identity".into(),
                json!({"sessionId":identity.session_id,"branchId":identity.branch_id}).into(),
            ),
            (
                "dispatcher".into(),
                Value::Record([("subscribe".into(), subscribe)].into()),
            ),
            (
                "timeline".into(),
                Value::Record(
                    [
                        ("committed".into(), committed),
                        ("history".into(), history),
                        ("local".into(), local),
                    ]
                    .into(),
                ),
            ),
            ("activated".into(), activated),
            ("closing".into(), closing),
        ]
        .into(),
    )
}

fn branch_reference(branch: Branch, cancelled: CancellationToken) -> Value {
    let input = branch.clone();
    let stop = cancelled.clone();
    Value::Record(
        [
            ("id".into(), json!(branch.id()).into()),
            (
                "input".into(),
                Value::callback(move |args| {
                    if stop.is_cancelled() {
                        return Ok(failure("closed", "TreeCommands owner ended"));
                    }
                    Ok(match request::<InputMessageData>(args) {
                        Ok(message) => data(input.input(message)),
                        Err(error) => error,
                    })
                }),
            ),
            (
                "service".into(),
                Value::callback(move |args| {
                    if cancelled.is_cancelled() {
                        return Ok(failure("closed", "TreeCommands owner ended"));
                    }
                    let name = match request::<ServiceName>(args) {
                        Ok(request) => request.name,
                        Err(error) => return Ok(error),
                    };
                    Ok(match branch.named_service::<ForeignBranchService>(&name) {
                        Ok(service) => success(service.reference()),
                        Err(error) => native_error(error),
                    })
                }),
            ),
        ]
        .into(),
    )
}

struct ForeignTreeCommands {
    commands: Arc<TreeCommands>,
    cancelled: CancellationToken,
}
impl HostDispatch for ForeignTreeCommands {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"get":"sync","list":"sync","branch":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if self.cancelled.is_cancelled() {
            return Ok(failure("closed", "TreeCommands owner ended"));
        }
        Ok(match method {
            "get" => {
                let request = match request::<BranchId>(args) {
                    Ok(request) => request,
                    Err(error) => return Ok(error),
                };
                match self.commands.get(&request.id) {
                    Ok(branch) => success(branch_reference(branch, self.cancelled.clone())),
                    Err(error) => native_error(error),
                }
            }
            "list" => match request::<Empty>(args) {
                Ok(_) => data(Ok(self.commands.list())),
                Err(error) => error,
            },
            "branch" => {
                let request = match request::<Create>(args) {
                    Ok(request) => request,
                    Err(error) => return Ok(error),
                };
                let commands = self.commands.clone();
                let cancelled = self.cancelled.clone();
                let admission = CancellationToken::new();
                let abandon = admission.clone().drop_guard();
                Value::future(async move {
                    let _abandon = abandon;
                    let result = tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => return Ok(failure("closed", "TreeCommands owner ended")),
                        result = commands.branch(request.parent, request.at, request.options, admission) => result,
                    };
                    Ok(match result {
                        Ok(branch) => success(branch_reference(branch, cancelled)),
                        Err(error) => native_error(error),
                    })
                })
            }
            _ => failure("invalid_params", "unknown treeCommands method"),
        })
    }
}

pub(super) fn provide(
    ctx: &Ctx,
    scope: &SessionScope,
    commands: Arc<TreeCommands>,
) -> Result<(), CordisError> {
    ctx.provide_as(
        host_key_in("treeCommands", scope.instance),
        Arc::new(ForeignTreeCommands {
            commands,
            cancelled: ctx.cancellation_token(),
        }) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}
