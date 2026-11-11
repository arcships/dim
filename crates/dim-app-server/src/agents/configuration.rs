use super::{AgentIndex, SESSION_NAMESPACE};
use crate::connection::{configuration_outcome, port_error};
use dim_agent_capabilities::{
    host::ModelSelectionRequest,
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    prompt::Prompt,
    prompt_sources,
    prompt_state::PromptSnapshot,
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{Names, PromptSourceTarget};
use dim_agent_types::{ModelSelection, PromptScope};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Agents' Session entry adopts profile configuration on the addressed branch
/// and reads back the profile the branch's configuration was adopted from.
pub(crate) struct AgentsSessionPlugin {
    index: Key<AgentIndex>,
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
}
impl AgentsSessionPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            index: names.key()?,
            tree: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for AgentsSessionPlugin {
    fn name(&self) -> &str {
        "AgentsSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.index.erase(), self.tree.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let index = ctx.require_at(&self.index)?;
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                SESSION_NAMESPACE,
                vec![
                    MethodDecl::new("session.agent.update", true, Interruption::Report),
                    MethodDecl::new("session.agent.get", false, Interruption::Report),
                ],
                Arc::new(move |call, cancel, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let index = index.clone();
                    let tree = tree.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "session.agent.get" => configured_agent(&tree, value),
                            _ => adopt_profile(index, tree, value, cancel).await,
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// `session.agent.get`: the profile recorded on the branch's adopted
/// configuration. It reads the branch's published Prompt snapshot, which
/// projects the configuration inherited from the branch's ancestry.
fn configured_agent(tree: &TreeCommands, value: Value) -> Result<Value, RpcError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        #[serde(rename = "sessionId")]
        _session_id: String,
        branch_id: String,
    }
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    }
    let Params { branch_id, .. } = params(value)?;
    let agent_id = tree
        .get(&branch_id)?
        .get::<PromptSnapshot>()
        .ok_or_else(RpcError::reloading)?
        .configuration
        .and_then(|configuration| configuration.metadata)
        .and_then(|metadata| metadata.agent_id);
    reply(Answer { agent_id })
}

/// `session.agent.update`: adopts the named profile's prompt and optional
/// model on `main`. The configuration it publishes records the profile, so
/// the record is the adopted configuration's own metadata.
async fn adopt_profile(
    index: Arc<AgentIndex>,
    tree: Arc<TreeCommands>,
    value: Value,
    cancel: CancellationToken,
) -> Result<Value, RpcError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        #[serde(rename = "sessionId")]
        _session_id: String,
        branch_id: String,
        agent_id: String,
    }
    let Params {
        branch_id,
        agent_id,
        ..
    } = params(value)?;
    if branch_id != "main" {
        return Err(RpcError::invalid(
            "Agent configuration updates only support main",
        ));
    }
    let branch = tree.get(&branch_id)?;
    let profile = index
        .host(Some(&agent_id))
        .ok_or_else(crate::protocol::agent_unknown)?
        .profile
        .clone();
    let model = match profile.model {
        Some(target) if index.selects_models() => Some(
            index
                .model_selection()
                .ok_or_else(RpcError::reloading)?
                .resolve(ModelSelectionRequest::at(Some(target), None).map_err(port_error)?)
                .await
                .map_err(port_error)?,
        ),
        Some(target) => Some(ModelSelection {
            provider_id: target.provider_id,
            model_id: target.model_id,
            capability_snapshot: None,
            config: None,
            metadata: None,
        }),
        // A profile without a model keeps the adopted configuration; it is
        // materialized so the configuration can carry the profile's record.
        None => branch
            .get::<PromptSnapshot>()
            .ok_or_else(RpcError::reloading)?
            .configuration
            .map(|configuration| ModelSelection {
                provider_id: configuration.provider_id,
                model_id: configuration.model_id,
                capability_snapshot: configuration.capability_snapshot,
                config: configuration.config,
                metadata: configuration.metadata,
            }),
    };
    let model = model.map(|selection| {
        let mut metadata = selection.metadata.unwrap_or_default();
        metadata.agent_id = Some(agent_id);
        ModelSelection {
            metadata: Some(metadata),
            ..selection
        }
    });
    let scope = PromptScope::Branch {
        timeline: branch_id.clone(),
    };
    let target = PromptSourceTarget {
        source: prompt_sources::SESSION.into(),
        scope: scope.clone(),
    };
    let entry = prompt_sources::session_entry(profile.system_prompt, scope);
    let adoption = branch
        .service::<Prompt>()?
        .configure_loaded(model, vec![(target, entry)]);
    tokio::select! {
        result = adoption => reply(json!({"outcome": configuration_outcome(result?)?})),
        _ = cancel.cancelled() => Err(RpcError::interrupted()),
    }
}
