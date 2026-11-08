//! Goal's Session part (design §1, A.1, A.2): the `goal_update` tool, the
//! `session.goal.*` methods and the guide entry. It writes changes by branch
//! id through the branch part's Owner, which composes their records, and never
//! reads Goal's state.

use crate::branch::{Change, Changes};
use crate::text::{self, OBJECTIVE_LIMIT, SUMMARY_LIMIT};
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, reply},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    timeline_tree::TreeCommands,
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::Names;
use dim_agent_types::{ToolCall, ToolDefinition, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct GoalSession {
    tree: Key<TreeCommands>,
    tools: Key<ToolRegistry>,
    methods: Key<SessionMethods>,
    guide: Key<GuideEntries>,
    changes: Changes,
}

impl GoalSession {
    pub fn new(names: &Names<'_>, changes: Changes) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
            methods: names.key()?,
            guide: names.key()?,
            changes,
        })
    }
}

impl DynPlugin for GoalSession {
    fn name(&self) -> &str {
        "GoalSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.tree.erase(),
            self.tools.erase(),
            self.methods.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let tools = ctx.require_at(&self.tools)?;
            let methods = ctx.require_at(&self.methods)?;
            let guide = ctx.require_at(&self.guide)?;
            guide.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-goal".into()],
                    order: 55,
                    text: text::ENTRY.into(),
                },
            )?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: ToolDefinition {
                        name: "goal_update".into(),
                        description: text::TOOL_DESCRIPTION.into(),
                        input_schema: serde_json::from_value(text::tool_schema())
                            .expect("the goal_update schema is valid"),
                        source: ToolSource::Extension,
                        risk: Some("session".into()),
                        metadata: None,
                        file_changes: None,
                    },
                    handler: Arc::new(GoalUpdate {
                        changes: self.changes.clone(),
                    }),
                },
            )?;
            let owner = Arc::new(Owner {
                tree,
                changes: self.changes.clone(),
            });
            methods.register(
                ctx,
                "session.goal",
                ["set", "edit", "pause", "resume", "clear"]
                    .into_iter()
                    .map(|name| {
                        MethodDecl::new(&format!("session.goal.{name}"), true, Interruption::Report)
                    })
                    .collect(),
                Arc::new(move |call, _cancel, _progress| {
                    let MethodCall { method, params, .. } = call;
                    let owner = owner.clone();
                    Box::pin(async move { owner.call(&method, params).await })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

struct Owner {
    tree: Arc<TreeCommands>,
    changes: Changes,
}

impl Owner {
    /// A change method asks the branch part for the batch (record, trigger)
    /// and replies with the trigger's sequence; it validates only its own
    /// parameters and never reads the fold.
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let branch = branch_of(&params)?;
        let op = method
            .strip_prefix("session.goal.")
            .ok_or_else(|| RpcError::invalid(format!("unknown method {method}")))?;
        let change = match op {
            "set" | "edit" => {
                let objective =
                    text_value(params.get("objective"), OBJECTIVE_LIMIT).ok_or_else(|| {
                        RpcError::invalid(format!(
                            "objective must be 1 to {OBJECTIVE_LIMIT} characters"
                        ))
                    })?;
                if op == "edit" {
                    Change::Edit { objective }
                } else {
                    let judge = match params.get("judge") {
                        None | Some(Value::Null) => text::Judge::Model,
                        Some(Value::String(judge)) => match judge.as_str() {
                            "model" => text::Judge::Model,
                            "evaluator" => text::Judge::Evaluator,
                            _ => {
                                return Err(RpcError::invalid(
                                    "judge must be \"model\" or \"evaluator\"",
                                ));
                            }
                        },
                        Some(_) => {
                            return Err(RpcError::invalid(
                                "judge must be \"model\" or \"evaluator\"",
                            ));
                        }
                    };
                    Change::Set { objective, judge }
                }
            }
            "pause" => Change::Pause,
            "resume" => Change::Resume,
            "clear" => Change::Clear,
            _ => return Err(RpcError::invalid(format!("unknown method {method}"))),
        };
        // The branch is checked through the tree first, so a missing or ended
        // branch answers as it does to every other write.
        self.tree.get(&branch).map_err(RpcError::from)?;
        let sequence = self
            .changes
            .change(&branch, change)
            .await
            .map_err(RpcError::from)?;
        reply(json!({ "sequence": sequence }))
    }
}

/// The branch a change names: `main` when the call omits `branchId`, else a
/// non-empty string. A present `null` is not an omitted member.
fn branch_of(params: &Value) -> Result<String, RpcError> {
    match params.get("branchId") {
        None => Ok("main".to_owned()),
        Some(Value::String(branch)) if !branch.is_empty() => Ok(branch.clone()),
        Some(_) => Err(RpcError::invalid("branchId must be a non-empty string")),
    }
}

fn text_value(value: Option<&Value>, limit: usize) -> Option<String> {
    let value = value?.as_str()?;
    let length = value.chars().count();
    (length > 0 && value.trim() != "" && length <= limit).then(|| value.to_owned())
}

/// The `goal_update` tool (A.1): validate the parameters, ask the branch part
/// for the (record, trigger) batch on the calling branch, and confirm the
/// write.
struct GoalUpdate {
    changes: Changes,
}

impl ToolHandler for GoalUpdate {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let status = call.input.get("status").and_then(Value::as_str);
            let summary = text_value(call.input.get("summary"), SUMMARY_LIMIT);
            let (Some(status), Some(summary)) = (status, summary) else {
                return Ok(ToolResult::error(text::TOOL_INVALID));
            };
            if status != "completed" && status != "blocked" {
                return Ok(ToolResult::error(text::TOOL_INVALID));
            }
            let change = Change::Report {
                completed: status == "completed",
                summary,
            };
            match self.changes.change(&context.branch_id, change).await {
                Ok(_) => Ok(ToolResult::text(format!("goal_update recorded: {status}."))),
                Err(error) => Ok(ToolResult::error(error.to_string())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::branch_of;
    use serde_json::json;

    #[test]
    fn a_change_names_main_only_when_branch_id_is_absent() {
        assert_eq!(branch_of(&json!({})).unwrap(), "main");
        assert_eq!(branch_of(&json!({"branchId": "line"})).unwrap(), "line");
        for params in [
            json!({"branchId": null}),
            json!({"branchId": ""}),
            json!({"branchId": 7}),
            json!({"branchId": ["main"]}),
        ] {
            let error = branch_of(&params).expect_err("a non-string branchId is refused");
            assert_eq!(
                error.data.as_ref().map(|data| data.app_error.as_str()),
                Some("invalid_params"),
                "{params}"
            );
        }
    }
}
