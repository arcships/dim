//! The `recall` tool: the branch's history told to the model as an account of
//! what happened. Design: `docs/design/plugins/Recall.md` §3 (projection), §4
//! (handles), §5 (tool contract) and §6 (what the model sees). This part's
//! page is `docs/architecture/rust-agent/plugins/Recall.md`.
//!
//! The tool reads the published history once per call and folds it into
//! *things*: one origin event and the later events a join id attaches to it.
//! It tells the kinds §3.2 names and nothing else, and counts with the rules
//! context assembly uses: an input counts once it is consumed, a reply counts
//! when its output completed or as the partial text it left, and a tool result
//! is told as its `content`.
use crate::{
    context_policy::check_cancel,
    host::{HostError, HostFuture, ToolExecutionContext},
    timeline_tree::TreeCommands,
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use chrono::{DateTime, TimeZone, Utc};
use dim_agent_runtime::{AgentBuilder, AgentError, CAPABILITY_RELOADING, Feature, Names};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
#[cfg(feature = "testing")]
use std::collections::HashSet;
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

pub const RECALL_TOOL: &str = "recall";
/// Entries a page of the turn index, a search or one turn holds (§5.3).
const PAGE_ENTRIES: usize = 20;
/// Characters a page of one thing in full holds (§5.3).
const PAGE_CHARACTERS: usize = 8_000;
/// Characters a variable part of a line keeps, its `…` included (§3.4).
const PART: usize = 120;
/// Longest `about` the tool accepts (§5.2).
const ABOUT: usize = 200;

/// The Recall plugin: the Session tool.
pub fn feature() -> Feature {
    Feature::new("Recall").session("Recall", |names| Ok(Box::new(RecallPlugin::new(names)?)))
}

/// Installs Recall in the profile, beside `wait`.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(feature());
}

fn recall_definition() -> ToolDefinition {
    let mut definition = definition(
        RECALL_TOOL,
        "Recall what happened earlier in this conversation, including what a compaction summarized: what the user said, what you did and what came back. With no arguments it lists past turns; open a turn or anything in it by its id, or search with about. A shortened input that ends in [input <id>] is read in full by opening that id.",
        json!({
            "type": "object",
            "properties": {
                "about": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": ABOUT,
                    "description": "Words to look for; all must appear, in any order, ignoring case."
                },
                "tool": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Only calls to this tool."
                },
                "before": {"type": "string", "description": "An id; show what came before it."},
                "open": {
                    "type": "string",
                    "description": "An id; show that turn, or that thing in full."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "With open: continue from here, a line of a turn or a character of one thing."
                }
            },
            "additionalProperties": false
        }),
    );
    definition.source = ToolSource::Extension;
    definition.risk = Some("session".into());
    definition
}

pub struct RecallPlugin {
    tree: Key<TreeCommands>,
    tools: Key<ToolRegistry>,
}

impl RecallPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
        })
    }
}

impl DynPlugin for RecallPlugin {
    fn name(&self) -> &str {
        "Recall"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let tools = ctx.require_at(&self.tools)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: recall_definition(),
                    handler: Arc::new(RecallTool(tree)),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct RecallTool(Arc<TreeCommands>);

impl RecallTool {
    fn facade_error(error: AgentError) -> HostError {
        match error {
            AgentError::Reloading(_) => HostError::Cancelled(CAPABILITY_RELOADING.into()),
            error => HostError::failed(error.to_string()),
        }
    }

    async fn run(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let branch = self.0.get(&context.branch_id).map_err(Self::facade_error)?;
        let head = branch.sequence();
        let events = branch
            .history(Some(head))
            .await
            .map_err(Self::facade_error)?;
        check_cancel(&cancel)?;
        Ok(Narrative::new(&events).answer(&context.turn_id, &call.input))
    }
}

impl ToolHandler for RecallTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(self.run(call, context, cancel))
    }
}

/// What a thing is (§3.2), with what its later events fold into it.
enum Kind<'a> {
    Input {
        input: &'a InputMessageData,
        /// Per block: the latest block of its key, a retiring one aside (§3.5).
        live: Vec<bool>,
        /// A model step of the input's turn had started when it was consumed;
        /// a steer that wakes an idle line is consumed before any (§3.4).
        working: bool,
    },
    Reply {
        text: String,
        state: ReplyState,
    },
    Call(Box<Call>),
    Interrupt,
    Permission {
        mode: String,
    },
    Compaction {
        current: bool,
    },
    Fork {
        parent: &'a str,
    },
    SessionFork,
}

enum ReplyState {
    /// The pinned history ends inside this output.
    Writing,
    Completed,
    /// The output ended failed or cancelled, with the reason it carries.
    CutOff(String),
}

/// One tool call and the events that joined it, folded into the ending of §3.4.
#[derive(Default)]
struct Call {
    name: String,
    /// The complete arguments, when `step/tool-use` arrived.
    input: Option<JsonObject>,
    approval: Option<Approval>,
    result: Option<Outcome>,
    exec: Option<Exec>,
    sub_agent: Option<SubAgent>,
}

enum Approval {
    Denied,
    Cancelled(String),
}

struct Outcome {
    time: u64,
    text: String,
    error: bool,
    /// A result that closed a call never dispatched (`call_event_id` absent).
    never_dispatched: bool,
}

struct Exec {
    task: String,
    ended: Option<(u64, ExecStatus, String)>,
}

struct SubAgent {
    agent: String,
    outcome: CollaborationOutcome,
    text: String,
}

/// One origin event and the events joined to it.
struct Thing<'a> {
    /// Index of the origin event in the pinned history; its id is the handle.
    origin: usize,
    /// Position the thing is ordered and attributed by: the origin event, or
    /// for an input the `input/consumed` that made it count.
    anchor: usize,
    kind: Kind<'a>,
}

struct Turn<'a> {
    /// Index of `turn/start`.
    start: usize,
    id: &'a str,
    turn_id: &'a str,
    outcome: Option<OutcomeText>,
}

#[derive(Clone, Copy)]
enum OutcomeText {
    Completed,
    Failed,
    Cancelled,
}

