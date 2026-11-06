//! Branch permission: decides a tool call by risk, its file-access facts and
//! policy. The policy is `SessionConfig.permission_policy` folded with this
//! branch's dispatched `permission/changed` events. The user's command rules,
//! read through the Session's `CommandRulesHost` on each decision, deny or
//! approve commands of process-starting calls. A file read or write reads the
//! Session's `fileAccessFacts` service, which resolves the workspace, system,
//! owned and grant facts; without that service a write keeps the
//! workspace-only rule and a read the preset rule.
//!
//! The admission decision also asks the advisors registered with it (Hooks
//! A.6.1): a branch part registers one with [`Permission::register_advice`]
//! while it runs, and the advice is folded with Permission's own decision.
//! [`Permission::decide`] stays the plain decision, for the admission's
//! recheck after an answer.
use crate::{
    file_access::FileAccessFacts,
    host::{HostError, HostFuture},
    host_platform::HostPlatform,
    workspace_tools::files::{canonical_prefix, resolve_path},
};
use dim_agent_runtime::{
    AgentError, Dispatcher, Names, Registration, Registrations, SessionConfig, SessionIdentity,
    StateCell, Timeline, TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy,
    TimelineQueryScope,
};
use dim_agent_types::{
    AgentEvent, ApprovalCallData, BranchSource, COMMAND_SEPARATORS, CommandMatch, CommandRule,
    CommandRuleDecision, EventKind, FileAccess, PermissionChangedData, PermissionPolicy,
    PermissionPreset, grant::GrantMode,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

const CANCELLED: &str = "Tool execution cancelled.";

/// Permission's own decision when advisors are asked: `allow` or `ask`,
/// never `deny` (Hooks A.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allow,
    Ask,
}

/// The admission decision asked of one registered advisor (Hooks A.6.1).
#[derive(Clone, Debug)]
pub struct ToolAdviceRequest {
    pub source: BranchSource,
    pub call: ApprovalCallData,
    pub risk: Option<String>,
    pub decision: AdmissionDecision,
}

/// What one advisor answers (Hooks A.6.1). It is advice, not authority:
/// Permission folds it with its own decision, and it never overrides a deny.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolAdviceReply {
    None,
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

/// An advisor registered with the branch's Permission (Hooks A.6.1): asked
/// at each admission whose own decision is not a deny, in name order. An
/// error counts as no advice.
pub trait AdviseTool: Send + Sync + 'static {
    fn advise(
        &self,
        request: ToolAdviceRequest,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolAdviceReply>;
}

/// The dispatched policy; `version` is the local sequence of the last
/// `permission/changed` of this branch, 0 when there is none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PermissionState {
    pub version: u64,
    pub policy: PermissionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PermissionUpdateError {
    #[error("permission version conflict; current version is {current}")]
    Conflict { current: u64 },
    #[error("branch is closed")]
    Closed,
    #[error("{0}")]
    Failed(String),
}

/// The user's command rules, read when a command call is decided. Errors are
/// `Failed` (unreadable) or `Cancelled`.
pub trait ReadCommandRules: Send + Sync + 'static {
    fn rules(&self, cancel: CancellationToken) -> HostFuture<'_, Vec<CommandRule>>;
}
/// Session port of the command rules; each profile binds exactly one.
pub struct CommandRulesHost(pub Arc<dyn ReadCommandRules>);

