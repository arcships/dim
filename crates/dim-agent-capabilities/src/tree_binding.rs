//! The language binding of TimelineTree (C5, branch-typescript.md): the
//! Session service `treeCommands`, the BranchRef values it returns and the
//! public business services foreign branch parts declare.
use crate::timeline_tree::TreeCommands;
use dim_agent_runtime::{AgentError, Branch, BranchOptions, BranchService, PublicService};
use dim_agent_types::InputMessageData;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value as BridgeValue};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Name of the Session service, as TypeScript rows `inject` it.
pub const TREE_COMMANDS: &str = "treeCommands";

/// A business service a foreign branch part declares next to its
/// `branchPart:<part>`: its own dispatch and its declared method modes. The
/// original row publishes it under its name only after `bind` completed.
pub struct ForeignBranchService {
    pub service: Arc<dyn HostDispatch>,
    pub methods: Map<String, Value>,
}
impl BranchService for ForeignBranchService {}
impl PublicService for ForeignBranchService {}

/// `{ok: true, value}` of the named-service envelope.
pub fn ok(value: BridgeValue) -> BridgeValue {
    BridgeValue::Record([("ok".into(), json!(true).into()), ("value".into(), value)].into())
}
fn error(code: &str, message: String) -> BridgeValue {
    json!({"ok": false, "error": {"code": code, "message": message}}).into()
}
/// A native failure with its classifying code; the code grants no retry.
pub fn failure(failure: &AgentError) -> BridgeValue {
    let code = match failure {
        AgentError::InvalidInput(_) => "invalid_input",
        AgentError::UnknownBranch(_) => "unknown_branch",
        AgentError::Closed(_) => "closed",
        AgentError::Cancelled(_) => "cancelled",
        AgentError::Reloading(_) => "reloading",
        _ => "failed",
    };
    error(code, failure.to_string())
}
pub fn invalid_params(message: impl Into<String>) -> BridgeValue {
    error("invalid_params", message.into())
}
/// A committed native value as bridge data.
pub fn data(value: &impl serde::Serialize) -> BridgeValue {
    serde_json::to_value(value)
        .expect("native values serialize")
        .into()
}

/// Decodes the one data argument of a call; the native operation does not
/// run when it does not decode.
pub fn request<T: DeserializeOwned>(args: BridgeValue) -> Result<T, BridgeValue> {
    let [value]: [BridgeValue; 1] = args
        .list()
        .ok()
        .and_then(|args| args.try_into().ok())
        .ok_or_else(|| invalid_params("expected one request argument"))?;
    let value = value
        .json()
        .map_err(|error| invalid_params(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| invalid_params(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Get {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    parent: String,
    at: u64,
    options: BranchOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    name: String,
}

/// A branch facade value: its id, validated input and public-service lookup.
/// It follows the branch owner's lifetime.
pub fn branch_ref(branch: Branch) -> BridgeValue {
    let writer = branch.clone();
    let input = BridgeValue::callback(move |args| {
        Ok(match request::<InputMessageData>(args) {
            Ok(input) => match writer.input(input) {
                Ok(event) => ok(data(&*event)),
                Err(error) => failure(&error),
            },
            Err(invalid) => invalid,
        })
    });
    let id = branch.id().to_owned();
    let service = BridgeValue::callback(move |args| {
        Ok(match request::<Lookup>(args) {
            Ok(Lookup { name }) => match branch.named_service::<ForeignBranchService>(&name) {
                Ok(service) => ok(methods(service)),
                Err(error) => failure(&error),
            },
            Err(invalid) => invalid,
        })
    });
    BridgeValue::Record(
        [
            ("id".into(), json!(id).into()),
            ("input".into(), input),
            ("service".into(), service),
        ]
        .into(),
    )
}

/// The looked-up provider's methods, called on that provider directly: its
/// replies and errors are its own contract, without another envelope.
fn methods(service: Arc<ForeignBranchService>) -> BridgeValue {
    BridgeValue::Record(
        service
            .methods
            .keys()
            .map(|method| {
                let (service, name) = (service.clone(), method.clone());
                let call = BridgeValue::callback(move |args| service.service.invoke(&name, args));
                (method.clone(), call)
            })
            .collect(),
    )
}

/// `treeCommands` over the Session's TreeCommands.
pub struct TreeCommandsService(pub Arc<TreeCommands>);
impl HostDispatch for TreeCommandsService {
    fn methods(&self) -> Option<Value> {
        Some(json!({"get": "sync", "list": "sync", "branch": "async"}))
    }
    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        match method {
            "get" => Ok(match request::<Get>(args) {
                Ok(Get { id }) => match self.0.get(&id) {
                    Ok(branch) => ok(branch_ref(branch)),
                    Err(error) => failure(&error),
                },
                Err(invalid) => invalid,
            }),
            "list" => Ok(match request::<Empty>(args) {
                Ok(Empty {}) => ok(data(&self.0.list())),
                Err(invalid) => invalid,
            }),
            "branch" => {
                // The optional AbortSignal only marks the call cancellable:
                // dropping its future cancels admission.
                let mut args = args.list()?;
                if args.len() == 2 && args[1] == BridgeValue::Signal {
                    args.pop();
                }
                let create = request::<Create>(BridgeValue::List(args));
                let commands = self.0.clone();
                Ok(BridgeValue::future(async move {
                    let Create {
                        parent,
                        at,
                        options,
                    } = match create {
                        Ok(create) => create,
                        Err(invalid) => return Ok(invalid),
                    };
                    let admission = CancellationToken::new();
                    let _cancel = admission.clone().drop_guard();
                    Ok(
                        match commands.branch(parent, at, options, admission).await {
                            Ok(branch) => ok(branch_ref(branch)),
                            Err(error) => failure(&error),
                        },
                    )
                }))
            }
            _ => Err(Error::Value(format!(
                "unknown treeCommands method {method}"
            ))),
        }
    }
}
