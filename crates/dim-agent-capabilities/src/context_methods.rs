//! `sessionMethods["session.context"]`: manual compaction and the usage
//! query, exposed from their owners (ContextCompaction §4.1).
use crate::{
    agent_loop::AgentLoop,
    context_preparation::{ContextOpError, ContextPreparation},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    Branch, Names, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec,
    QueryValue, TimelineQuery, TimelineQueryScope,
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
                Arc::new(move |call, cancel, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
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
                    from: Some(TimelinePosition {
                        branch_id: branch.id().into(),
                        sequence: after + 1,
                    }),
                    through: Some(head),
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["context/compaction".into()],
                                },
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "data.command".into(),
                                    },
                                    values: vec![json!(command)],
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec![
                                "position.sequence".into(),
                                "data.status".into(),
                                "data.reason".into(),
                                "data.error".into(),
                            ],
                        },
                        group_by: vec![],
                        pick: None,
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                })
                .await?;
            let QueryOutput::Rows { rows } = result.result else {
                unreachable!("field selection returns rows")
            };
            if let Some(row) = rows.into_iter().next() {
                let [
                    QueryValue::Value { value: sequence },
                    QueryValue::Value {
                        value: Value::String(status),
                    },
                    reason,
                    error,
                ] = row.values.as_slice()
                else {
                    unreachable!("compaction has a sequence and status")
                };
                return match status.as_str() {
                    "completed" => reply(json!({"status": "completed", "sequence": sequence})),
                    "skipped" => {
                        let QueryValue::Value { value: reason } = reason else {
                            unreachable!("skipped compaction has a reason")
                        };
                        reply(json!({"status": "skipped", "reason": reason}))
                    }
                    "failed" => {
                        let QueryValue::Value {
                            value: Value::String(error),
                        } = error
                        else {
                            unreachable!("failed compaction has an error")
                        };
                        Err(RpcError::app(-32000, "compaction_failed", error.clone()))
                    }
                    "cancelled" => {
                        let QueryValue::Value {
                            value: Value::String(reason),
                        } = reason
                        else {
                            unreachable!("cancelled compaction has a reason")
                        };
                        Err(RpcError::app(
                            -32000,
                            "compaction_cancelled",
                            reason.clone(),
                        ))
                    }
                    _ => unreachable!("compaction status follows the event contract"),
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
        Err(ContextOpError::NotReady) => Err(RpcError::app(
            -32000,
            "context_usage_unavailable",
            ContextOpError::NotReady.to_string(),
        )),
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
