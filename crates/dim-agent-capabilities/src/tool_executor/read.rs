use crate::{
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params},
    timeline_tree::TimelineTree,
};
use dim_agent_runtime::*;
use dim_agent_types::{ToolRead, ToolUseData};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{num::NonZeroU64, sync::Arc};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Params {
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    request_id: String,
    call_id: String,
    through: Option<u64>,
}

pub struct ToolMethodsPlugin {
    tree: Key<TimelineTree>,
    methods: Key<SessionMethods>,
}
impl ToolMethodsPlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            tree: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for ToolMethodsPlugin {
    fn name(&self) -> &str {
        "ToolMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            ctx.require_at(&self.methods)?.register(
                ctx,
                "session.tool",
                vec![MethodDecl::new(
                    "session.tool.get",
                    false,
                    Interruption::Report,
                )],
                Arc::new(move |call, cancel, _| {
                    let tree = tree.clone();
                    Box::pin(async move {
                        let p: Params = params(call.params)?;
                        let field = |path: &str, value: serde_json::Value| QueryFilter::In {
                            value: QueryOperand::Field { path: path.into() },
                            values: vec![value],
                        };
                        let query = TimelineQuery {
                            scope: TimelineQueryScope::Branch,
                            from: None,
                            through: p.through,
                            include_publishing: false,
                            query: QuerySpec {
                                filter: Some(QueryFilter::And {
                                    conditions: vec![
                                        field("kind", "step/tool-use".into()),
                                        field("data.request_id", p.request_id.into()),
                                        field("data.call.id", p.call_id.into()),
                                    ],
                                }),
                                select: QuerySelect::Fields {
                                    fields: vec!["data".into()],
                                },
                                group_by: vec![],
                                pick: Some(QueryPick::First),
                                order_by: vec![],
                            },
                            page: Some(QueryPage {
                                limit: NonZeroU64::MIN,
                                after: None,
                            }),
                        };
                        let reply = tokio::select! {
                            _=cancel.cancelled()=>return Err(RpcError::interrupted()),
                            reply=tree.query(p.branch_id.clone(),query)=>reply?,
                        };
                        let QueryOutput::Rows { rows } = reply.result else {
                            unreachable!("tool properties")
                        };
                        let call = rows.first().map(|row| {
                            let QueryValue::Value { value } = &row.values[0] else {
                                unreachable!("tool data")
                            };
                            serde_json::from_value::<ToolUseData>(value.clone())
                                .expect("tool data contract")
                        });
                        Ok(serde_json::to_value(ToolRead {
                            branch_id: p.branch_id,
                            through: reply.through,
                            call,
                        })
                        .expect("tool read is JSON"))
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
