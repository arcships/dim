//! ShellInput, see `docs/architecture/rust-agent/plugins/ShellInput.md` and
//! the design `docs/design/plugins/ShellInput.md`.
//!
//! `ShellInputPlugin` (Session) serves `shell.run`, `shell.stop` and
//! `shell.list` through `sessionMethods` key `shell`. A command the user typed
//! runs in the Session's working directory, built and read exactly as `exec`
//! builds and reads it; at most one runs per Session. A run belongs to this
//! plugin generation, not to the call that started it: the call's token only
//! ends the wait for the reply, while the generation's cancellation or the
//! Session's closing terminates the process tree. A registered run holds the
//! Session (`SessionLifecycle::hold`), so an idle unload never ends it.
use crate::{
    context_block::{element, raw, text},
    host_platform::{HostPlatform, ProcessTree},
    methods::{Interruption, MethodDecl, Progress, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
    workspace_tools::exec::{
        OutputSink, capture_preview, pump, sanitize, signal_name, tail_bytes, truncate_preview,
    },
};
use dim_agent_runtime::{
    AgentBuilder, Capability, InSession, KeysOf, SessionConfig, SessionHold, SessionIdentity,
    SessionLifecycle, SessionPhase, SessionScope, scoped,
};
use dim_agent_types::{InputMessageData, InputMode};
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io,
    path::PathBuf,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::oneshot,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// The Session method namespace ShellInput provides.
pub const SHELL_METHODS: &str = "shell";
const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// At most one progress item per interval, with at most this much text.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const PROGRESS_MAX_BYTES: usize = 64 * 1024;

/// Installs ShellInput in every Session and records its namespace.
pub fn install(builder: &mut AgentBuilder) {
    builder
        .session_plugin(ShellInputPlugin::new)
        .session_methods(SHELL_METHODS);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// `!command`: the result is also written as an Append input.
    Record,
    /// `!!command`: the result goes only to the caller.
    Private,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunParams {
    /// Read by Rpc to route the call; the provider already serves this Session.
    #[serde(rename = "sessionId")]
    _session_id: String,
    #[serde(default = "main_branch")]
    branch_id: String,
    run_id: String,
    command: String,
    mode: Mode,
}
fn main_branch() -> String {
    "main".into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StopParams {
    #[serde(rename = "sessionId")]
    _session_id: String,
    run_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(rename = "sessionId")]
    _session_id: String,
}

/// A started run as `shell.list` reports it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunInfo {
    run_id: String,
    branch_id: String,
    mode: Mode,
    command: String,
    /// Unix milliseconds.
    started_at: u64,
}

struct Running {
    info: RunInfo,
    stop: CancellationToken,
    /// Cancelled once the run has ended and left the registry.
    ended: CancellationToken,
    /// Keeps the Session from an idle release until the run leaves the
    /// registry, after its last fact (idle unload §5).
    _hold: SessionHold,
}

#[derive(Default)]
struct Registry {
    running: Option<Running>,
    /// Run ids started in this generation, or stopped before their start: a
    /// command never runs twice.
    used: HashSet<String>,
    /// Run ids a `shell.stop` reached before their run did: that run then
    /// starts nothing.
    stopped: HashSet<String>,
}

struct ShellInput {
    platform: Arc<HostPlatform>,
    cwd: PathBuf,
    tree: Arc<TreeCommands>,
    lifecycle: Arc<SessionLifecycle>,
    identity: Arc<SessionIdentity>,
    /// This generation's cancellation: with the Session's closing, it ends
    /// every run.
    generation: CancellationToken,
    /// Run tasks, owned by this generation rather than by their calls.
    runs: TaskTracker,
    registry: Mutex<Registry>,
}

fn refused(app_error: &'static str, message: impl Into<String>) -> RpcError {
    RpcError::app(-32000, app_error, message)
}

/// What ended the wait for a running command.
enum End {
    Exited(io::Result<ExitStatus>),
    Stopped,
    Interrupted,
}

/// `runId` is `[A-Za-z0-9-]{1,64}` (design A.1).
fn check_run_id(run_id: &str) -> Result<(), RpcError> {
    if run_id.is_empty()
        || run_id.len() > 64
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(RpcError::invalid("runId must match [A-Za-z0-9-]{1,64}"));
    }
    Ok(())
}