impl OutcomeText {
    fn text(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A level-1 entry: a turn, or a thing outside any turn.
#[derive(Clone, Copy)]
enum Entry {
    Turn(usize),
    Thing(usize),
}

/// A rendered line of a page, before its date line.
struct Line<'a> {
    time: u64,
    id: &'a str,
    text: String,
}

/// The call arguments of §5.2.
struct Arguments {
    about: Option<String>,
    tool: Option<String>,
    before: Option<String>,
    open: Option<String>,
    offset: Option<usize>,
}

impl Arguments {
    fn parse(input: &JsonObject) -> Result<Self, String> {
        let mut arguments = Self {
            about: None,
            tool: None,
            before: None,
            open: None,
            offset: None,
        };
        for (field, value) in input {
            match field.as_str() {
                "about" => arguments.about = Some(string(field, value, Some(ABOUT))?),
                "tool" => arguments.tool = Some(string(field, value, None)?),
                "before" => arguments.before = Some(string(field, value, None)?),
                "open" => arguments.open = Some(string(field, value, None)?),
                "offset" => {
                    let Some(offset) = value.as_u64() else {
                        return Err("recall: offset must be a non-negative integer.".into());
                    };
                    arguments.offset = Some(usize::try_from(offset).unwrap_or(usize::MAX));
                }
                _ => return Err(format!("recall: {} must be a known argument.", part(field))),
            }
        }
        if arguments.open.is_some()
            && (arguments.about.is_some() || arguments.tool.is_some() || arguments.before.is_some())
        {
            return Err(
                "recall: open shows one turn or thing; drop about, tool and before.".into(),
            );
        }
        if arguments.offset.is_some() && arguments.open.is_none() {
            return Err("recall: offset needs open.".into());
        }
        Ok(arguments)
    }
}

fn string(field: &str, value: &Value, limit: Option<usize>) -> Result<String, String> {
    let Some(text) = value.as_str() else {
        return Err(format!("recall: {field} must be a string."));
    };
    if text.trim().is_empty() {
        return Err(format!("recall: {field} must be a non-empty string."));
    }
    if let Some(limit) = limit
        && text.chars().count() > limit
    {
        return Err(format!(
            "recall: {field} must be at most {limit} characters."
        ));
    }
    Ok(text.to_owned())
}

/// The pinned history of one branch folded into the narrative of §3. The flow
/// tests reach it as [`testing::Narrative`](testing).
struct Narrative<'a> {
    events: &'a [Arc<AgentEvent>],
    things: Vec<Thing<'a>>,
    /// The turn each thing belongs to; `None` for a thing between turns.
    owners: Vec<Option<usize>>,
    turns: Vec<Turn<'a>>,
    /// Level-1 entries, earliest first.
    top: Vec<Entry>,
    ids: HashMap<&'a str, Entry>,
    /// The events this narrative tells; only the flow tests read it.
    told: Told<'a>,
    /// Position of this branch's own `branch/start`; the end of the history
    /// when the pinned list holds none.
    own_start: usize,
}

/// The events a narrative tells: each thing's origin and its joined events.
/// Feature `testing` only; a production fold pays nothing to build it.
#[cfg(feature = "testing")]
type Told<'a> = HashSet<&'a str>;
#[cfg(not(feature = "testing"))]
type Told<'a> = std::marker::PhantomData<&'a ()>;

/// Records one told event. Without `testing` there is no set to record in.
fn tell<'a>(told: &mut Told<'a>, id: &'a str) {
    #[cfg(feature = "testing")]
    told.insert(id);
    #[cfg(not(feature = "testing"))]
    let _ = (told, id);
}

impl<'a> Narrative<'a> {
    /// Folds a pinned branch history into things and turns.
    fn new(events: &'a [Arc<AgentEvent>]) -> Self {
        let own_start = events
            .iter()
            .rposition(|event| matches!(event.kind, EventKind::BranchStart(_)))
            .unwrap_or(events.len());
        let last_block = last_blocks(events);
        let mut things: Vec<Thing<'a>> = Vec::new();
        // The turn each thing is told inside, pushed beside it.
        let mut owners: Vec<Option<usize>> = Vec::new();
        let mut turns: Vec<Turn<'a>> = Vec::new();
        let mut told = Told::default();
        // The turn the fold is inside; its end is not written yet.
        let mut open_turn: Option<usize> = None;
        // A model step of the open turn has arrived, so an input consumed now
        // was read while the line was working (§3.4).
        let mut working = false;
        let mut inputs: HashMap<&'a str, (usize, &'a InputMessageData)> = HashMap::new();
        let mut replies: HashMap<&'a str, usize> = HashMap::new();
        let mut calls: HashMap<&'a str, usize> = HashMap::new();
        let mut approvals: HashMap<&'a str, usize> = HashMap::new();
        let mut tasks: HashMap<&'a str, usize> = HashMap::new();
        let mut collaborations: HashMap<&'a str, usize> = HashMap::new();
        let mut summary = None;

