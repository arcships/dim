//! The user's subagent model policy (`docs/design/subagent-model-policy.md`):
//! the `collaboration` Settings source Collaboration reads for each new member,
//! and its process management owner.
use crate::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{BusinessDocument, RegisteredDocument, Settings, SettingsError, SettingsScope},
};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use dim_agent_runtime::Names;
use dim_agent_types::ModelId;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

/// Which model a new native Collaboration member uses. The document is the
/// policy itself; user scope only, so no project file can set it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum ModelPolicy {
    /// The requested profile's model, or the existing default.
    Agent {},
    /// The model selection adopted by the requesting branch.
    Inherit {},
    Fixed {
        #[serde(deserialize_with = "closed_model")]
        model: ModelId,
    },
}
impl Default for ModelPolicy {
    fn default() -> Self {
        Self::Agent {}
    }
}
impl BusinessDocument for ModelPolicy {
    const NAMESPACE: &'static str = "collaboration";
    const USER_FILE: &'static str = "collaboration.json";
    fn validate(&self) -> crate::settings::Result<()> {
        match self {
            Self::Fixed { model } if model.provider_id.is_empty() || model.model_id.is_empty() => {
                Err(SettingsError::Invalid("empty fixed model id"))
            }
            _ => Ok(()),
        }
    }
}

/// `ModelId` accepts unknown fields elsewhere; this closed source does not.
fn closed_model<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ModelId, D::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Closed {
        provider_id: String,
        model_id: String,
    }
    let Closed {
        provider_id,
        model_id,
    } = Closed::deserialize(deserializer)?;
    Ok(ModelId {
        provider_id,
        model_id,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetPolicy {
    expected_revision: String,
    policy: ModelPolicy,
}
#[derive(Serialize)]
pub(crate) struct PolicyView {
    revision: String,
    pub(crate) policy: ModelPolicy,
}
#[derive(Serialize)]
struct Revision {
    revision: String,
}

#[derive(Clone)]
pub(crate) struct CollaborationModelPolicy(Arc<RegisteredDocument<ModelPolicy>>);

impl CollaborationModelPolicy {
    pub(crate) async fn read(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> crate::settings::Result<PolicyView> {
        let snapshot = self.0.read(SettingsScope::User, cancel).await?;
        Ok(PolicyView {
            revision: snapshot.revision,
            policy: snapshot.document,
        })
    }
}

impl rutis_bridge::session::HostDispatch for CollaborationModelPolicy {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"read":"async"}))
    }
    fn invoke(
        &self,
        method: &str,
        args: rutis_bridge::session::Value,
    ) -> rutis_bridge::session::Reply {
        use rutis_bridge::session::{Error, Value};
        if method != "read" {
            return Err(Error::Value(format!(
                "unknown collaborationModelPolicy method {method}"
            )));
        }
        let (_request,): (Empty,) = serde_json::from_value(args.json()?)
            .map_err(|_| Error::Value("expected one empty request".into()))?;
        let owner = self.clone();
        Ok(Value::future(async move {
            let cancel = tokio_util::sync::CancellationToken::new();
            let _guard = cancel.clone().drop_guard();
            let view = owner.read(&cancel).await.map_err(|error| {
                rutis_bridge::exports::failure(
                    "collaborationModelPolicy",
                    error.code(),
                    error.to_string(),
                    None,
                )
            })?;
            rutis_bridge::session::arg(&view)
        }))
    }
}

fn error(error: SettingsError) -> RpcError {
    let kind = match &error {
        SettingsError::Closed => return RpcError::reloading(),
        SettingsError::Cancelled => "operation_interrupted",
        SettingsError::Conflict => "collaboration_model_policy_conflict",
        _ => "collaboration_model_policy_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

/// Process owner of the `collaboration` source and its management methods,
/// apart from Collaboration so it reloads without unloading Rpc.
pub struct CollaborationModelPolicyPlugin {
    settings: Key<Settings>,
    methods: Key<ManagementMethods>,
}
impl CollaborationModelPolicyPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for CollaborationModelPolicyPlugin {
    fn name(&self) -> &str {
        "CollaborationModelPolicy"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let methods = ctx.require_at(&self.methods)?;
            let settings = settings.register_document::<ModelPolicy>(ctx)?;
            let owner = Arc::new(CollaborationModelPolicy(settings));
            ctx.provide_as::<CollaborationModelPolicy>(owner.clone())?;
            ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key("collaborationModelPolicy"),
                owner.clone(),
            )?;
            methods.register(
                ctx,
                "collaboration",
                [
                    "collaboration.model_policy",
                    "collaboration.set_model_policy",
                ]
                .into_iter()
                .map(|name| {
                    MethodDecl::new(
                        name,
                        name != "collaboration.model_policy",
                        Interruption::Report,
                    )
                })
                .collect(),
                Arc::new(move |call, stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let owner = owner.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "collaboration.model_policy" => {
                                let _: Empty = params(value)?;
                                reply(owner.read(&stopping).await.map_err(error)?)
                            }
                            "collaboration.set_model_policy" => {
                                let p: SetPolicy = params(value)?;
                                if let Err(SettingsError::Invalid(reason)) = p.policy.validate() {
                                    return Err(RpcError::invalid(reason));
                                }
                                let snapshot = owner
                                    .0
                                    .update(
                                        SettingsScope::User,
                                        &p.expected_revision,
                                        p.policy,
                                        &stopping,
                                    )
                                    .await
                                    .map_err(error)?;
                                reply(Revision {
                                    revision: snapshot.revision,
                                })
                            }
                            _ => unreachable!("registered collaboration management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
