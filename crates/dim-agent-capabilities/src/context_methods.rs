//! `sessionMethods["session.context"]`: manual compaction and the usage
//! query, exposed from their owners (ContextCompaction §4.1).
use crate::{
    agent_loop::AgentLoop,
    context_preparation::{ContextOpError, ContextPreparation},
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    Branch, Names, PredicateOp, TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy,
    TimelineQueryPredicate, TimelineQueryScope,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{num::NonZeroU64, sync::Arc};
use tokio_util::sync::CancellationToken;

pub const NAMESPACE: &str = "session.context";

pub struct ContextMethodsPlugin {
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
}
impl ContextMethodsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for ContextMethodsPlugin {
    fn name(&self) -> &str {
        "ContextMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                NAMESPACE,
                vec![
                    MethodDecl::new("session.context.compact", true, Interruption::Report),
                    MethodDecl::new("session.context.usage", false, Interruption::Report),
                ],
                Arc::new(move |method, value, cancel, _| {
                    let tree = tree.clone();
                    Box::pin(async move {
                        #[derive(serde::Deserialize)]
                        #[serde(rename_all = "camelCase")]
                        struct Params {
                            branch_id: String,
                        }
                        let Params { branch_id } = params(value)?;
                        let branch = tree.get(&branch_id)?;
                        match method.as_str() {
                            "session.context.compact" => compact(&branch, &cancel).await,
                            _ => usage(&branch, cancel).await,
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
/// Writes `context/compact`, or joins the outstanding command, and waits for
/// the result that carries that command.
async fn compact(branch: &Branch, cancel: &CancellationToken) -> Result<Value, RpcError> {
    let command = branch.service::<AgentLoop>()?.compact()?;
    let publication = branch.publication();
    let mut watch = publication.watch();
    let mut after = command;
    loop {
        let head = watch.borrow_and_update().0;
        if head > after {
            let result = branch
                .query(TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    include_publishing: false,
                    from_event: None,
                    from_sequence: Some(after + 1),
                    order_by: TimelineQueryOrderBy::Sequence,
                    direction: TimelineQueryDirection::Ascending,
                    kinds: vec!["context/compaction".into()],
                    flags: Vec::new(),
                    event_id: None,
                    predicate: Some(TimelineQueryPredicate {
                        op: PredicateOp::Eq,
                        path: "command".into(),
                        value: json!(command),
                    }),
                    predicates: Vec::new(),
                    limit: NonZeroU64::MIN,
                    offset: 0,
                })
                .await?
                .events
                .into_iter()
                .next();
            if let Some(event) = result {
                let EventKind::ContextCompaction(data) = &event.kind else {
                    unreachable!("matched a compaction")
                };
                return match &data.outcome {
                    ContextCompactionResult::Completed { .. } => {
                        reply(json!({"status": "completed", "sequence": event.sequence}))
                    }
                    ContextCompactionResult::Skipped { reason, .. } => {
                        reply(json!({"status": "skipped", "reason": reason}))
                    }
                    ContextCompactionResult::Failed { error, .. } => {
                        Err(RpcError::app(-32000, "compaction_failed", error.clone()))
                    }
                    ContextCompactionResult::Cancelled { reason } => Err(RpcError::app(
                        -32000,
                        "compaction_cancelled",
                        reason.clone(),
                    )),
                };
            }
            after = head;
        }
        tokio::select! {
            changed = watch.changed() => {
                if changed.is_err() {
                    return Err(RpcError::closed());
                }
            }
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
        }
    }
}

async fn usage(branch: &Branch, cancel: CancellationToken) -> Result<Value, RpcError> {
    match branch.service::<ContextPreparation>()?.usage(cancel).await {
        Ok(usage) => reply(usage),
        Err(ContextOpError::NotConfigured) => Err(RpcError::app(
            -32000,
            "context_not_configured",
            ContextOpError::NotConfigured.to_string(),
        )),
        Err(ContextOpError::Model(error)) => Err(RpcError::app(
            -32000,
            "context_model_failed",
            error.to_string(),
        )),
        Err(ContextOpError::Read(error)) => Err(error.into()),
    }
}