        for (index, event) in events.iter().enumerate() {
            let id = event.id.as_str();
            let mut joined = false;
            match &event.kind {
                EventKind::InputMessage(data) => {
                    inputs.insert(id, (index, data));
                }
                EventKind::InputConsumed(data) => {
                    if let Some((origin, input)) = inputs.get(data.input_id.as_str()) {
                        joined = true;
                        things.push(Thing {
                            origin: *origin,
                            anchor: index,
                            kind: Kind::Input {
                                input,
                                live: live_blocks(input, *origin, &last_block),
                                working,
                            },
                        });
                        owners.push(open_turn);
                        tell(&mut told, id);
                    }
                }
                EventKind::TurnStart(data) => {
                    turns.push(Turn {
                        start: index,
                        id,
                        turn_id: data.turn_id.as_str(),
                        outcome: None,
                    });
                    open_turn = Some(turns.len() - 1);
                    working = false;
                }
                EventKind::TurnEnd(data) => {
                    if let Some(position) = turns
                        .iter()
                        .position(|turn| turn.turn_id == data.turn_id && turn.outcome.is_none())
                    {
                        turns[position].outcome = Some(match &data.outcome {
                            TurnOutcome::Completed { .. } => OutcomeText::Completed,
                            TurnOutcome::Failed { .. } => OutcomeText::Failed,
                            TurnOutcome::Cancelled { .. } => OutcomeText::Cancelled,
                        });
                        if open_turn == Some(position) {
                            open_turn = None;
                        }
                    }
                }
                EventKind::ModelStart(data) => {
                    working = true;
                    replies.insert(data.request_id.as_str(), things.len());
                    things.push(Thing {
                        origin: index,
                        anchor: index,
                        kind: Kind::Reply {
                            text: String::new(),
                            state: ReplyState::Writing,
                        },
                    });
                    owners.push(open_turn);
                }
                EventKind::Text(data) => {
                    if let Some(thing) = replies
                        .get(data.request_id.as_str())
                        .and_then(|position| things.get_mut(*position))
                        && let Kind::Reply { text, .. } = &mut thing.kind
                    {
                        text.push_str(&data.delta);
                        tell(&mut told, id);
                    }
                }
                EventKind::ModelContent(data) => {
                    if let Some(thing) = replies
                        .get(data.request_id.as_str())
                        .and_then(|position| things.get_mut(*position))
                        && let Kind::Reply { text, .. } = &mut thing.kind
                    {
                        *text = content_text(&data.content);
                        tell(&mut told, id);
                    }
                }
                EventKind::ModelOutputEnd(data) => {
                    if let Some(thing) = replies
                        .get(data.request_id.as_str())
                        .and_then(|position| things.get_mut(*position))
                        && let Kind::Reply { state, .. } = &mut thing.kind
                    {
                        *state = match &data.outcome {
                            ModelOutputOutcome::Completed { .. } => ReplyState::Completed,
                            ModelOutputOutcome::Failed { error, .. } => {
                                ReplyState::CutOff(format!("failed: {error}"))
                            }
                            ModelOutputOutcome::Cancelled { reason } => {
                                ReplyState::CutOff(format!("cancelled: {reason}"))
                            }
                        };
                        tell(&mut told, id);
                    }
                }
                EventKind::ToolUseStart(data) => {
                    calls.insert(data.call_id.as_str(), things.len());
                    things.push(Thing {
                        origin: index,
                        anchor: index,
                        kind: Kind::Call(Box::new(Call {
                            name: data.name.clone(),
                            ..Call::default()
                        })),
                    });
                    owners.push(open_turn);
                }
                EventKind::ToolUse(data) => {
                    if let Some(call) = calls
                        .get(data.call.id.as_str())
                        .and_then(|position| call_at(&mut things, *position))
                    {
                        call.input = Some(data.call.input.clone());
                        joined = true;
                    }
                }
                EventKind::ToolResult(data) => {
                    for item in &data.results {
                        let Some(position) = calls.get(item.call_id.as_str()).copied() else {
                            continue;
                        };
                        if let Some(call) = call_at(&mut things, position) {
                            call.result = Some(Outcome {
                                time: event.create_at,
                                text: result_text(&item.result),
                                error: item.result.is_error.unwrap_or(false),
                                never_dispatched: data.call_event_id.is_none(),
                            });
                            joined = true;
                        }
                    }
                }
                EventKind::ApprovalRequest(data) => {
                    if let BranchSource::Tool { call_id, .. } = &data.source
                        && let Some(position) = calls.get(call_id.as_str()).copied()
                    {
                        approvals.insert(data.approval_id.as_str(), position);
                        joined = true;
                    }
                }
                EventKind::ApprovalRespond(data) => {
                    if let Some(position) = approvals.get(data.approval_id.as_str()).copied() {
                        if let Some(call) = call_at(&mut things, position) {
                            call.approval = match data.decision {
                                ApprovalDecision::Deny => Some(Approval::Denied),
                                ApprovalDecision::Allow => None,
                            };
                        }
                        joined = true;
                    }
                }
                EventKind::ApprovalCancelled(data) => {
                    if let Some(position) = approvals.get(data.approval_id.as_str()).copied() {
                        if let Some(call) = call_at(&mut things, position) {
                            call.approval = Some(Approval::Cancelled(data.reason.clone()));
                        }
                        joined = true;
                    }
                }
                EventKind::ExecStarted(data) => {
                    if let Some(position) = calls.get(data.call_id.as_str()).copied() {
                        if let Some(call) = call_at(&mut things, position) {
                            call.exec = Some(Exec {
                                task: data.task_id.clone(),
                                ended: None,
                            });
                        }
                        tasks.insert(data.task_id.as_str(), position);
                        joined = true;
                    }
                }
                EventKind::ExecEnded(data) => {
                    if let Some(position) = tasks.get(data.task_id.as_str()).copied() {
                        if let Some(call) = call_at(&mut things, position)
                            && let Some(exec) = &mut call.exec
                        {
                            exec.ended =
                                Some((event.create_at, data.status, result_text(&data.result)));
                        }
                        joined = true;
                    }
                }
                EventKind::CollaborationRequest(data) => {
                    // The request returns at once and the member settles later:
                    // only `collaboration/result` gives the call its ending.
                    if let Some(position) = calls.get(data.call_id.as_str()).copied() {
                        collaborations.insert(id, position);
                        joined = true;
                    }
                }
                EventKind::CollaborationMessage(data) => {
                    joined = calls.contains_key(data.call_id.as_str());
                }
                EventKind::CollaborationResult(data) => {
                    if let Some(position) =
                        collaborations.get(data.request_event_id.as_str()).copied()
                    {
                        if let Some(call) = call_at(&mut things, position) {
                            call.sub_agent = Some(SubAgent {
                                agent: data.agent_id.clone(),
                                outcome: data.outcome.clone(),
                                text: data.text.clone(),
                            });
                        }
                        joined = true;
                    }
                }
                EventKind::BranchCreate(data) => {
                    if let BranchSource::Tool { call_id, .. } = &data.source {
                        joined = calls.contains_key(call_id.as_str());
                    }
                }
                EventKind::StopInterrupt(_) => {
                    things.push(Thing {
                        origin: index,
                        anchor: index,
                        kind: Kind::Interrupt,
                    });
                    owners.push(open_turn);
                    tell(&mut told, id);
                }
                EventKind::PermissionChanged(data) => {
                    things.push(Thing {
                        origin: index,
                        anchor: index,
                        kind: Kind::Permission {
                            mode: if data.policy.file_access.is_none()
                                && data.policy.tool_approval.is_none()
                            {
                                preset(&data.policy.preset).into()
                            } else {
                                let files = match data.policy.file_access() {
                                    FileAccess::ReadOnly => "read-only",
                                    FileAccess::WorkspaceReadWrite => "workspace-read-write",
                                    FileAccess::Unrestricted => "unrestricted",
                                };
                                let tools = match data.policy.tool_approval {
                                    Some(ToolApproval::Ask) => "ask",
                                    Some(ToolApproval::Bypass) => "bypass",
                                    None => preset(&data.policy.preset),
                                };
                                let legacy = if data.policy.tool_approval.is_none() {
                                    "legacy:"
                                } else {
                                    ""
                                };
                                format!("files={files}, tools={legacy}{tools}")
                            },
                        },
                    });
                    owners.push(open_turn);
                    tell(&mut told, id);
                }
                EventKind::ContextCompaction(data) => {
                    if matches!(data.outcome, ContextCompactionResult::Completed { .. }) {
                        summary = Some(things.len());
                        things.push(Thing {
                            origin: index,
                            anchor: index,
                            kind: Kind::Compaction { current: false },
                        });
                        owners.push(open_turn);
                    }
                }
                EventKind::BranchStart(data) => {
                    // A fork point cuts the turn it forks away from (§3.6).
                    open_turn = None;
                    if index == own_start
                        && let Some(parent) = &data.parent
                    {
                        things.push(Thing {
                            origin: index,
                            anchor: index,
                            kind: Kind::Fork { parent },
                        });
                        owners.push(None);
                    }
                }
                EventKind::SessionForked(_) => {
                    things.push(Thing {
                        origin: index,
                        anchor: index,
                        kind: Kind::SessionFork,
                    });
                    owners.push(open_turn);
                    tell(&mut told, id);
                }
                _ => {}
            }
            if joined {
                tell(&mut told, id);
            }
        }
        if let Some(position) = summary
            && let Kind::Compaction { current } = &mut things[position].kind
        {
            *current = true;
        }
        // A request whose folded text is empty (only calls, or only thinking)
        // is no reply line; its calls are told on their own (§3.2).
        let (things, owners): (Vec<Thing<'a>>, Vec<Option<usize>>) = things
            .into_iter()
            .zip(owners)
            .filter(|(thing, _)| {
                !matches!(&thing.kind, Kind::Reply { text, .. } if text.trim().is_empty())
            })
            .unzip();
        for thing in &things {
            let origin = events[thing.origin].id.as_str();
            tell(&mut told, origin);
        }
        for turn in &turns {
            tell(&mut told, turn.id);
        }
        let mut top: Vec<Entry> = turns
            .iter()
            .enumerate()
            .map(|(index, _)| Entry::Turn(index))
            .chain(
                things
                    .iter()
                    .enumerate()
                    .zip(&owners)
                    .filter(|(_, owner)| owner.is_none())
                    .map(|((index, _), _)| Entry::Thing(index)),
            )
            .collect();
        let mut ids: HashMap<&'a str, Entry> = HashMap::new();
        for (index, turn) in turns.iter().enumerate() {
            ids.insert(turn.id, Entry::Turn(index));
        }
        for (index, thing) in things.iter().enumerate() {
            ids.insert(events[thing.origin].id.as_str(), Entry::Thing(index));
        }
        top.sort_by_key(|entry| entry_anchor(&turns, &things, *entry));

        Self {
            events,
            things,
            owners,
            turns,
            top,
            ids,
            told,
            own_start,
        }
    }