/// The rules of a host without a rules source.
pub struct NoCommandRules;
impl ReadCommandRules for NoCommandRules {
    fn rules(&self, _: CancellationToken) -> HostFuture<'_, Vec<CommandRule>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[derive(Default)]
struct Inner {
    closed: bool,
    /// Local sequence of the last written `permission/changed`.
    written: u64,
    /// `update` waiters by event id, released when the event is dispatched.
    waiters: HashMap<String, oneshot::Sender<u64>>,
}

pub struct Permission {
    timeline: Arc<Timeline>,
    state: Arc<StateCell<PermissionState>>,
    rules: Arc<CommandRulesHost>,
    /// Held for reads after publication: the optional facts and platform are
    /// looked up at decision time, never while the part loads.
    ctx: Ctx,
    facts: Key<FileAccessFacts>,
    platform: Key<HostPlatform>,
    session_id: String,
    cwd: PathBuf,
    inner: Mutex<Inner>,
    advisors: Arc<Registrations<Arc<dyn AdviseTool>>>,
}
impl Permission {
    /// Registers `advise` under `name` on behalf of the part that owns `ctx`,
    /// until that part unloads (Hooks A.6.1). A second advisor of the same
    /// name fails.
    pub fn register_advice(
        &self,
        ctx: &Ctx,
        name: &str,
        advise: Arc<dyn AdviseTool>,
    ) -> Result<(), Error> {
        self.advisors.register(ctx, name, advise).map(|_| ())
    }

