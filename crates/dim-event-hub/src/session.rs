//! The Session part: the guide entry for the markers EventHub writes and the
//! `events` tool (design A.4, A.6).
use crate::{subscriptions::EventSubscriptions, text::EventHubTexts};
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::{Names, SessionIdentity};
use dim_agent_types::{InputMode, ToolCall, ToolDefinition, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Refusals of a call the generation stopped before making (design A.4).
const UNLOADING_BEFORE_CALL: &str = "tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.";
/// The refusal of a call cancelled before it was made (design A.4).
const CANCELLED_BEFORE_HANDOVER: &str = "events: cancelled before handover";
/// The refusal of a call the generation may have made (design A.4).
const UNLOADING_AFTER_CALL: &str = "tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.";

/// The `events` tool declaration (design A.4). The definition's risk is
/// `session`; the two configuration actions override it per call.
fn declaration(description: &str) -> ToolDefinition {
    let mut tool = definition(
        "events",
        description,
        json!({
            "type": "object",
            "required": ["action"],
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["sources", "subscribe", "unsubscribe", "list"]
                },
                "types": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "maxItems": 16
                },
                "match": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "maxProperties": 16
                },
                "mode": {"type": "string", "enum": ["queue", "steer", "append"]},
                "branchId": {"type": "string"},
                "subscriptionId": {"type": "string"}
            }
        }),
    );
    tool.source = ToolSource::Extension;
    tool.risk = Some("session".into());
    tool
}

/// The model-facing arguments of `events`, exactly as the declaration reads
/// them (design A.4).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsArgs {
    action: Action,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default, rename = "match")]
    matches: Option<BTreeMap<String, String>>,
    #[serde(default)]
    mode: Option<InputMode>,
    #[serde(default)]
    branch_id: Option<String>,
    #[serde(default)]
    subscription_id: Option<String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Action {
    Sources,
    Subscribe,
    Unsubscribe,
    List,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Sources => "sources",
            Self::Subscribe => "subscribe",
            Self::Unsubscribe => "unsubscribe",
            Self::List => "list",
        }
    }
}

impl EventsArgs {
    /// Refuses an argument the action does not take (design A.4).
    fn only(&self, allowed: &[&str]) -> Result<(), crate::EventHubError> {
        let present = [
            ("types", self.types.is_some()),
            ("match", self.matches.is_some()),
            ("mode", self.mode.is_some()),
            ("branchId", self.branch_id.is_some()),
            ("subscriptionId", self.subscription_id.is_some()),
        ];
        let extra: Vec<&str> = present
            .into_iter()
            .filter(|(name, present)| *present && !allowed.contains(name))
            .map(|(name, _)| name)
            .collect();
        if extra.is_empty() {
            return Ok(());
        }
        Err(crate::EventHubError::invalid_params(format!(
            "{} does not take {}",
            self.action.name(),
            extra.join(", ")
        )))
    }
}

pub struct EventHubSession {
    guide: Key<GuideEntries>,
    tools: Key<ToolRegistry>,
    identity: Key<SessionIdentity>,
    /// The process part's subscription core: injecting it means the tool
    /// exists exactly while that part is active (design §1, §6).
    subscriptions: Key<EventSubscriptions>,
    /// The texts the row's factory configuration carries, the product's
    /// [`ENTRY`](crate::text::ENTRY) and `TOOL` by default (design A.4, A.6).
    texts: Arc<EventHubTexts>,
}

impl EventHubSession {
    pub fn new(names: &Names<'_>, texts: Arc<EventHubTexts>) -> rutis::Result<Self> {
        Ok(Self {
            guide: names.key()?,
            tools: names.key()?,
            identity: names.key()?,
            subscriptions: names.key()?,
            texts,
        })
    }
}