    /// The tool's answer for `input`, called from `calling_turn`.
    fn answer(&self, calling_turn: &str, input: &JsonObject) -> ToolResult {
        let arguments = match Arguments::parse(input) {
            Ok(arguments) => arguments,
            Err(message) => return ToolResult::error(message),
        };
        if let Some(open) = &arguments.open {
            let Some(entry) = self.ids.get(open.as_str()).copied() else {
                return ToolResult::error(format!("No turn or line has id {}.", part(open)));
            };
            return match entry {
                Entry::Turn(index) => self.turn_page(index, arguments.offset.unwrap_or(0)),
                Entry::Thing(index) => self.thing_page(index, arguments.offset.unwrap_or(0)),
            };
        }
        let before = match &arguments.before {
            Some(before) => match self.ids.get(before.as_str()).copied() {
                Some(entry) => Some(entry_anchor(&self.turns, &self.things, entry)),
                None => {
                    return ToolResult::error(format!("No turn or line has id {}.", part(before)));
                }
            },
            None => None,
        };
        if arguments.about.is_some() || arguments.tool.is_some() {
            self.search(&arguments, before)
        } else {
            self.index(before, calling_turn)
        }
    }

    /// The turn index, latest first (§3.6).
    fn index(&self, before: Option<usize>, calling_turn: &str) -> ToolResult {
        let entries: Vec<Entry> = self
            .top
            .iter()
            .rev()
            .copied()
            .filter(|entry| {
                before.is_none_or(|before| entry_anchor(&self.turns, &self.things, *entry) < before)
            })
            .collect();
        let more = entries.len() > PAGE_ENTRIES;
        let lines: Vec<Line<'a>> = entries
            .iter()
            .take(PAGE_ENTRIES)
            .map(|entry| self.entry_line(*entry, calling_turn))
            .collect();
        let mut output = self.render(&lines);
        if more {
            let last = lines.last().expect("a full page");
            output.push(format!(
                "Earlier: before={}. Open one: open={}.",
                last.id, lines[0].id
            ));
        } else {
            output.push("Nothing earlier.".into());
        }
        let next = more.then(|| json!({"before": lines.last().expect("a full page").id}));
        page("turns", output, &ids_of(&lines), next)
    }

    /// The matching lines of every turn, latest first (§3.6).
    fn search(&self, arguments: &Arguments, before: Option<usize>) -> ToolResult {
        let about: Vec<String> = arguments
            .about
            .as_deref()
            .map(|about| about.split_whitespace().map(str::to_lowercase).collect())
            .unwrap_or_default();
        let matching: Vec<usize> = (0..self.things.len())
            .rev()
            .filter(|index| self.owners[*index].is_some())
            .filter(|index| before.is_none_or(|before| self.things[*index].anchor < before))
            .filter(|index| match &arguments.tool {
                Some(tool) => match &self.things[*index].kind {
                    Kind::Call(call) => call.name == *tool,
                    _ => false,
                },
                None => true,
            })
            .filter(|index| {
                let text = self.body(&self.things[*index]).to_lowercase();
                about.iter().all(|word| text.contains(word))
            })
            .collect();
        if matching.is_empty() {
            let last = if before.is_some() {
                "No more.".to_owned()
            } else {
                format!("Nothing matches {}.", filters(arguments))
            };
            return page("search", vec![last], &[], None);
        }
        let more = matching.len() > PAGE_ENTRIES;
        let lines: Vec<Line<'a>> = matching
            .iter()
            .take(PAGE_ENTRIES)
            .map(|index| self.thing_line(&self.things[*index]))
            .collect();
        let mut output = self.render(&lines);
        match (more, lines.last()) {
            (true, Some(last)) => output.push(format!("More: before={}.", last.id)),
            _ => output.push("No more.".into()),
        }
        let next = more.then(|| json!({"before": lines.last().expect("a full page").id}));
        page("search", output, &ids_of(&lines), next)
    }