impl ShellInput {
    /// Refuses a run that cannot start now: its call was interrupted, the
    /// Session part is ending, another run is registered or the runId was
    /// used.
    fn admit(
        &self,
        registry: &Registry,
        run_id: &str,
        cancel: &CancellationToken,
    ) -> Result<(), RpcError> {
        if cancel.is_cancelled() {
            return Err(RpcError::interrupted());
        }
        // A reload's next generation serves a retry; a closing Session none.
        if self.generation.is_cancelled() {
            return Err(RpcError::reloading());
        }
        if self.lifecycle.phase() >= SessionPhase::Closing {
            return Err(RpcError::closed());
        }
        if registry.running.is_some() {
            return Err(refused(
                "shell_busy",
                "Another command is running in this Session",
            ));
        }
        if registry.used.contains(run_id) {
            return Err(refused(
                "duplicate_run",
                format!("runId {run_id} was already used"),
            ));
        }
        Ok(())
    }

    /// The Session part is ending: a run then writes nothing.
    fn ending(&self) -> bool {
        self.generation.is_cancelled() || self.lifecycle.phase() >= SessionPhase::Closing
    }

    async fn run(
        self: Arc<Self>,
        value: Value,
        cancel: CancellationToken,
        progress: Progress,
    ) -> Result<Value, RpcError> {
        let p: RunParams = params(value)?;
        if p.command.is_empty() || p.command.len() > MAX_COMMAND_BYTES {
            return Err(RpcError::invalid(
                "command must be non-empty and at most 64 KiB",
            ));
        }
        check_run_id(&p.run_id)?;
        self.admit(&self.registry.lock().unwrap(), &p.run_id, &cancel)?;
        self.tree.get(&p.branch_id)?;
        // The builder may wait for HostPlatform's shell lock and resolution;
        // the call's token ends that wait, so no unload waits for it.
        let built = async {
            #[cfg(feature = "testing")]
            testing::pause(&self.cwd).await;
            self.platform
                .build_process_spec(&p.command, &self.cwd)
                .await
        };
        let (spec, _) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
            built = built => built.map_err(|error| {
                refused(
                    "shell_unavailable",
                    format!("{}: {}", error.code, error.reason),
                )
            })?,
        };
        let (send, reply) = oneshot::channel();
        {
            let mut registry = self.registry.lock().unwrap();
            // The admission that counts: one step with the Session hold, the
            // spawn and the registration. A call interrupted before it starts
            // nothing and leaves its runId unused; after it, the run belongs
            // to this generation (A.4). The disposer closes `runs` under this
            // lock after the generation token fired, so every admitted run is
            // tracked. The spawn (fork and exec) runs under this std mutex,
            // which no await holds, so `shell.list` and `shell.stop` wait for
            // that alone.
            self.admit(&registry, &p.run_id, &cancel)?;
            if registry.stopped.remove(&p.run_id) {
                registry.used.insert(p.run_id.clone());
                return Ok(json!({
                    "runId": p.run_id,
                    "status": "stopped",
                    "output": "",
                    "truncated": false,
                    "durationMs": 0,
                }));
            }
            // None once the Session is closing or an idle release has been
            // decided; a refused spawn drops it with the start.
            let hold = self.lifecycle.hold().ok_or_else(RpcError::closed)?;
            let tree = self.platform.spawn(spec).map_err(|error| {
                refused("spawn_failed", format!("Failed to spawn process: {error}"))
            })?;
            let info = RunInfo {
                run_id: p.run_id,
                branch_id: p.branch_id,
                mode: p.mode,
                command: p.command,
                started_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };
            let (stop, ended) = (CancellationToken::new(), CancellationToken::new());
            registry.used.insert(info.run_id.clone());
            registry.running = Some(Running {
                info: info.clone(),
                stop: stop.clone(),
                ended: ended.clone(),
                _hold: hold,
            });
            self.runs.spawn(
                self.clone()
                    .supervise(info, tree, stop, ended, progress, send),
            );
        }
        tokio::select! {
            reply = reply => reply.unwrap_or_else(|_| Err(RpcError::interrupted())),
            // An Rpc reload or this provider's unload ends only the wait; the
            // run stays with this generation (A.4 Two cancellations).
            _ = cancel.cancelled() => Err(RpcError::interrupted()),
        }
    }

    /// Owns one run until it ends, then leaves the registry and replies.
    async fn supervise(
        self: Arc<Self>,
        run: RunInfo,
        mut tree: ProcessTree,
        stop: CancellationToken,
        ended: CancellationToken,
        progress: Progress,
        reply: oneshot::Sender<Result<Value, RpcError>>,
    ) {
        let span = tracing::info_span!(
            "shell.run",
            session_id = %self.identity.session_id,
            branch_id = %run.branch_id,
            run_id = %run.run_id,
            outcome = tracing::field::Empty,
        );
        let started = Instant::now();
        let output = Arc::new(RunOutput::default());
        let stop_pumps = CancellationToken::new();
        let pumps = [
            tokio::spawn(pump(tree.stdout(), output.clone(), stop_pumps.clone())),
            tokio::spawn(pump(tree.stderr(), output.clone(), stop_pumps.clone())),
        ];
        let ending = async {
            tokio::select! {
                _ = self.generation.cancelled() => {}
                _ = self.lifecycle.closing() => {}
            }
        };
        tokio::pin!(ending);
        let mut ticks = tokio::time::interval(PROGRESS_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut sent = None;
        // An exit already there wins over a stop or the end of the part.
        let end = loop {
            tokio::select! {
                biased;
                status = tree.wait() => break End::Exited(status),
                _ = &mut ending => break End::Interrupted,
                _ = stop.cancelled() => break End::Stopped,
                _ = ticks.tick() => if let Some(item) = output.take_progress() {
                    progress.send(item);
                    sent = Some(Instant::now());
                },
            }
        };
        let terminated = match end {
            End::Exited(Ok(_)) => Ok(()),
            _ => tree.terminate().await,
        };
        // Output ready at the exit is read for exec's grace, then the pipes
        // close; descendants left in the background do not hold the run.
        stop_pumps.cancel();
        futures_util::future::join_all(pumps).await;
        drop(tree);
        // The rest of the output, still one interval after the last item;
        // before the Session part is checked again for the write below.
        if !self.ending()
            && let Some(item) = output.take_progress()
        {
            if let Some(sent) = sent {
                tokio::time::sleep_until(sent + PROGRESS_INTERVAL).await;
            }
            progress.send(item);
        }
        // Whether to write is decided here, and no await follows before
        // `record`. A run the Session part ended never writes, since
        // `ending()` stays true. A Session that starts closing after this
        // check gets the input like any input sent just before its close,
        // and once its branch's close has started `Branch::input` refuses it
        // (`recorded.error`).
        let (result, outcome) = if self.ending() {
            (Err(RpcError::interrupted()), "interrupted".to_owned())
        } else {
            let (status, exit, reason) = match (end, terminated) {
                (End::Exited(Ok(status)), _) => ("completed", Some(status), None),
                (End::Exited(Err(error)), _) => (
                    "failed",
                    None,
                    Some(format!("Failed to wait for process: {error}")),
                ),
                (_, Err(error)) => (
                    "failed",
                    None,
                    Some(format!("Command could not be terminated: {error}")),
                ),
                _ => ("stopped", None, None),
            };
            let code = exit.and_then(|status| status.code());
            let signal = exit.and_then(signal_name);
            let (output, truncated) = output.bounded();
            let mut result = json!({
                "runId": run.run_id,
                "status": status,
                "output": output,
                "truncated": truncated,
                "durationMs": started.elapsed().as_millis() as u64,
            });
            if let Some(code) = code {
                result["exitCode"] = code.into();
            }
            if let Some(signal) = &signal {
                result["signal"] = signal.as_str().into();
            }
            if let Some(reason) = reason {
                result["reason"] = reason.into();
            }
            let exit = code.map(|code| code.to_string()).or(signal);
            if run.mode == Mode::Record {
                result["recorded"] =
                    match self.record(&run, status, exit.as_deref(), truncated, &output) {
                        Ok(sequence) => json!({ "sequence": sequence }),
                        Err(error) => {
                            let error = RpcError::from(error);
                            let mut rejected = json!({ "message": error.message });
                            if let Some(data) = error.data {
                                rejected["appError"] = data.app_error.into();
                            }
                            json!({ "error": rejected })
                        }
                    };
            }
            let outcome = match exit {
                Some(exit) => format!("{status}:{exit}"),
                None => status.to_owned(),
            };
            (Ok(result), outcome)
        };
        // Free the Session before replying, so the caller can start the next
        // run; this also drops the run's hold, after its last fact above.
        self.registry.lock().unwrap().running = None;
        ended.cancel();
        span.record("outcome", outcome.as_str());
        let _ = reply.send(result);
    }

    /// Writes the run's result once, as the Append input of design A.3 on
    /// its branch; never retried.
    fn record(
        &self,
        run: &RunInfo,
        status: &str,
        exit: Option<&str>,
        truncated: bool,
        output: &str,
    ) -> dim_agent_runtime::Result<u64> {
        let truncated = truncated.to_string();
        let mut attributes = vec![("kind", "command"), ("status", status)];
        if let Some(exit) = exit {
            attributes.push(("exit", exit));
        }
        attributes.push(("truncated", &truncated));
        let marker = element(
            "dim-shell-input",
            &attributes,
            &text(&format!("$ {}\n{output}", run.command)),
        );
        let event = self.tree.get(&run.branch_id)?.input(InputMessageData {
            blocks: vec![raw(marker, None)],
            mode: Some(InputMode::Append),
            ..InputMessageData::default()
        })?;
        Ok(event.sequence)
    }

    async fn stop(&self, value: Value, cancel: CancellationToken) -> Result<Value, RpcError> {
        let p: StopParams = params(value)?;
        check_run_id(&p.run_id)?;
        let ended = {
            let mut guard = self.registry.lock().unwrap();
            let registry = &mut *guard;
            match &registry.running {
                Some(running) if running.info.run_id == p.run_id => {
                    running.stop.cancel();
                    running.ended.clone()
                }
                _ => {
                    // Ahead of its run (A.4 Stop): that run then starts nothing.
                    if !registry.used.contains(&p.run_id) {
                        registry.stopped.insert(p.run_id);
                    }
                    return reply(json!({"stopped": false}));
                }
            }
        };
        tokio::select! {
            _ = ended.cancelled() => reply(json!({"stopped": true})),
            _ = cancel.cancelled() => Err(RpcError::interrupted()),
        }
    }

    fn list(&self, value: Value) -> Result<Value, RpcError> {
        let _: ListParams = params(value)?;
        let registry = self.registry.lock().unwrap();
        let runs: Vec<_> = registry
            .running
            .iter()
            .map(|running| &running.info)
            .collect();
        reply(json!({ "runs": runs }))
    }
}

