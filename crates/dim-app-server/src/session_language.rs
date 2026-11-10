//! Session part: `session.language.get` and `session.language.update`
//! (methods contract A.1). The value lives in the Session's config record and
//! in PromptSources' live cell; this part reads both and changes both, then
//! has Prompt adopt the `dim/language` source.
use crate::storage::SessionStore;
use dim_agent_capabilities::{
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params},
    prompt_sources::{self, PromptSourcesBranch},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{AgentError, Names, ReplyLanguage};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) const NAMESPACE: &str = "session.language";
const UNAVAILABLE: &str = "language_unavailable";
const FAILED: &str = "language_failed";

/// The Session's two reply-language methods; one per Session instance.
pub(crate) struct SessionLanguagePlugin {
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
    /// Native storage, when the assembly has it: the record carries the value.
    store: Option<Key<SessionStore>>,
}
impl SessionLanguagePlugin {
    pub(crate) fn new(names: &Names<'_>, stored: bool) -> Result<Self, Error> {
        Ok(Self {
            tree: names.key()?,
            methods: names.key()?,
            store: stored.then(|| names.key::<SessionStore>()).transpose()?,
        })
    }
}
impl DynPlugin for SessionLanguagePlugin {
    fn name(&self) -> &str {
        "SessionLanguage"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut injects = vec![self.tree.erase(), self.methods.erase()];
        if let Some(store) = &self.store {
            injects.push(store.erase());
        }
        injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            let store = match &self.store {
                Some(key) => Some(ctx.require_at(key)?),
                None => None,
            };
            methods.register(
                ctx,
                NAMESPACE,
                vec![
                    MethodDecl::new("session.language.get", false, Interruption::Report),
                    MethodDecl::new("session.language.update", true, Interruption::Report),
                ],
                Arc::new(move |call, cancel, _| {
                    let tree = tree.clone();
                    let store = store.clone();
                    let MethodCall { method, params, .. } = call;
                    Box::pin(async move {
                        match method.as_str() {
                            "session.language.get" => read(&tree).await,
                            _ => change(&tree, store.as_deref(), params, cancel).await,
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// `session.language.get`: the value the Session holds now, `null` when the
/// operator selected none.
async fn read(tree: &TreeCommands) -> Result<Value, RpcError> {
    let language = cell(tree)?.language();
    let _guard = language.gate().await;
    Ok(language_value(language.get()))
}

/// `session.language.update`: writes the record, replaces the live cell and
/// has Prompt adopt the source, all under the Session's language gate.
async fn change(
    tree: &TreeCommands,
    store: Option<&SessionStore>,
    value: Value,
    cancel: CancellationToken,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        session_id: String,
        language: Change,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Clear {
        action: ClearAction,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum ClearAction {
        Clear,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Change {
        Clear(Clear),
        Code(ReplyLanguage),
    }
    let Params {
        session_id,
        language,
    } = params(value)?;
    let language = match language {
        Change::Clear(Clear {
            action: ClearAction::Clear,
        }) => None,
        Change::Code(code) => Some(code),
    };
    let branch = tree.get("main").map_err(RpcError::from)?;
    let language_cell = cell(tree)?.language();
    let _guard = language_cell.gate().await;
    // A failure before this write leaves the value unchanged; once the record
    // is written and the cell replaced, a later failure or a cancellation
    // keeps the new value stored and leaves its delivery to each branch's
    // periodic check.
    if let Some(store) = store {
        store
            .set_session_language(&session_id, language)
            .await
            .map_err(|error| RpcError::app(-32000, FAILED, error.to_string()))?;
    }
    let applied = language_cell.replace(language);
    let adoption = branch.configure(prompt_sources::language_update());
    let outcome = tokio::select! {
        result = adoption => crate::connection::configuration_outcome(result.map_err(RpcError::from)?)?,
        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
    };
    // Every loaded branch adopts the value before this reply, so a request on
    // a side branch right after the update carries it; an idle branch only
    // gains a state entry and no turn. A cancellation abandons the remaining
    // branches to the periodic check, as on main. Unloaded branches read the
    // value when they load, and the periodic check remains the fallback.
    for info in tree.list() {
        if info.id == "main" {
            continue;
        }
        let Ok(side) = tree.get(&info.id) else {
            continue;
        };
        let adoption = side.configure(prompt_sources::language_update());
        let result = tokio::select! {
            result = adoption => result,
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
        };
        if let Err(error) = result {
            tracing::warn!(
                %error,
                branch = %info.id,
                "the reply language was not adopted on a loaded branch"
            );
        }
    }
    let mut answer = language_value(applied);
    answer["outcome"] = json!(outcome);
    Ok(answer)
}

/// The branch service that carries the Session's cell. An assembly without
/// PromptSources has none, which is an explicit unavailable result and never a
/// default language.
fn cell(tree: &TreeCommands) -> Result<Arc<PromptSourcesBranch>, RpcError> {
    let branch = tree.get("main").map_err(RpcError::from)?;
    match branch.service::<PromptSourcesBranch>() {
        Ok(service) => Ok(service),
        Err(AgentError::Closed(_)) => Err(RpcError::closed()),
        Err(_) => Err(RpcError::app(
            -32000,
            UNAVAILABLE,
            "This Session's prompt sources are unavailable",
        )),
    }
}

fn language_value(language: Option<ReplyLanguage>) -> Value {
    json!({ "language": language.map(ReplyLanguage::code) })
}
