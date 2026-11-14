pub use dim_agent_types::ModelId;
use dim_agent_types::ModelSelection;
use rutis::service;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub name: String,
    pub system_prompt: String,
    pub model: ModelId,
}
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "dim-agent".into(),
            system_prompt: String::new(),
            model: ModelId {
                provider_id: "mock".into(),
                model_id: "deterministic".into(),
            },
        }
    }
}
#[service]
impl AgentConfig {
    /// The Agent's identity and default model as data, for parts in other
    /// languages.
    pub fn get(&self) -> AgentConfig {
        self.clone()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    #[serde(default)]
    pub permission_policy: crate::PermissionPolicy,
    pub cwd: String,
    pub model: ModelSelection,
    pub system_prompt: String,
    /// The Session's mount list, fixed at creation: the tools it declares,
    /// in this order. Without one, it declares every host tool and every
    /// tool a part registers (#364). Permission decides whether a declared
    /// tool's call runs, without changing what is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Tool names this Session never mounts, even when `tools` is absent or
    /// names them. The App Server uses it to keep the pre-v4 `codemode` tool
    /// out of a Session whose client cannot read child batches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_tools: Vec<String>,
    /// Fixed at creation; only the collaboration host creates members.
    #[serde(default)]
    pub collaboration_role: CollaborationRole,
    /// Fixed at creation: whether an interaction counterpart (the user
    /// through a client, or an agent in another Session or branch) can
    /// answer this Session's questions and approvals. Assembly never reads
    /// it. A stored record without it predates the field and was a client
    /// Session, so absence means `true`; only `false` is written, keeping
    /// interactive records in their earlier v13 form. The omission belongs to
    /// this serde form, which only the native store serializes; exposing
    /// `SessionConfig` anywhere else inherits it and needs its own decision.
    #[serde(
        default = "default_interactive",
        skip_serializing_if = "is_interactive"
    )]
    pub interactive: bool,
    /// The language the model writes to the operator in (model-facing prompts
    /// §7), when the operator selected one. Absent means no selection and the
    /// base rule stands. Unlike the other fields, the native store updates this
    /// member in place while the Session runs; a fork copies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<ReplyLanguage>,
    /// The title the creating caller already knew for the Session, fixed at
    /// creation. Only `sessions.launch` sets it (#1421); SessionTitle applies
    /// its text rules and writes it as the first manual title. It stays in the
    /// stored record, so a daemon that stops before that write still makes it
    /// on the next load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_title: Option<String>,
}
fn default_interactive() -> bool {
    true
}
fn is_interactive(interactive: &bool) -> bool {
    *interactive
}