/// The output of one run, both pipes in arrival order: exec's bounded tail,
/// and the text the next progress item carries.
#[derive(Default)]
struct RunOutput(Mutex<Captured>);
#[derive(Default)]
struct Captured {
    preview: String,
    /// Bytes decoded in all; more than `preview` holds means its head was cut.
    decoded: usize,
    /// Sanitized text since the last item, within its last 64 KiB.
    pending: String,
    /// Bytes left out before `pending`.
    skipped: usize,
}
impl OutputSink for RunOutput {
    async fn push(&self, text: &str) {
        let mut captured = self.0.lock().unwrap();
        captured.decoded += text.len();
        capture_preview(&mut captured.preview, text);
        captured.pending.push_str(&sanitize(text));
        if captured.pending.len() > PROGRESS_MAX_BYTES {
            let kept = tail_bytes(&captured.pending, PROGRESS_MAX_BYTES).to_owned();
            captured.skipped += captured.pending.len() - kept.len();
            captured.pending = kept;
        }
    }
}
impl RunOutput {
    /// The next progress item `{text, skipped?}`, if there is new text.
    fn take_progress(&self) -> Option<Value> {
        let mut captured = self.0.lock().unwrap();
        if captured.pending.is_empty() {
            return None;
        }
        let mut item = json!({ "text": std::mem::take(&mut captured.pending) });
        let skipped = std::mem::take(&mut captured.skipped);
        if skipped > 0 {
            item["skipped"] = skipped.into();
        }
        Some(item)
    }
    /// exec's bounds (the last 2000 lines within 50 KiB), and whether they
    /// left anything out.
    fn bounded(&self) -> (String, bool) {
        let captured = self.0.lock().unwrap();
        let text = truncate_preview(&captured.preview);
        let truncated = captured.decoded > captured.preview.len()
            || text.len() < sanitize(&captured.preview).len();
        (text, truncated)
    }
}

