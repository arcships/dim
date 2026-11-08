//! The user's subagent model policy (`docs/design/subagent-model-policy.md`):
//! the `collaboration` Settings source Collaboration reads for each new member,
//! and its process management owner.
use crate::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
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
struct PolicyView {
    revision: String,
    policy: ModelPolicy,
}
#[derive(Serialize)]
struct Revision {
    revision: String,
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
    settings: Key<SettingsService>,
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
            settings.register::<ModelPolicy>(ctx)?;
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
                    let settings = settings.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "collaboration.model_policy" => {
                                let _: Empty = params(value)?;
                                let snapshot = settings
                                    .read::<ModelPolicy>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                reply(PolicyView {
                                    revision: snapshot.revision,
                                    policy: snapshot.document,
                                })
                            }
                            "collaboration.set_model_policy" => {
                                let p: SetPolicy = params(value)?;
                                if let Err(SettingsError::Invalid(reason)) = p.policy.validate() {
                                    return Err(RpcError::invalid(reason));
                                }
                                let snapshot = settings
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