/// The operator's reply-language choices, by their stable wire codes. The
/// names are the ones the operator selects, named in English for the prompt
/// text ([PromptSources §3]).
///
/// [PromptSources §3]: ../../../docs/design/plugins/PromptSources.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyLanguage {
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "zh-Hant")]
    TraditionalChinese,
    #[serde(rename = "ja")]
    Japanese,
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "id")]
    Indonesian,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
    #[serde(rename = "pt-PT")]
    PortuguesePortugal,
}
impl ReplyLanguage {
    /// Every choice, in the order the design names them.
    pub const ALL: [Self; 11] = [
        Self::English,
        Self::SimplifiedChinese,
        Self::TraditionalChinese,
        Self::Japanese,
        Self::Korean,
        Self::Indonesian,
        Self::German,
        Self::French,
        Self::Spanish,
        Self::PortugueseBrazil,
        Self::PortuguesePortugal,
    ];
    /// The stable wire code.
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
            Self::TraditionalChinese => "zh-Hant",
            Self::Japanese => "ja",
            Self::Korean => "ko",
            Self::Indonesian => "id",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::PortugueseBrazil => "pt-BR",
            Self::PortuguesePortugal => "pt-PT",
        }
    }
    /// The English name the prompt states for this choice.
    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "Simplified Chinese",
            Self::TraditionalChinese => "Traditional Chinese",
            Self::Japanese => "Japanese",
            Self::Korean => "Korean",
            Self::Indonesian => "Bahasa Indonesia",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
            Self::PortugueseBrazil => "Portuguese (Brazil)",
            Self::PortuguesePortugal => "Portuguese (Portugal)",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationRole {
    #[default]
    Leader,
    Member,
}
#[service]
impl SessionConfig {
    /// The creation-time configuration as data, for parts in other
    /// languages.
    pub fn get(&self) -> SessionConfig {
        self.clone()
    }
}
impl SessionConfig {
    /// Removes tools a connection below protocol 4 cannot project. The
    /// App Server calls this on every creation, fork and restore path; the
    /// stored `excluded_tools` keeps an old Session settled across restart.
    pub fn exclude_codemode_below(&mut self, protocol_version: u32) {
        if protocol_version < 4 && !self.excluded_tools.iter().any(|name| name == "codemode") {
            self.excluded_tools.push("codemode".into());
        }
    }
}
impl Default for SessionConfig {
    fn default() -> Self {
        let model = AgentConfig::default().model;
        Self {
            permission_policy: crate::PermissionPolicy::default(),
            cwd: ".".into(),
            model: ModelSelection {
                provider_id: model.provider_id,
                model_id: model.model_id,
                capability_snapshot: None,
                config: None,
                metadata: None,
            },
            system_prompt: String::new(),
            tools: Some(vec!["echo".into(), "wait".into(), "branch".into()]),
            excluded_tools: Vec::new(),
            collaboration_role: CollaborationRole::Leader,
            interactive: default_interactive(),
            language: None,
            initial_title: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineIdentity {
    pub session_id: String,
    pub branch_id: String,
}
#[service]
impl TimelineIdentity {
    /// The identity as data, for parts in other languages.
    pub fn get(&self) -> TimelineIdentity {
        self.clone()
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// Set only for idle instance release, never for business close.
    #[serde(skip)]
    pub unloading: tokio_util::sync::CancellationToken,
    pub session_id: String,
    /// True when this Session was loaded from storage rather than newly created.
    pub restored: bool,
}
#[service]
impl SessionIdentity {
    /// The identity as data, `{sessionId, restored}`, for parts in other
    /// languages.
    pub fn get(&self) -> SessionIdentity {
        self.clone()
    }
}

/// Phase of one branch's lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum BranchPhase {
    /// Its capabilities are loading; it may still fail and never start.
    #[default]
    Loading,
    /// Fully started and published: background work may begin.
    Active,
    Closing,
    Closed,
}

/// Phase of one Session's lifecycle, ordered like [`BranchPhase`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionPhase {
    /// Its capabilities are loading; it may still fail and never be published.
    #[default]
    Loading,
    /// Built and published by its Agent: background work may begin.
    Active,
    Closing,
    Closed,
}

/// Monotonic phase behind both lifecycles: Loading < Active < Closing <
/// Closed, and a later phase is never replaced by an earlier one.
#[derive(Clone)]
struct Phases<P>(std::sync::Arc<tokio::sync::watch::Sender<P>>);
impl<P: Copy + Ord + Default + Send + Sync + 'static> Phases<P> {
    fn new() -> Self {
        Self(std::sync::Arc::new(
            tokio::sync::watch::channel(P::default()).0,
        ))
    }
    fn get(&self) -> P {
        *self.0.borrow()
    }
    fn watch(&self) -> tokio::sync::watch::Receiver<P> {
        self.0.subscribe()
    }
    async fn reach(&self, done: impl FnMut(&P) -> bool) -> Option<P> {
        let mut phase = self.0.subscribe();
        phase.wait_for(done).await.map(|phase| *phase).ok()
    }
    fn advance(&self, next: P) {
        self.0.send_if_modified(|phase| {
            let changed = next > *phase;
            if changed {
                *phase = next;
            }
            changed
        });
    }
}

/// Lifecycle of one branch: provided by its scope, advanced only by the
/// branch's [`crate::TimelineContext`]. Capabilities that spawn background
/// work in `apply` wait for [`Self::activated`] before doing it.
#[derive(Clone)]
pub struct BranchLifecycle(Phases<BranchPhase>);
#[service]
impl BranchLifecycle {
    pub(crate) fn new() -> Self {
        Self(Phases::new())
    }
    #[service(skip)]
    pub fn phase(&self) -> BranchPhase {
        self.0.get()
    }
    #[service(skip)]
    pub fn watch(&self) -> tokio::sync::watch::Receiver<BranchPhase> {
        self.0.watch()
    }
    /// Waits until the branch leaves `Loading`: true when it became Active,
    /// false when it closes without ever starting.
    pub async fn activated(&self) -> bool {
        self.0.reach(|phase| *phase != BranchPhase::Loading).await == Some(BranchPhase::Active)
    }
    /// Waits until the branch starts closing; background work ends then,
    /// without waiting for its capability to unload.
    pub async fn closing(&self) {
        self.0.reach(|phase| *phase >= BranchPhase::Closing).await;
    }
    pub(crate) fn advance(&self, next: BranchPhase) {
        self.0.advance(next);
    }
}

/// Lifecycle of one Session, provided by its scope. Session capabilities that
/// spawn background work in `apply`, such as replacing Session-owned instances
/// or contacting external services, wait for [`Self::activated`] and end that
/// work when [`Self::closing`] returns. A Session whose assembly fails never
/// activates, and readiness of its assembly never depends on that work.
///
/// Its Agent publishes it (`Active`) and the runtime records `Closed` after
/// close or failed assembly. Closing follows the Session's close admission:
/// the phase is `Closing` from the moment its closing token is cancelled, by
/// `Session::close`, by `Agent::close` or during assembly, so no observer sees
/// a closing Session as active.
///
/// Owners of accepted detached work take a [`SessionHold`] with
/// [`Self::take_hold`], which keeps an idle release from ending that work;
/// parts in other languages use [`Self::hold`].
#[derive(Clone)]
pub struct SessionLifecycle {
    phases: Phases<SessionPhase>,
    closing: tokio_util::sync::CancellationToken,
    /// Live holds, with [`RELEASING`] set once an idle release is decided.
    holds: Arc<AtomicUsize>,
    /// The holds parts in other languages took, by id ([`Self::hold`]).
    foreign: Arc<Mutex<ForeignHolds>>,
}
/// Each hold is also a cleanup of the row that took it: the row's unload
/// runs it, or [`SessionLifecycle::release`] runs it early.
#[derive(Default)]
struct ForeignHolds {
    next: u64,
    held: std::collections::HashMap<u64, (SessionHold, rutis::Disposer)>,
}
/// Set in the hold count by the idle release decision; no hold follows it.
const RELEASING: usize = 1 << (usize::BITS - 1);
#[service]
impl SessionLifecycle {
    pub(crate) fn new(closing: tokio_util::sync::CancellationToken) -> Self {
        Self {
            phases: Phases::new(),
            closing,
            holds: Arc::default(),
            foreign: Arc::default(),
        }
    }
    #[service(skip)]
    pub fn phase(&self) -> SessionPhase {
        let phase = self.phases.get();
        if phase < SessionPhase::Closing && self.closing.is_cancelled() {
            SessionPhase::Closing
        } else {
            phase
        }
    }
    /// Waits until the Session is published or starts closing: true only when
    /// it was published and is not closing.
    pub async fn activated(&self) -> bool {
        tokio::select! {
            phase = self.phases.reach(|phase| *phase != SessionPhase::Loading) => {
                phase == Some(SessionPhase::Active) && !self.closing.is_cancelled()
            }
            _ = self.closing.cancelled() => false,
        }
    }
    /// Waits until the Session starts closing; background work ends then,
    /// without waiting for its capability to unload.
    pub async fn closing(&self) {
        tokio::select! {
            _ = self.phases.reach(|phase| *phase >= SessionPhase::Closing) => {}
            _ = self.closing.cancelled() => {}
        }
    }
    pub(crate) fn advance(&self, next: SessionPhase) {
        self.phases.advance(next);
    }
    /// Takes an activity hold for one piece of accepted detached work: while
    /// any hold exists, [`crate::Session::unload_if_idle`] refuses. Take it
    /// while the turn or lease that admitted the work still keeps the Session
    /// loaded, and drop it once the work's last fact is committed. `None` once
    /// the Session is closing or an idle release has been decided. A hold
    /// reports and cancels nothing, and never delays or refuses a close.
    #[must_use = "dropping the hold releases it"]
    #[service(skip)]
    pub fn take_hold(&self) -> Option<SessionHold> {
        if self.phase() >= SessionPhase::Closing {
            return None;
        }
        self.holds
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |holds| {
                (holds & RELEASING == 0).then_some(holds + 1)
            })
            .ok()?;
        Some(SessionHold(self.holds.clone()))
    }
    /// An activity hold for parts in other languages, as [`Self::take_hold`]
    /// takes one: the id of a hold the calling row owns. It lasts until
    /// [`Self::release`] releases it or the calling row unloads, which
    /// releases every hold the row still has. No hold (`None`, `null` in
    /// TypeScript) once the Session is closing or its idle release is
    /// decided.
    pub fn hold(&self, ctx: &rutis::Ctx) -> Option<u64> {
        let hold = self.take_hold()?;
        let mut foreign = self.foreign.lock().unwrap();
        foreign.next += 1;
        let id = foreign.next;
        let holds = Arc::downgrade(&self.foreign);
        let disposer = ctx
            .effect(move || async move {
                if let Some(holds) = holds.upgrade() {
                    holds.lock().unwrap().held.remove(&id);
                }
                Ok(())
            })
            .ok()?;
        foreign.held.insert(id, (hold, disposer));
        Some(id)
    }
    /// Releases the hold `id` took and ends its cleanup in the row that
    /// took it: true when it was held, false when it was released already
    /// or by its row's unload.
    pub async fn release(&self, id: u64) -> std::result::Result<bool, crate::AgentError> {
        let Some((hold, disposer)) = self.foreign.lock().unwrap().held.remove(&id) else {
            return Ok(false);
        };
        drop(hold);
        disposer
            .dispose()
            .await
            .map_err(crate::AgentError::Framework)?;
        Ok(true)
    }
    /// True while a hold exists or once a release has been decided.
    pub(crate) fn held(&self) -> bool {
        self.holds.load(Ordering::Acquire) != 0
    }
    /// The idle release decision: true only while no hold exists, and then
    /// every later [`Self::take_hold`] is `None`.
    pub(crate) fn refuse_holds(&self) -> bool {
        self.holds
            .compare_exchange(0, RELEASING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn resume_holds(&self) {
        self.holds.store(0, Ordering::Release);
    }
}

/// An activity hold from [`SessionLifecycle::take_hold`]; dropping it releases it.
/// It carries no identity, result or cancellation.
pub struct SessionHold(Arc<AtomicUsize>);
impl Drop for SessionHold {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