/// Session plugin providing `sessionMethods` key `shell`.
pub struct ShellInputPlugin {
    scope: SessionScope,
}
impl ShellInputPlugin {
    pub fn new(scope: &SessionScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for ShellInputPlugin {
    type Deps = (
        Arc<HostPlatform>,
        InSession<SessionConfig>,
        InSession<TreeCommands>,
        InSession<SessionLifecycle>,
        InSession<SessionIdentity>,
    );
    fn name(&self) -> &str {
        "ShellInput"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            platform,
            InSession(config),
            InSession(tree),
            InSession(lifecycle),
            InSession(identity),
        ): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let shell = Arc::new(ShellInput {
                platform,
                // `session.create` validated and canonicalized the cwd.
                cwd: PathBuf::from(&config.cwd),
                tree,
                lifecycle,
                identity,
                generation: ctx.cancellation_token(),
                runs: TaskTracker::new(),
                registry: Mutex::default(),
            });
            let owner = shell.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        {
                            let _registry = owner.registry.lock().unwrap();
                            owner.runs.close();
                        }
                        owner.runs.wait().await;
                        Ok(())
                    })
                }))
            })?;
            // After the effect above, so the provider's unload, which settles
            // the calls, runs before the runs are awaited.
            SessionMethods::provide(
                ctx,
                &self.scope,
                SHELL_METHODS,
                vec![
                    MethodDecl {
                        progress: true,
                        ..MethodDecl::new("shell.run", true, Interruption::Report)
                    },
                    MethodDecl::new("shell.stop", true, Interruption::Report),
                    MethodDecl::new("shell.list", false, Interruption::Report),
                ],
                Arc::new(move |method, value, cancel, progress| {
                    let shell = shell.clone();
                    Box::pin(async move {
                        // The provider admits only the declared names.
                        match method.as_str() {
                            "shell.run" => shell.run(value, cancel, progress).await,
                            "shell.stop" => shell.stop(value, cancel).await,
                            _ => shell.list(value),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ShellInputPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

/// Holds a run's command preparation open for race flow tests. Feature
/// `testing` only; production builds have no hook.
#[cfg(feature = "testing")]
pub mod testing {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };
    use tokio::sync::oneshot;

    type Hold = (PathBuf, oneshot::Sender<()>, oneshot::Receiver<()>);
    static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

    /// Holds the preparation of the next `shell.run` in a Session working in
    /// `cwd`, before its command is built, until this is dropped.
    pub struct HeldStart {
        arrived: oneshot::Receiver<()>,
        _release: oneshot::Sender<()>,
    }
    impl HeldStart {
        /// Resolves once the held run is being prepared.
        pub async fn arrived(&mut self) {
            let _ = (&mut self.arrived).await;
        }
    }

    pub fn hold_start(cwd: &Path) -> HeldStart {
        let (arrive, arrived) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let cwd = cwd.canonicalize().expect("an existing working directory");
        HOLDS.lock().unwrap().push((cwd, arrive, released));
        HeldStart {
            arrived,
            _release: release,
        }
    }

    pub(super) async fn pause(cwd: &Path) {
        let hold = {
            let mut holds = HOLDS.lock().unwrap();
            let index = holds.iter().position(|(held, ..)| held == cwd);
            index.map(|index| holds.swap_remove(index))
        };
        if let Some((_, arrive, released)) = hold {
            let _ = arrive.send(());
            let _ = released.await;
        }
    }
}