    /// One turn, line by line, from entry `offset` (§3.6).
    fn turn_page(&self, index: usize, offset: usize) -> ToolResult {
        let turn = &self.turns[index];
        let lines: Vec<Line<'a>> = self
            .things
            .iter()
            .zip(&self.owners)
            .filter(|(_, owner)| **owner == Some(index))
            .map(|(thing, _)| self.thing_line(thing))
            .collect();
        if lines.is_empty() && offset == 0 {
            return page("turn", vec!["Nothing in this turn.".into()], &[], None);
        }
        if offset >= lines.len() {
            return ToolResult::error(format!("Turn {} has {} lines.", turn.id, lines.len()));
        }
        let end = (offset + PAGE_ENTRIES).min(lines.len());
        let shown = &lines[offset..end];
        let mut output = self.render(shown);
        let next = if end < lines.len() {
            output.push(format!("More of this turn: open={} offset={end}.", turn.id));
            Some(json!({"open": turn.id, "offset": end}))
        } else {
            output.push(format!("End of turn. Open one: open={}.", shown[0].id));
            None
        };
        page("turn", output, &ids_of(shown), next)
    }

    /// One thing in full, from character `offset` (§6).
    fn thing_page(&self, index: usize, offset: usize) -> ToolResult {
        let thing = &self.things[index];
        let id = self.events[thing.origin].id.as_str();
        let text = self.full(thing);
        let total = text.chars().count();
        if offset >= total {
            return ToolResult::error(format!("{id} has {total} characters."));
        }
        let stop = offset + PAGE_CHARACTERS;
        let start = text
            .char_indices()
            .nth(offset)
            .map_or(text.len(), |(at, _)| at);
        let end = if stop >= total {
            text.len()
        } else {
            text.char_indices()
                .nth(stop)
                .map_or(text.len(), |(at, _)| at)
        };
        let mut output = vec![text[start..end].to_owned()];
        let next = if stop < total {
            output.push(format!("Continue: open={id} offset={stop}."));
            Some(json!({"open": id, "offset": stop}))
        } else {
            output.push("End.".into());
            None
        };
        page("full", output, &[id], next)
    }

    fn render(&self, lines: &[Line<'a>]) -> Vec<String> {
        let mut output = Vec::new();
        let mut day = None;
        for line in lines {
            let date = date(line.time);
            if day.as_deref() != Some(date.as_str()) {
                output.push(format!("{date} (UTC)"));
                day = Some(date);
            }
            output.push(format!("{} {} {}", clock(line.time), line.id, line.text));
        }
        output
    }

    fn entry_line(&self, entry: Entry, calling_turn: &str) -> Line<'a> {
        match entry {
            Entry::Turn(index) => {
                let turn = &self.turns[index];
                Line {
                    time: self.events[turn.start].create_at,
                    id: turn.id,
                    text: self.turn_line(index, calling_turn),
                }
            }
            Entry::Thing(index) => self.thing_line(&self.things[index]),
        }
    }

