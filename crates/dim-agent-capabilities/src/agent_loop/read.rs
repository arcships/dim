use crate::{
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde_json::json;
use std::{num::NonZeroU64, sync::Arc};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Params {
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    through: Option<u64>,
    turn_id: Option<String>,
    input_id: Option<String>,
}

pub struct LoopMethodsPlugin {
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
}
impl LoopMethodsPlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            tree: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for LoopMethodsPlugin {
    fn name(&self) -> &str {
        "LoopMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            ctx.require_at(&self.methods)?.register(ctx, "session.turn", vec![MethodDecl::new("session.turn.get", false, Interruption::Report)], Arc::new(move |call, cancel, _| {
                let tree = tree.clone();
                Box::pin(async move {
                    let p: Params = params(call.params)?;
                    if p.turn_id.is_some() && p.input_id.is_some() {
                        return Err(RpcError::invalid("turnId and inputId are mutually exclusive"));
                    }
                    if p.turn_id.as_ref().is_some_and(String::is_empty) || p.input_id.as_ref().is_some_and(String::is_empty) {
                        return Err(RpcError::invalid("A turn or input identity must be nonempty"));
                    }
                    let mut through = p.through;
                    let mut turn_id = p.turn_id.clone();
                    if let Some(input_id) = &p.input_id {
                        let mut consumed = TimelineQuery {
                            scope: TimelineQueryScope::Branch, from: None, through, include_publishing: false,
                            query: QuerySpec {
                                filter: Some(QueryFilter::And { conditions: vec![
                                    QueryFilter::In { value: QueryOperand::Field { path:"kind".into() }, values:vec![json!("input/consumed"),json!("input/deleted")] },
                                    QueryFilter::In { value: QueryOperand::Field { path:"data.input_id".into() }, values:vec![json!(input_id)] },
                                ] }),
                                select: QuerySelect::Fields { fields:vec!["kind".into(),"position.sequence".into()] },
                                group_by:vec![], pick:Some(QueryPick::First), order_by:vec![],
                            }, page:Some(QueryPage {limit:NonZeroU64::MIN,after:None}),
                        };
                        let found = tokio::select! {
                            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                            result = tree.query(p.branch_id.clone(), consumed.clone()) => result?,
                        };
                        through = Some(found.through);
                        let QueryOutput::Rows { rows } = found.result else { unreachable!("consumed input position") };
                        let Some(row) = rows.first() else {
                            return Ok(json!({"branchId":p.branch_id,"through":found.through,"turn":null,"output":null}));
                        };
                        let [QueryValue::Value {value: kind},QueryValue::Value {value: position}] = row.values.as_slice() else { unreachable!("input terminal position") };
                        if kind == "input/deleted" {
                            return Ok(json!({"branchId":p.branch_id,"through":found.through,"turn":null,"output":null,"inputDeleted":true}));
                        }
                        consumed.through = Some(position.as_u64().expect("consumed input sequence"));
                        consumed.query.filter = Some(QueryFilter::In {value:QueryOperand::Field {path:"kind".into()},values:vec![json!("turn/start")]});
                        consumed.query.select = QuerySelect::Fields {fields:vec!["data.turn_id".into()]};
                        consumed.query.pick = Some(QueryPick::Last);
                        let opened = tokio::select! {
                            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                            result = tree.query(p.branch_id.clone(), consumed) => result?,
                        };
                        let QueryOutput::Rows {rows} = opened.result else { unreachable!("input turn identity") };
                        let Some(row) = rows.first() else { return Err(AgentError::fault("LoopMethods", "consumed input has no turn start").into()); };
                        let [QueryValue::Value {value: id}] = row.values.as_slice() else { unreachable!("input turn identity") };
                        turn_id = Some(id.as_str().expect("turn identity").into());
                    }
                    let mut conditions = vec![QueryFilter::In { value: QueryOperand::Field { path: "kind".into() }, values: vec![json!("turn/start"), json!("turn/end")] }];
                    if let Some(id) = &turn_id {
                        conditions.push(QueryFilter::In { value: QueryOperand::Field { path: "data.turn_id".into() }, values: vec![json!(id)] });
                    }
                    let query = TimelineQuery {
                        scope: TimelineQueryScope::Branch, from: None, through, include_publishing: false,
                        query: QuerySpec {
                            filter: Some(QueryFilter::And { conditions }),
                            select: QuerySelect::Fields { fields: ["kind", "position.sequence", "data"].map(String::from).to_vec() },
                            group_by: vec![], pick: None,
                            order_by: vec![QueryOrder { key: QueryOrderKey::Position, direction: TimelineQueryDirection::Descending }],
                        },
                        page: Some(QueryPage { limit: NonZeroU64::MIN, after: None }),
                    };
                    let result = tokio::select! {
                        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                        result = tree.query(p.branch_id.clone(), query) => result?,
                    };
                    let QueryOutput::Rows { rows } = result.result else { unreachable!("turn fields") };
                    let mut turn = rows.into_iter().next().map(|row| {
                        let [QueryValue::Value { value: kind }, QueryValue::Value { value: sequence }, QueryValue::Value { value: data }] = <[_;3]>::try_from(row.values).expect("turn boundary fields") else { unreachable!("turn boundary") };
                        if kind == "turn/end" {
                            let ended:dim_agent_types::TurnEndData = serde_json::from_value(data).expect("turn end contract");
                            json!({"turnId":ended.turn_id,"sequence":sequence,"ended":true,"outcome":ended.outcome})
                        } else {
                            let started:dim_agent_types::TurnStartData = serde_json::from_value(data).expect("turn start contract");
                            json!({"turnId":started.turn_id,"sequence":sequence,"ended":false})
                        }
                    });
                    if (p.turn_id.is_some() || p.input_id.is_some()) && let Some(turn) = &mut turn && turn["outcome"]["status"] == "failed" {
                        let request = TimelineQuery {
                            scope:TimelineQueryScope::Branch,from:None,through:Some(result.through),include_publishing:false,
                            query:QuerySpec {
                                filter:Some(QueryFilter::And {conditions:vec![
                                    QueryFilter::In {value:QueryOperand::Field {path:"kind".into()},values:vec![json!("step/model-output-end")]},
                                    QueryFilter::In {value:QueryOperand::Field {path:"data.turn_id".into()},values:vec![turn["turnId"].clone()]},
                                ]}),
                                select:QuerySelect::Fields {fields:vec!["data".into()]},group_by:vec![],pick:Some(QueryPick::Last),order_by:vec![],
                            },page:Some(QueryPage {limit:NonZeroU64::MIN,after:None}),
                        };
                        let model = tokio::select! {
                            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                            result = tree.query(p.branch_id.clone(),request) => result?,
                        };
                        let QueryOutput::Rows {rows} = model.result else {unreachable!("model terminal outcome")};
                        if let Some(row) = rows.into_iter().next() {
                            let [QueryValue::Value {value:data}] = <[_;1]>::try_from(row.values).expect("model terminal fields") else {unreachable!("model terminal data")};
                            let ended:dim_agent_types::ModelOutputEndData = serde_json::from_value(data).expect("model terminal contract");
                            turn["modelOutcome"] = serde_json::to_value(ended.outcome).expect("model outcome JSON");
                        }
                    }
                    let mut output = None;
                    if let Some(turn) = &turn && turn["ended"] == false && p.turn_id.is_none() && p.input_id.is_none() {
                        let mut request = TimelineQuery {
                            scope: TimelineQueryScope::Branch, from: None, through: Some(result.through), include_publishing: false,
                            query: QuerySpec {
                                filter: Some(QueryFilter::And { conditions: vec![
                                    QueryFilter::In { value: QueryOperand::Field { path: "kind".into() }, values: vec![json!("step/model-start")] },
                                    QueryFilter::In { value: QueryOperand::Field { path: "data.turn_id".into() }, values: vec![turn["turnId"].clone()] },
                                    QueryFilter::Not { condition: Box::new(QueryFilter::Exists { condition: Box::new(QueryFilter::And { conditions: vec![
                                        QueryFilter::In { value: QueryOperand::Field { path: "kind".into() }, values: vec![json!("step/model-output-end")] },
                                        QueryFilter::Compare { left: QueryOperand::Field { path: "data.request_id".into() }, comparison: QueryComparison::Eq, right: QueryOperand::Outer { path: "data.request_id".into() } },
                                    ] }) }) },
                                ] }),
                                select: QuerySelect::Fields { fields: vec!["data.request_id".into()] }, group_by: vec![], pick: Some(QueryPick::Last), order_by: vec![],
                            }, page: Some(QueryPage { limit: NonZeroU64::MIN, after: None }),
                        };
                        let current = tokio::select! {
                            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                            result = tree.query(p.branch_id.clone(), request.clone()) => result?,
                        };
                        let QueryOutput::Rows { rows } = current.result else { unreachable!("request identity") };
                        if let Some(row) = rows.first() {
                            let QueryValue::Value { value: id } = &row.values[0] else { unreachable!("request id") };
                            let mut active = dim_agent_types::ActiveOutput { request_id: id.as_str().expect("request id").into(), text_characters: 0, thinking_characters: 0 };
                            for (kind, count) in [("step/text", &mut active.text_characters), ("step/thinking", &mut active.thinking_characters)] {
                                request.query.filter = Some(QueryFilter::And { conditions: vec![
                                    QueryFilter::In { value: QueryOperand::Field { path: "kind".into() }, values: vec![json!(kind)] },
                                    QueryFilter::In { value: QueryOperand::Field { path: "data.request_id".into() }, values: vec![id.clone()] },
                                ] });
                                request.query.select = QuerySelect::Characters { field: "data.delta".into() };
                                request.query.pick = None;
                                request.page = None;
                                let sum = tokio::select! {
                                    _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                                    result = tree.query(p.branch_id.clone(), request.clone()) => result?,
                                };
                                let QueryOutput::Count { count: total } = sum.result else { unreachable!("character total") };
                                *count = total;
                            }
                            output = Some(active);
                        }
                    }
                    Ok(json!({"branchId":p.branch_id,"through":result.through,"turn":turn,"output":output}))
                })
            }))?;
            Ok(Effect::Done)
        })
    }
}