    /// The admission decision (Hooks A.6.1): its own decision first, then
    /// the advisors registered when it is made, in name order, when that
    /// decision is not a deny. Any deny wins, then any ask, then any allow;
    /// a provider error counts as none, and an advisor withdrawn while its
    /// advice is pending has that advice dropped as none. A cancelled batch
    /// drops the advice and fails the admission as cancelled.
    pub async fn admit_decision(
        &self,
        call: &ApprovalCallData,
        source: &BranchSource,
        risk: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Decision, HostError> {
        let decision = self.decide(call, risk, cancel).await?;
        let own = match decision {
            Decision::Deny { .. } => return Ok(decision),
            Decision::Allow => AdmissionDecision::Allow,
            Decision::Ask { .. } => AdmissionDecision::Ask,
        };
        let (mut allow, mut ask, mut deny) = (false, None, None);
        for registration in self.advisors.all() {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled(CANCELLED.into()));
            }
            let request = ToolAdviceRequest {
                source: source.clone(),
                call: call.clone(),
                risk: risk.map(str::to_owned),
                decision: own,
            };
            let advised = tokio::select! {
                biased;
                advised = registration.value.advise(request, cancel.clone()) => Some(advised),
                _ = withdrawn(&self.advisors, &registration) => None,
                _ = cancel.cancelled() => {
                    return Err(HostError::Cancelled(CANCELLED.into()));
                }
            };
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled(CANCELLED.into()));
            }
            match advised {
                None | Some(Err(_)) | Some(Ok(ToolAdviceReply::None)) => {}
                Some(Ok(ToolAdviceReply::Deny { reason })) => {
                    deny.get_or_insert(reason);
                }
                Some(Ok(ToolAdviceReply::Ask { reason })) => {
                    ask.get_or_insert(reason);
                }
                Some(Ok(ToolAdviceReply::Allow)) => allow = true,
            }
        }
        Ok(match (deny, ask) {
            (Some(reason), _) => Decision::Deny { reason },
            (None, Some(reason)) => Decision::Ask { reason },
            (None, None) if allow => Decision::Allow,
            (None, None) => decision,
        })
    }

    /// Decides with the policy dispatched when the decision is made and, for a
    /// command call, the rules read now. A file call resolves its target before
    /// sampling that policy. Cancelled reads stop admission.
    pub async fn decide(
        &self,
        call: &ApprovalCallData,
        risk: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Decision, HostError> {
        let category = Risk::of(risk);
        let write = category == Risk::Write;
        let home = (write || category == Risk::Read)
            .then(|| {
                self.ctx
                    .get_at(&self.platform)
                    .map(|platform| platform.home.clone())
            })
            .flatten();
        let read_facts = (category == Risk::Read)
            .then(|| self.ctx.get_at(&self.facts))
            .flatten();
        let path = call
            .input
            .get("path")
            .and_then(|path| path.as_str())
            .filter(|path| !path.is_empty());
        let target = self
            .resolve_target(category, path, home, read_facts.is_some(), cancel)
            .await?;
        let command = match (category, call.input.get("command")) {
            (Risk::Execute, Some(serde_json::Value::String(command))) => Some(command.as_str()),
            _ => None,
        };
        let rules = match command {
            Some(_) => match self.rules.0.rules(cancel.clone()).await {
                Err(HostError::Cancelled(reason)) => return Err(HostError::Cancelled(reason)),
                read => Some(read),
            },
            None => None,
        };
        let dispatched = self.state.get();
        let policy = dispatched.policy;
        let access = policy.preset.file_access();
        let name = &call.name;
        if policy.disallowed_tools.contains(name) {
            return Ok(Decision::Deny {
                reason: format!("Tool `{name}` is disallowed by the permission policy"),
            });
        }
        if write && let decision @ Decision::Deny { .. } = decide(&policy, name, risk) {
            return Ok(decision);
        }
        if write && access == FileAccess::ReadOnly {
            return Ok(Decision::Deny {
                reason: "File writes are not allowed by the read-only preset".into(),
            });
        }
        if write && access != FileAccess::Unrestricted {
            let facts = self.ctx.get_at(&self.facts);
            if let (Some(Ok((_, target))), Some(facts)) = (&target, facts) {
                let facts = tokio::select! {
                    result = facts.0.facts(&self.session_id, access, target, GrantMode::Write, cancel.clone()) => result,
                    _ = cancel.cancelled() => return Err(HostError::Cancelled(CANCELLED.into())),
                };
                let facts = match facts {
                    Ok(facts) => facts,
                    Err(HostError::Cancelled(reason)) => {
                        return Err(HostError::Cancelled(reason));
                    }
                    Err(error) => {
                        return Ok(Decision::Deny {
                            reason: format!("File access facts could not be read: {error}"),
                        });
                    }
                };
                if self.state.get().version != dispatched.version {
                    return Box::pin(self.decide(call, risk, cancel)).await;
                }
                let scope = match access {
                    FileAccess::ReadOnly => "the read-only preset",
                    FileAccess::WorkspaceReadWrite => "the workspace-write preset",
                    FileAccess::Unrestricted => unreachable!("unrestricted skips the facts"),
                };
                if facts.system {
                    return Ok(Decision::Deny {
                        reason: format!("Writing to a system path is not allowed by {scope}"),
                    });
                }
                if facts.owned {
                    return Ok(Decision::Deny {
                        reason: format!(
                            "Writing to a session-owned file is not allowed by {scope}"
                        ),
                    });
                }
                if access == FileAccess::WorkspaceReadWrite {
                    if facts.workspace || facts.grant {
                        return Ok(decide(&policy, name, risk));
                    }
                    return Ok(Decision::Ask {
                        reason: format!(
                            "Writing outside the session workspace requires approval: {}{}",
                            target.display(),
                            facts
                                .discovery_error
                                .as_ref()
                                .map(|error| format!(" (worktree discovery: {error})"))
                                .unwrap_or_default()
                        ),
                    });
                }
            } else if access == FileAccess::WorkspaceReadWrite {
                return Ok(match target {
                    Some(Ok((workspace, target))) if target.starts_with(&workspace) => {
                        decide(&policy, name, risk)
                    }
                    Some(Ok((_, target))) => Decision::Ask {
                        reason: format!(
                            "Writing outside the session workspace requires approval: {}",
                            target.display()
                        ),
                    },
                    Some(Err(reason)) => Decision::Deny { reason },
                    None => Decision::Ask {
                        reason:
                            "A file write without a verified workspace target requires approval"
                                .into(),
                    },
                });
            }
        }
        if category == Risk::Read
            && access != FileAccess::Unrestricted
            && path.is_some()
            && let Some(facts) = &read_facts
        {
            let target = match &target {
                Some(Ok((_, target))) => target,
                Some(Err(reason)) => {
                    return Ok(Decision::Deny {
                        reason: reason.clone(),
                    });
                }
                None => {
                    return Ok(Decision::Deny {
                        reason: "A file read without a resolved target cannot be allowed".into(),
                    });
                }
            };
            let facts = tokio::select! {
                result = facts.0.facts(&self.session_id, access, target, GrantMode::Read, cancel.clone()) => result,
                _ = cancel.cancelled() => return Err(HostError::Cancelled(CANCELLED.into())),
            };
            let facts = match facts {
                Ok(facts) => facts,
                Err(HostError::Cancelled(reason)) => {
                    return Err(HostError::Cancelled(reason));
                }
                Err(error) => {
                    return Ok(Decision::Deny {
                        reason: format!("File access facts could not be read: {error}"),
                    });
                }
            };
            if self.state.get().version != dispatched.version {
                return Box::pin(self.decide(call, risk, cancel)).await;
            }
            if !(facts.workspace || facts.system || facts.owned || facts.grant) {
                return Ok(Decision::Ask {
                    reason: format!(
                        "Reading outside the session workspace requires approval: {}{}",
                        target.display(),
                        facts
                            .discovery_error
                            .as_ref()
                            .map(|error| format!(" (worktree discovery: {error})"))
                            .unwrap_or_default()
                    ),
                });
            }
        }
        let (Some(command), Some(rules)) = (command, rules) else {
            return Ok(decide(&policy, name, risk));
        };
        let rules = match rules {
            Ok(rules) => rules,
            Err(error) => {
                return Ok(Decision::Deny {
                    reason: format!("Command rules could not be read: {error}"),
                });
            }
        };
        if let Some(rule) = rules.iter().find(|rule| {
            rule.decision == CommandRuleDecision::Deny
                && command
                    .split(COMMAND_SEPARATORS)
                    .any(|segment| matches(rule, segment))
        }) {
            let matching = match rule.matching {
                CommandMatch::Exact => "exact",
                CommandMatch::Prefix => "prefix",
            };
            return Ok(Decision::Deny {
                reason: format!(
                    "Command denied by command rule `{}` ({matching})",
                    rule.command
                ),
            });
        }
        Ok(match decide(&policy, name, risk) {
            Decision::Ask { .. }
                if !command.contains(COMMAND_SEPARATORS)
                    && rules.iter().any(|rule| {
                        rule.decision == CommandRuleDecision::Allow && matches(rule, command)
                    }) =>
            {
                Decision::Allow
            }
            decision => decision,
        })
    }

    /// Resolves the call's file target before admission, like the tool owner
    /// will: `~`, cwd, `..` and symlinks are settled. `None` means no facts
    /// query needs a target: the call has no path, no facts service is bound to
    /// a read, a relative path cannot be resolved without HostPlatform, or the
    /// preset is full-access.
    async fn resolve_target(
        &self,
        category: Risk,
        path: Option<&str>,
        home: Option<PathBuf>,
        facts: bool,
        cancel: &CancellationToken,
    ) -> Result<Option<Result<(PathBuf, PathBuf), String>>, HostError> {
        let resolvable = match category {
            Risk::Write => {
                path.is_some_and(|path| home.is_some() || std::path::Path::new(path).is_absolute())
            }
            Risk::Read => path.is_some_and(|path| {
                facts && (home.is_some() || std::path::Path::new(path).is_absolute())
            }),
            _ => false,
        };
        let Some(path) = path.filter(|_| resolvable) else {
            return Ok(None);
        };
        if self.state.get().policy.preset.file_access() == FileAccess::Unrestricted {
            return Ok(None);
        }
        let (cwd, path) = (self.cwd.clone(), path.to_owned());
        let target = tokio::task::spawn_blocking(move || {
            let workspace = std::fs::canonicalize(&cwd).map_err(|error| {
                format!("Failed to resolve workspace {}: {error}", cwd.display())
            })?;
            let target = match home {
                Some(home) => resolve_path(&cwd, &home, &path),
                None => canonical_prefix(PathBuf::from(path)),
            };
            target.map(|target| (workspace, target))
        });
        let target = tokio::select! {
            result = target => result.map_err(|error| HostError::failed(error.to_string()))?,
            _ = cancel.cancelled() => return Err(HostError::Cancelled(CANCELLED.into())),
        };
        Ok(Some(target))
    }

    /// Writes `permission/changed` when `expected` is the last written
    /// version; returns the new version once it is dispatched.
    pub async fn update(
        &self,
        expected: u64,
        policy: PermissionPolicy,
    ) -> Result<u64, PermissionUpdateError> {
        let (waiter, dispatched) = oneshot::channel();
        let (mut conflict, mut registered) = (None, None);
        let committed = self.timeline.append_registered(
            EventKind::PermissionChanged(PermissionChangedData { policy }).into(),
            |event| {
                let mut inner = self.inner.lock().unwrap();
                if inner.closed {
                    return Err(AgentError::Closed("Permission".into()));
                }
                if inner.written != expected {
                    conflict = Some(inner.written);
                    return Err(AgentError::InvalidInput(
                        "permission version conflict".into(),
                    ));
                }
                inner.written = event.sequence;
                inner.waiters.insert(event.id.clone(), waiter);
                registered = Some(event.id.clone());
                Ok(())
            },
        );
        if let Err(error) = committed {
            if let Some(id) = registered {
                let mut inner = self.inner.lock().unwrap();
                inner.written = expected;
                inner.waiters.remove(&id);
            }
            return Err(match (conflict, error) {
                (Some(current), _) => PermissionUpdateError::Conflict { current },
                (None, AgentError::Closed(_) | AgentError::Cancelled(_)) => {
                    PermissionUpdateError::Closed
                }
                (None, error) => PermissionUpdateError::Failed(error.to_string()),
            });
        }
        dispatched.await.map_err(|_| PermissionUpdateError::Closed)
    }
    /// Folds the chain's last `permission/changed`: its policy, and this
    /// branch's version when the change is the branch's own.
    async fn rebuild(&self) -> dim_agent_runtime::Result<u64> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Chain,
                include_publishing: true,
                from_event: None,
                from_sequence: None,
                order_by: TimelineQueryOrderBy::Sequence,
                direction: TimelineQueryDirection::Descending,
                kinds: vec!["permission/changed".into()],
                flags: Vec::new(),
                event_id: None,
                predicate: None,
                predicates: Vec::new(),
                text: None,
                limit: NonZeroU64::MIN,
                offset: 0,
            })
            .await?;
        let mut state = self.state.get();
        if let Some((event, position)) = reply.events.first().zip(reply.positions.first())
            && let EventKind::PermissionChanged(data) = &event.kind
        {
            state.policy = data.policy.clone();
            if position.branch_id == self.timeline.branch_id() {
                state.version = position.sequence;
            }
        }
        self.inner.lock().unwrap().written = state.version;
        self.state.set(state);
        Ok(reply.through)
    }
    fn consume(&self, event: &AgentEvent) {
        let EventKind::PermissionChanged(data) = &event.kind else {
            return;
        };
        let mut inner = self.inner.lock().unwrap();
        self.state.set(PermissionState {
            version: event.sequence,
            policy: data.policy.clone(),
        });
        if let Some(waiter) = inner.waiters.remove(&event.id) {
            let _ = waiter.send(event.sequence);
        }
    }
}