impl DynPlugin for EventHubSession {
    fn name(&self) -> &str {
        "EventHubSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.guide.erase(),
            self.tools.erase(),
            self.identity.erase(),
            self.subscriptions.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            ctx.require_at(&self.guide)?.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-event".into(), "dim-event-guide".into()],
                    order: 130,
                    text: self.texts.entry.clone(),
                },
            )?;
            ctx.require_at(&self.tools)?.register(
                ctx,
                ToolContribution {
                    definition: declaration(&self.texts.tool),
                    handler: Arc::new(EventsTool {
                        subscriptions: ctx.require_at(&self.subscriptions)?,
                        session_id: identity.session_id.clone(),
                        closing: ctx.cancellation_token(),
                    }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct EventsTool {
    subscriptions: Arc<EventSubscriptions>,
    /// The caller's own Session (design A.4): the target never names another.
    session_id: String,
    /// Cancelled when this generation starts to unload.
    closing: CancellationToken,
}

impl EventsTool {
    fn sources(&self, args: &EventsArgs) -> Result<Value, crate::EventHubError> {
        args.only(&[])?;
        Ok(self.subscriptions.sources())
    }

    async fn subscribe(
        &self,
        args: &EventsArgs,
        branch_id: &str,
    ) -> Result<Value, crate::EventHubError> {
        args.only(&["types", "match", "mode", "branchId"])?;
        self.subscriptions
            .subscribe(crate::SubscriptionCall {
                session_id: self.session_id.clone(),
                types: args.types.clone().unwrap_or_default(),
                matches: args.matches.clone(),
                mode: args.mode,
                branch_id: Some(branch_id.to_owned()),
            })
            .await
    }

    async fn unsubscribe(&self, args: &EventsArgs) -> Result<Value, crate::EventHubError> {
        args.only(&["subscriptionId"])?;
        let Some(subscription_id) = args.subscription_id.as_deref() else {
            return Err(crate::EventHubError::invalid_params(
                "unsubscribe needs a subscriptionId",
            ));
        };
        self.subscriptions
            .unsubscribe(crate::UnsubscribeCall {
                session_id: self.session_id.clone(),
                subscription_id: subscription_id.to_owned(),
            })
            .await
    }

    fn list(&self, args: &EventsArgs) -> Result<Value, crate::EventHubError> {
        args.only(&[])?;
        Ok(self.subscriptions.list(crate::ListCall {
            session_id: self.session_id.clone(),
        }))
    }
}

impl ToolHandler for EventsTool {
    fn risk(&self, call: &ToolCall) -> Option<&'static str> {
        // Changing where future external text enters a Session is
        // configuration (design §7).
        matches!(
            call.input.get("action").and_then(Value::as_str),
            Some("subscribe" | "unsubscribe")
        )
        .then_some("persistent_configuration")
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            let args: EventsArgs = match serde_json::from_value(Value::Object(call.input)) {
                Ok(args) => args,
                Err(error) => {
                    return Ok(refusal(crate::EventHubError::invalid_params(
                        error.to_string(),
                    )));
                }
            };
            // A call cancelled after it was parsed and before it was made is
            // refused, as Scheduler's tool is (design A.4).
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if cancel.is_cancelled() {
                return Ok(ToolResult::error(CANCELLED_BEFORE_HANDOVER));
            }
            let outcome = match args.action {
                Action::Sources => self.sources(&args),
                Action::Subscribe => {
                    // The branch is the caller's own unless `branchId` names
                    // another of its Session's branches (design A.4).
                    let branch = args
                        .branch_id
                        .clone()
                        .unwrap_or_else(|| context.branch_id.clone());
                    self.subscribe(&args, &branch).await
                }
                Action::Unsubscribe => self.unsubscribe(&args).await,
                Action::List => self.list(&args),
            };
            // The call may have taken effect before the generation unloaded,
            // so the result says so and the model checks before repeating it
            // (design A.4).
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_AFTER_CALL));
            }
            Ok(match outcome {
                Ok(reply) => ToolResult::text(reply.to_string()),
                Err(error) => refusal(error),
            })
        })
    }
}

/// A refusal's first line is `<code>: <message>` (design A.4).
fn refusal(error: crate::EventHubError) -> ToolResult {
    ToolResult::error(format!("{}: {}", error.code, error.message))
}