    fn thing_line(&self, thing: &Thing<'a>) -> Line<'a> {
        Line {
            time: self.events[thing.origin].create_at,
            id: self.events[thing.origin].id.as_str(),
            text: self.line(thing),
        }
    }

    fn line(&self, thing: &Thing<'a>) -> String {
        match &thing.kind {
            Kind::Input {
                input,
                live,
                working,
            } => input_line(input, live, *working),
            Kind::Reply { text, state } => match state {
                ReplyState::Writing => format!("you (still writing): {}", part(text)),
                ReplyState::Completed => format!("you: {}", part(text)),
                ReplyState::CutOff(reason) => {
                    format!("your reply was cut off ({}): {}", part(reason), part(text))
                }
            },
            Kind::Call(call) => call_line(call, self.events[thing.origin].create_at),
            Kind::Interrupt => "the user interrupted the turn".into(),
            Kind::Permission { mode } => format!("permission mode changed to {mode}"),
            Kind::Compaction { current } => compaction_line(*current),
            Kind::Fork { parent } => format!("this line was opened by {}", part(parent)),
            Kind::SessionFork => {
                "this conversation was copied from another session up to here".into()
            }
        }
    }

    /// The turn's line in the index (§3.6).
    fn turn_line(&self, index: usize, calling_turn: &str) -> String {
        let turn = &self.turns[index];
        let mut parts = Vec::new();
        // The inputs that opened the turn: the ones it consumed before its
        // first model request, so a steer read later does not open it (§3.6).
        let opening: Vec<(&InputMessageData, &[bool])> = self
            .things
            .iter()
            .zip(&self.owners)
            .filter(|(_, owner)| **owner == Some(index))
            .filter_map(|(thing, _)| match &thing.kind {
                Kind::Input {
                    input,
                    live,
                    working: false,
                } => Some((*input, live.as_slice())),
                _ => None,
            })
            .collect();
        match opening
            .iter()
            .find(|(input, _)| input.user.is_some())
            .or_else(|| {
                opening
                    .iter()
                    .find(|(input, _)| line_write(input).is_some())
            })
            // An Append input joins the request but does not open the turn
            // (`AgentLoop::driven`), so it is no title: the driving input is
            // the first that is not one.
            .or_else(|| {
                opening
                    .iter()
                    .find(|(input, _)| input.effective_mode() != InputMode::Append)
            })
            .or_else(|| opening.first())
        {
            Some((input, live)) => parts.push(match (line_write(input), &input.user) {
                (Some(write), _) => format!("turn: {}", write_line(&write, false)),
                (None, Some(user)) => format!("turn: user \"{}\"", part(&user_text(user))),
                (None, None) => format!("turn: {}", part(&blocks_name(&input.blocks, live))),
            }),
            None => parts.push("turn: no opening input".into()),
        }
        let summary = self.tool_summary(index);
        if !summary.is_empty() {
            parts.push(format!("you called {}", part(&summary)));
        }
        parts.push(self.outcome_text(turn, calling_turn).into());
        parts.join(" — ")
    }

    /// The tools a turn called, each with its count, in call order.
    fn tool_summary(&self, index: usize) -> String {
        let mut tools: Vec<(&str, usize)> = Vec::new();
        for (thing, owner) in self.things.iter().zip(&self.owners) {
            if *owner != Some(index) {
                continue;
            }
            let Kind::Call(call) = &thing.kind else {
                continue;
            };
            match tools.iter_mut().find(|(name, _)| *name == call.name) {
                Some((_, count)) => *count += 1,
                None => tools.push((call.name.as_str(), 1)),
            }
        }
        tools
            .into_iter()
            .map(|(name, count)| {
                if count == 1 {
                    name.to_owned()
                } else {
                    format!("{name} ×{count}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn outcome_text(&self, turn: &Turn<'a>, calling_turn: &str) -> &'static str {
        match turn.outcome {
            Some(outcome) => outcome.text(),
            None if turn.turn_id == calling_turn => "in progress",
            None if turn.start < self.own_start => "continued on the parent line",
            None => "in progress",
        }
    }

    /// The full text of one thing (§6), paged by characters.
    fn full(&self, thing: &Thing<'a>) -> String {
        let event = &self.events[thing.origin];
        let heading = heading(thing, event.id.as_str(), event.create_at);
        format!("{heading}\n{}", self.body(thing))
    }

    /// The part of a full view below its heading: what `about` searches, so
    /// searching and opening one thing cannot disagree (§2).
    fn body(&self, thing: &Thing<'a>) -> String {
        let time = self.events[thing.origin].create_at;
        match &thing.kind {
            Kind::Input { input, live, .. } => {
                let mut output = Vec::new();
                if let Some(user) = &input.user {
                    output.push(String::new());
                    output.push(user_full(user));
                }
                for (position, block) in input.blocks.iter().enumerate() {
                    let InputBlock::Raw { text, key, retire } = block;
                    let marker = if live[position] {
                        " (current version in your context)"
                    } else if *retire && key.is_some() {
                        " (ended)"
                    } else {
                        ""
                    };
                    output.push(String::new());
                    output.push(format!("{text}{marker}"));
                }
                output.join("\n")
            }
            Kind::Reply { text, .. } => format!("\n{text}"),
            Kind::Call(call) => {
                let ending = ending(call, time, true);
                match &call.input {
                    Some(input) => format!(
                        "{}\n\n{ending}",
                        serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".into())
                    ),
                    None => format!("\n{ending}"),
                }
            }
            _ => format!("\n{}", self.line(thing)),
        }
    }
}

/// The first line of a thing's full view: its time, id and what it is (§6).
fn heading(thing: &Thing<'_>, id: &str, time: u64) -> String {
    let clock = clock(time);
    match &thing.kind {
        Kind::Input {
            input,
            live,
            working,
        } => {
            if let Some(write) = line_write(input) {
                let label = match (&write, *working) {
                    (
                        LineWrite::Message {
                            from,
                            final_message: true,
                            ..
                        },
                        _,
                    ) => {
                        format!("line {}'s final message", part(from))
                    }
                    (LineWrite::Message { from, .. }, true) => {
                        format!("line {} (while you were working)", part(from))
                    }
                    (LineWrite::Message { from, .. }, false) => format!("line {}", part(from)),
                    (LineWrite::Idle { branch, .. }, _) => {
                        format!("line {}'s turn ended", part(branch))
                    }
                    (LineWrite::Ended { branch }, _) => {
                        format!("line {} was ended", part(branch))
                    }
                };
                return format!("{clock} {id} {label}");
            }
            match (&input.user, *working) {
                (Some(_), true) => {
                    format!("{clock} {id} user (while you were working)")
                }
                (Some(_), false) => format!("{clock} {id} user"),
                (None, _) => format!("{clock} {id} {}", part(&blocks_name(&input.blocks, live))),
            }
        }
        Kind::Reply { state, .. } => {
            let label = match state {
                ReplyState::Writing => "you (still writing)".to_owned(),
                ReplyState::Completed => "you".to_owned(),
                ReplyState::CutOff(reason) => format!("your reply was cut off ({reason})"),
            };
            format!("{clock} {id} {label}")
        }
        Kind::Call(call) => format!("{clock} {id} {}", call_heading(call)),
        _ => format!("{clock} {id} {}", line_heading(thing)),
    }
}

fn page(level: &str, output: Vec<String>, ids: &[&str], next: Option<Value>) -> ToolResult {
    let mut structured = JsonObject::new();
    structured.insert("level".into(), Value::String(level.into()));
    structured.insert(
        "ids".into(),
        Value::Array(ids.iter().map(|id| Value::String((*id).into())).collect()),
    );
    if let Some(next) = next {
        structured.insert("next".into(), next);
    }
    ToolResult {
        structured_content: Some(structured),
        ..ToolResult::text(output.join("\n"))
    }
}

fn ids_of<'a>(lines: &[Line<'a>]) -> Vec<&'a str> {
    lines.iter().map(|line| line.id).collect()
}

fn entry_anchor<'a>(turns: &[Turn<'a>], things: &[Thing<'a>], entry: Entry) -> usize {
    match entry {
        Entry::Turn(index) => turns[index].start,
        Entry::Thing(index) => things[index].anchor,
    }
}

fn call_at<'t>(things: &'t mut [Thing<'_>], position: usize) -> Option<&'t mut Call> {
    match &mut things.get_mut(position)?.kind {
        Kind::Call(call) => Some(call.as_mut()),
        _ => None,
    }
}

/// The last consumed block of each key, by the event and block position that
/// block is at. Context assembly replays a block only at its `input/consumed`,
/// so a pending or withdrawn input carries no live version (§3.5).
fn last_blocks(events: &[Arc<AgentEvent>]) -> HashMap<&str, (usize, usize)> {
    let mut inputs: HashMap<&str, (usize, &InputMessageData)> = HashMap::new();
    let mut last = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        match &event.kind {
            EventKind::InputMessage(data) => {
                inputs.insert(event.id.as_str(), (index, data));
            }
            EventKind::InputConsumed(data) => {
                let Some((origin, input)) = inputs.get(data.input_id.as_str()) else {
                    continue;
                };
                for (position, block) in input.blocks.iter().enumerate() {
                    let InputBlock::Raw { key: Some(key), .. } = block else {
                        continue;
                    };
                    last.insert(key.as_str(), (*origin, position));
                }
            }
            _ => {}
        }
    }
    last
}

/// Which of an input's blocks are still live: the latest block of its key in
/// the pinned history, a retiring one aside (§3.5).
fn live_blocks(
    input: &InputMessageData,
    origin: usize,
    last: &HashMap<&str, (usize, usize)>,
) -> Vec<bool> {
    input
        .blocks
        .iter()
        .enumerate()
        .map(|(position, block)| {
            let InputBlock::Raw {
                key: Some(key),
                retire,
                ..
            } = block
            else {
                return false;
            };
            !retire && last.get(key.as_str()) == Some(&(origin, position))
        })
        .collect()
}

/// The text a list of message parts says, thinking left out.
fn content_text(parts: &[MessagePart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn user_text(user: &UserInput) -> String {
    user.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "[image]".into(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn user_full(user: &UserInput) -> String {
    user.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "[image]".into(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The result text the model read: its `content`, blobs and images as
/// `[file]` and `[image]`, and the context messages the tool added (§3.4).
fn result_text(result: &ToolResult) -> String {
    let mut text = match &result.content {
        ToolContent::Text(text) => text.clone(),
        ToolContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::Image { .. } => "[image]".into(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    for _ in result.blob_refs.iter().flatten() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str("[file]");
    }
    if let Some(messages) = &result.context_messages
        && !messages.is_empty()
    {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&format!("with {} context messages", messages.len()));
    }
    text
}

fn compaction_line(current: bool) -> String {
    if current {
        "the conversation up to here was summarized (that summary is in your context)".into()
    } else {
        "the conversation up to here was summarized".into()
    }
}

fn input_line(input: &InputMessageData, live: &[bool], working: bool) -> String {
    if let Some(write) = line_write(input) {
        return write_line(&write, working);
    }
    let speaker = if working {
        "user (while you were working)"
    } else {
        "user"
    };
    let blocks = blocks_name(&input.blocks, live);
    match (&input.user, input.blocks.is_empty()) {
        (Some(user), true) => format!("{speaker}: {}", part(&user_text(user))),
        (Some(user), false) => format!("{speaker}: {} + {}", part(&user_text(user)), part(&blocks)),
        (None, _) => format!(
            "{}: {}",
            part(&blocks),
            part(&first_body_line(&input.blocks))
        ),
    }
}

/// Each block named in order by its opening tag and attributes, or `block`,
/// with its liveness (§3.4).
fn blocks_name(blocks: &[InputBlock], live: &[bool]) -> String {
    blocks
        .iter()
        .enumerate()
        .map(|(position, block)| {
            let InputBlock::Raw { text, key, retire } = block;
            let mut name = match tag(text) {
                Some(tag) => tag.replace('"', ""),
                None => "block".into(),
            };
            if live[position] {
                name.push_str(" (current)");
            } else if *retire && key.is_some() {
                name.push_str(" (ended)");
            }
            name
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What another line wrote to this one, when the input opens with one of the
/// line markers (§3.4).
enum LineWrite<'a> {
    /// A message; `final_message` marks the one a line ends with.
    Message {
        from: &'a str,
        text: &'a str,
        final_message: bool,
    },
    /// A line's turn ended without a message to this one.
    Idle { branch: &'a str, status: &'a str },
    /// A line was ended from outside its own line.
    Ended { branch: &'a str },
}

/// The line write an input carries, read from the marker of its first block
/// that is one. A roster update carries its ended notice in a later block, and
/// that notice is what the input says.
fn line_write(input: &InputMessageData) -> Option<LineWrite<'_>> {
    for block in &input.blocks {
        let InputBlock::Raw { text, .. } = block;
        let Some(head) = tag(text) else {
            continue;
        };
        let name = head.split(' ').next().unwrap_or_default();
        let write = match name {
            "dim-branch-message" => LineWrite::Message {
                from: attribute(head, "from")?,
                text: body_line(text, name),
                final_message: attribute(head, "final") == Some("true"),
            },
            "dim-branch-idle" => LineWrite::Idle {
                branch: attribute(head, "branch")?,
                status: attribute(head, "status")?,
            },
            "dim-branch-ended" => LineWrite::Ended {
                branch: attribute(head, "branch")?,
            },
            _ => continue,
        };
        return Some(write);
    }
    None
}

/// How one line write reads in a line (§3.4). `working` is whether the input
/// was read after its turn's first model step, whatever mode it arrived in.
fn write_line(write: &LineWrite<'_>, working: bool) -> String {
    match write {
        LineWrite::Message {
            from,
            text,
            final_message: true,
        } => {
            format!(
                "line {}'s final message: {}",
                part(from),
                value(text, false)
            )
        }
        LineWrite::Message {
            from,
            text,
            final_message: false,
        } if working => format!(
            "line {} (while you were working): {}",
            part(from),
            value(text, false)
        ),
        LineWrite::Message {
            from,
            text,
            final_message: false,
        } => {
            format!("line {}: {}", part(from), value(text, false))
        }
        LineWrite::Idle { branch, status } => format!(
            "line {}'s turn ended ({}) without a message to you",
            part(branch),
            part(status)
        ),
        LineWrite::Ended { branch } => format!("line {} was ended from outside", part(branch)),
    }
}

/// The first line of a marked block's body, without its closing tag.
fn body_line<'a>(text: &'a str, name: &str) -> &'a str {
    let Some((_, rest)) = text.split_once('>') else {
        return text;
    };
    let closing = format!("</{name}>");
    let body = rest.trim().trim_end_matches(closing.as_str()).trim();
    body.lines().next().unwrap_or_default()
}

/// The value of one attribute of an opening tag, when it has one.
fn attribute<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let spelled = format!("{name}=\"");
    let at = head.find(&spelled)?;
    let rest = &head[at + spelled.len()..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// The opening tag of a block's text, when it opens with one.
fn tag(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('<')?;
    let end = rest.find('>')?;
    let head = &rest[..end];
    (!head.is_empty() && !head.starts_with('/')).then_some(head)
}

/// The first line of a block's body: after its opening tag, or of its text.
fn first_body_line(blocks: &[InputBlock]) -> String {
    let Some(InputBlock::Raw { text, .. }) = blocks.first() else {
        return String::new();
    };
    let body = match tag(text) {
        Some(_) => text
            .split_once('>')
            .map(|(_, body)| body.trim_start_matches('\n'))
            .unwrap_or(text),
        None => text.as_str(),
    };
    body.split('\n').next().unwrap_or_default().to_owned()
}

fn call_line(call: &Call, origin: u64) -> String {
    match &call.input {
        Some(input) => {
            let brief = brief(input);
            if brief.is_empty() {
                format!(
                    "you called {} → {}",
                    part(&call.name),
                    ending(call, origin, false)
                )
            } else {
                format!(
                    "you called {} {} → {}",
                    part(&call.name),
                    part(&brief),
                    ending(call, origin, false)
                )
            }
        }
        None => format!(
            "you started calling {} (arguments not received) → {}",
            part(&call.name),
            ending(call, origin, false)
        ),
    }
}

fn call_heading(call: &Call) -> String {
    if call.input.is_some() {
        format!("you called {}", part(&call.name))
    } else {
        format!(
            "you started calling {} (arguments not received)",
            part(&call.name)
        )
    }
}

/// The folded ending of a call (§3.4), the first that applies.
fn ending(call: &Call, origin: u64, full: bool) -> String {
    if matches!(call.approval, Some(Approval::Denied)) {
        return "denied by the user".into();
    }
    if let Some(Approval::Cancelled(reason)) = &call.approval {
        return format!("approval cancelled: {}", value(reason, full));
    }
    if let Some(result) = &call.result
        && result.never_dispatched
    {
        return format!(
            "not run{}: {}",
            when(result.time, origin),
            value(&result.text, full)
        );
    }
    if let Some(sub_agent) = &call.sub_agent {
        let (label, text) = match &sub_agent.outcome {
            CollaborationOutcome::Completed => ("answered", sub_agent.text.as_str()),
            CollaborationOutcome::Failed { reason } => ("failed", reason.as_str()),
            CollaborationOutcome::Cancelled { reason } => ("cancelled", reason.as_str()),
        };
        return format!(
            "sub-agent {} {label}: {}",
            value(&sub_agent.agent, full),
            value(text, full)
        );
    }
    if let Some(exec) = &call.exec {
        if let Some((time, status, text)) = &exec.ended {
            return format!(
                "ran in background task {}; finished at {}: {}: {}",
                value(&exec.task, full),
                clock(*time),
                status_text(*status),
                value(text, full)
            );
        }
        return format!("running in background task {}", value(&exec.task, full));
    }
    if let Some(result) = &call.result {
        let label = if result.error { "error" } else { "ok" };
        return format!(
            "{label}{}: {}",
            when(result.time, origin),
            value(&result.text, full)
        );
    }
    "no result yet".into()
}

fn status_text(status: ExecStatus) -> &'static str {
    match status {
        ExecStatus::Completed => "ok",
        ExecStatus::Failed => "error",
        ExecStatus::Cancelled => "cancelled",
    }
}

/// One variable part of a line: cut to `PART` characters, quoted when it says
/// spaces, or verbatim and whole in a full view.
fn value(text: &str, full: bool) -> String {
    if full {
        return text.to_owned();
    }
    let text = part(text);
    if text.chars().any(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text
    }
}

/// The time an ending carries when it arrives in a later minute than its call.
fn when(time: u64, origin: u64) -> String {
    if time / 60_000 == origin / 60_000 {
        String::new()
    } else {
        format!(" ({})", clock(time))
    }
}

/// The scalar fields of an input as `key=value`, nested values as `{…}`.
fn brief(input: &JsonObject) -> String {
    input
        .iter()
        .map(|(key, value)| match value {
            Value::String(text) => {
                let (first, more) = first_line(text);
                let text = if more { format!("{first}…") } else { first };
                format!("{key}={}", quote(&text))
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => format!("{key}={value}"),
            Value::Array(_) | Value::Object(_) => format!("{key}={{…}}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(text: &str) -> String {
    if text.chars().any(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

/// The first line of a text that says something, at most `PART` characters
/// with its `…`.
fn part(text: &str) -> String {
    let (first, _) = first_line(text);
    if first.chars().count() <= PART {
        return first;
    }
    let mut cut: String = first.chars().take(PART - 1).collect();
    cut.push('…');
    cut
}

/// The first non-blank line of a text, and whether content follows it.
fn first_line(text: &str) -> (String, bool) {
    let mut chosen: Option<&str> = None;
    let mut more = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if chosen.is_none() {
            if !line.trim().is_empty() {
                chosen = Some(line);
            }
        } else if !line.trim().is_empty() {
            more = true;
            break;
        }
    }
    (chosen.unwrap_or_default().to_owned(), more)
}

fn filters(arguments: &Arguments) -> String {
    match (&arguments.about, &arguments.tool) {
        (Some(about), Some(tool)) => format!("about \"{about}\" and tool {}", part(tool)),
        (Some(about), None) => format!("about \"{about}\""),
        (None, Some(tool)) => format!("tool {}", part(tool)),
        (None, None) => String::new(),
    }
}

/// The short label a full view of a thing without a body puts above its line.
fn line_heading<'a>(thing: &Thing<'a>) -> String {
    match &thing.kind {
        Kind::Interrupt => "interrupt".into(),
        Kind::Permission { mode } => format!("permission {mode}"),
        Kind::Compaction { .. } => "summarized".into(),
        Kind::Fork { parent } => format!("fork of {}", part(parent)),
        Kind::SessionFork => "session fork".into(),
        Kind::Input { .. } | Kind::Reply { .. } | Kind::Call(_) => {
            unreachable!("inputs, replies and calls render their own full views")
        }
    }
}

fn preset(preset: &PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "read-only",
        PermissionPreset::WorkspaceWrite => "workspace-write",
        PermissionPreset::FullAccess => "full-access",
    }
}

fn time(millis: u64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(millis as i64)
        .single()
        .unwrap_or_default()
}

fn clock(millis: u64) -> String {
    time(millis).format("%H:%M").to_string()
}

fn date(millis: u64) -> String {
    time(millis).format("%Y-%m-%d").to_string()
}

/// The projection behind `recall`, for flow tests that answer recorded
/// histories. Feature `testing` only; production builds have none.
#[cfg(feature = "testing")]
pub mod testing {
    use super::Narrative as Fold;
    use dim_agent_types::{AgentEvent, JsonObject, ToolResult};
    use std::sync::Arc;

    /// The pinned history of one branch folded into the narrative of §3.
    pub struct Narrative<'a>(Fold<'a>);

    impl<'a> Narrative<'a> {
        /// Folds a pinned branch history into things and turns.
        pub fn new(events: &'a [Arc<AgentEvent>]) -> Self {
            Self(Fold::new(events))
        }

        /// The tool's answer for `input`, called from `calling_turn`.
        pub fn answer(&self, calling_turn: &str, input: &JsonObject) -> ToolResult {
            self.0.answer(calling_turn, input)
        }

        /// Whether this narrative tells the event: the origin of a thing, or
        /// an event joined to one (§3.2). Every other event, of any kind, is
        /// not told.
        pub fn tells(&self, event_id: &str) -> bool {
            self.0.told.contains(event_id)
        }
    }
}