/// Resolves when `registration` is no longer the advisor of its name, which
/// is how its part's unload appears to an admission in flight (Hooks A.6.1).
async fn withdrawn(
    advisors: &Registrations<Arc<dyn AdviseTool>>,
    registration: &Arc<Registration<Arc<dyn AdviseTool>>>,
) {
    let mut changes = advisors.changes();
    loop {
        if !advisors
            .get(&registration.name)
            .is_some_and(|current| Arc::ptr_eq(&current, registration))
        {
            return;
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// Risk categories declared by tool owners in `ToolDefinition.risk`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Risk {
    Read,
    Write,
    Execute,
    Network,
    Session,
    Unclassified,
}
impl Risk {
    fn of(risk: Option<&str>) -> Self {
        match risk {
            Some("read_filesystem") => Self::Read,
            Some("write_filesystem") => Self::Write,
            Some("execute_process") => Self::Execute,
            Some("network") => Self::Network,
            Some("session") => Self::Session,
            _ => Self::Unclassified,
        }
    }
    fn describe(self) -> &'static str {
        match self {
            Self::Read => "reading files",
            Self::Write => "writing files",
            Self::Execute => "starting processes",
            Self::Network => "network access",
            Self::Session => "session operations",
            Self::Unclassified => "unclassified tools",
        }
    }
}

/// Compares words; `prefix` matches whole leading words.
fn matches(rule: &CommandRule, command: &str) -> bool {
    let expected: Vec<_> = rule.command.split_whitespace().collect();
    let words: Vec<_> = command.split_whitespace().collect();
    match rule.matching {
        CommandMatch::Exact => words == expected,
        CommandMatch::Prefix => words.starts_with(&expected),
    }
}

/// `allowedTools`, then the preset by risk; `disallowedTools` comes first.
fn decide(policy: &PermissionPolicy, name: &str, risk: Option<&str>) -> Decision {
    if policy.allowed_tools.iter().any(|item| item == name) {
        return Decision::Allow;
    }
    let risk = Risk::of(risk);
    let (preset, verdict) = match policy.preset {
        PermissionPreset::FullAccess => return Decision::Allow,
        PermissionPreset::ReadOnly => (
            "read-only",
            match risk {
                Risk::Read | Risk::Session => return Decision::Allow,
                _ => false,
            },
        ),
        PermissionPreset::WorkspaceWrite => (
            "workspace-write",
            match risk {
                Risk::Read | Risk::Write | Risk::Session => return Decision::Allow,
                _ => true,
            },
        ),
    };
    let what = risk.describe();
    if verdict {
        Decision::Ask {
            reason: format!("Tool `{name}` requires approval for {what} under the {preset} preset"),
        }
    } else {
        Decision::Deny {
            reason: format!("Tool `{name}` is not allowed by the {preset} preset ({what})"),
        }
    }
}

pub struct PermissionPlugin {
    state: Key<StateCell<PermissionState>>,
    permission: Key<Permission>,
    config: Key<SessionConfig>,
    rules: Key<CommandRulesHost>,
    facts: Key<FileAccessFacts>,
    identity: Key<SessionIdentity>,
    platform: Key<HostPlatform>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
}
impl PermissionPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            state: names.state_cell()?,
            permission: names.key()?,
            config: names.key()?,
            rules: names.key()?,
            facts: names.key()?,
            identity: names.key()?,
            platform: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for PermissionPlugin {
    fn name(&self) -> &str {
        "Permission"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.config.erase(),
            self.rules.erase(),
            self.identity.erase(),
            self.timeline.erase(),
            self.dispatcher.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            let rules = ctx.require_at(&self.rules)?;
            let identity = ctx.require_at(&self.identity)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let service = Arc::new(Permission {
                timeline,
                state: Arc::new(StateCell::new(PermissionState {
                    version: 0,
                    policy: config.permission_policy.clone(),
                })),
                rules,
                ctx: ctx.clone(),
                facts: self.facts.clone(),
                platform: self.platform.clone(),
                session_id: identity.session_id.clone(),
                cwd: PathBuf::from(&config.cwd),
                inner: Mutex::new(Inner::default()),
                advisors: Arc::new(Registrations::default()),
            });
            let closing = service.clone();
            ctx.effect(move || async move {
                let mut inner = closing.inner.lock().unwrap();
                inner.closed = true;
                inner.waiters.clear();
                Ok(())
            })?;
            let rebuild = service.clone();
            let live = service.clone();
            dispatcher
                .consume(
                    ctx,
                    "Permission".into(),
                    ["permission/changed"].map(String::from).to_vec(),
                    Arc::new(move || {
                        let rebuild = rebuild.clone();
                        Box::pin(async move { rebuild.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        let live = live.clone();
                        Box::pin(async move {
                            live.consume(&event);
                            Ok(())
                        })
                    }),
                )
                .await?;
            ctx.provide_at(self.state.clone(), service.state.clone())?;
            ctx.provide_at(self.permission.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}
