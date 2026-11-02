use dim_agent_capabilities::methods::{RpcError, params};
use dim_agent_runtime::{ModelId, PermissionPolicy};
use dim_agent_types::{AgentEvent, InputMode, JS_MAX_SAFE_INTEGER, JsonObject, UserInput, v1};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub const PROTOCOL_VERSION: u32 = 3;
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) async fn canonical_cwd(cwd: &str) -> Result<std::path::PathBuf, RpcError> {
    if !std::path::Path::new(cwd).is_absolute() {
        return Err(RpcError::invalid("cwd must be an absolute directory"));
    }
    let path = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| RpcError::invalid("cwd is not accessible"))?;
    if !tokio::fs::metadata(&path)
        .await
        .map_err(|_| RpcError::invalid("cwd is not accessible"))?
        .is_dir()
    {
        return Err(RpcError::invalid("cwd must be a directory"));
    }
    Ok(path)
}

/// The working directory of a new Session: an accessible absolute directory,
/// canonicalized and UTF-8. `session.create` and `sessions.launch` share it.
pub(crate) async fn session_cwd(cwd: &str) -> Result<String, RpcError> {
    canonical_cwd(cwd)
        .await?
        .into_os_string()
        .into_string()
        .map_err(|_| RpcError::invalid("cwd must be UTF-8"))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(serde_json::Number),
    String(String),
}
impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::String(value) => Ok(Self::String(value)),
            Value::Number(number) => {
                let value = number.as_f64().expect("finite JSON number");
                Ok(Self::Number(
                    if value.fract() == 0.0 && value >= 0.0 && value < u64::MAX as f64 {
                        serde_json::Number::from(value as u64)
                    } else if value.fract() == 0.0 && value < 0.0 && value >= i64::MIN as f64 {
                        serde_json::Number::from(value as i64)
                    } else {
                        serde_json::Number::from_f64(value).expect("finite JSON number")
                    },
                ))
            }
            _ => Err(serde::de::Error::custom(
                "Expected a numeric or string request identity",
            )),
        }
    }
}

/// The error of an unknown or closed Agent.
pub(crate) fn agent_unknown() -> RpcError {
    RpcError::app(-32000, "agent_unknown", "Agent is closed or does not exist")
}
impl From<crate::agents::AgentOpError> for RpcError {
    fn from(error: crate::agents::AgentOpError) -> Self {
        match error {
            crate::agents::AgentOpError::Unknown(_) => agent_unknown(),
            crate::agents::AgentOpError::Build(_) => {
                Self::app(-32000, "agent_build_failed", "Agent initialization failed")
            }
            crate::agents::AgentOpError::Unload(_) => Self::wire(-32000, "Agent unload failed"),
            crate::agents::AgentOpError::Settle(_) => Self::wire(-32000, "Agent settlement failed"),
        }
    }
}

#[derive(Debug)]
pub struct Request {
    pub id: RequestId,
    pub call: Call,
}
#[derive(Debug)]
pub enum Call {
    Hello(Hello),
    /// A method no built-in decodes: a management or Session method, routed
    /// by Rpc with its raw params.
    Contributed {
        method: String,
        params: Value,
    },
    Ping,
    Shutdown(Shutdown),
    Create(Create),
    List(SessionList),
    Summaries(SessionList),
    Catalog(Catalog),
    CatalogCancel(CatalogCancel),
    Attach(Attach),
    Detach(SessionId),
    Submit(Submit),
    InputLookup(InputLookup),
    History(History),
    Stop(Stop),
    Idle(Idle),
    Close(SessionId),
    Delete(SessionId),
    Fork(Fork),
    ApprovalRespond(ApprovalRespond),
    BranchCreate(BranchCreate),
    BranchEnd(BranchEnd),
    AgentCreate(AgentCreate),
    AgentList,
    AgentClose(AgentClose),
    PermissionGet(BranchRequest),
    PermissionUpdate(UpdatePermission),
    ExecStop(StopExec),
    SessionSkills(BranchRequest),
    ModelUpdate(UpdateModel),
    PromptRefresh(BranchRequest),
    SessionMcp(SessionId),
    McpRefresh(SessionId),
    McpAppWidget(McpAppWidget),
    McpAppCall(McpAppCall),
    SessionMethods(SessionId),
    WebOpen(WebOpen),
    WebToggle(WebOpen),
    WebDisable,
    WebStatus,
    UiOpen(UiOpen),
    UiClose(UiClose),
    UiAttach(UiAttach),
    UiStatus,
    UiShutdown(UiShutdown),
    UiReplace(UiReplace),
}

impl Call {
    pub(crate) fn is_execution_session(&self) -> bool {
        matches!(
            self,
            Self::PermissionGet(_)
                | Self::PermissionUpdate(_)
                | Self::ExecStop(_)
                | Self::SessionSkills(_)
                | Self::ModelUpdate(_)
                | Self::PromptRefresh(_)
                | Self::SessionMcp(_)
                | Self::McpRefresh(_)
                | Self::McpAppWidget(_)
                | Self::McpAppCall(_)
        )
    }
    /// Methods that open and report the local web entry; local transport only.
    pub(crate) fn is_web_entry(&self) -> bool {
        matches!(
            self,
            Self::WebOpen(_) | Self::WebToggle(_) | Self::WebDisable | Self::WebStatus
        )
    }
    /// Methods of the UI registry (Tui design §3.6); local transport only.
    pub(crate) fn is_ui_host(&self) -> bool {
        matches!(
            self,
            Self::UiOpen(_)
                | Self::UiClose(_)
                | Self::UiAttach(_)
                | Self::UiStatus
                | Self::UiShutdown(_)
                | Self::UiReplace(_)
        )
    }
}

pub const WEB_ENTRY_METHODS: [&str; 4] = ["web.open", "web.toggle", "web.disable", "web.status"];
pub const UI_HOST_METHODS: [&str; 6] = [
    "ui.open",
    "ui.close",
    "ui.attach",
    "ui.status",
    "ui.shutdown",
    "ui.replace",
];

/// Management methods available to authenticated loopback Web clients.
/// Replies contain account metadata and credential presence, never credentials.
pub const WEB_MANAGEMENT_METHODS: &[&str] = &[
    "provider.management",
    "provider.create",
    "provider.update",
    "provider.probe",
    "provider.models.create",
    "provider.models.delete",
    "provider.preferences.update",
    "provider.list",
    "provider.catalog",
    "provider.refresh",
    "provider.models.set_enabled",
    "provider.models.edit",
    "provider.models.reset",
    "provider.selection",
    "provider.select",
    "provider.delete",
    "account.providers",
    "account.status",
    "account.recover",
    "account.login.begin",
    "account.login.browser.begin",
    "account.login.browser.complete",
    "account.login.status",
    "account.login.cancel",
    "account.logout",
    "account.usage",
    "account.referral",
    "account.reset_batches",
    "account.apply_reset",
    "account.codex.status",
    "account.codex.recover",
    "account.codex.login.browser.begin",
    "account.codex.login.browser.complete",
    "account.codex.login.status",
    "account.codex.login.cancel",
    "account.codex.logout",
    "account.grok.status",
    "account.grok.recover",
    "account.grok.login.begin",
    "account.grok.login.status",
    "account.grok.login.cancel",
    "account.grok.logout",
    "settings.read",
    "settings.update",
    "skills.catalog",
    "skills.set_enabled",
    "skills.refresh",
    "permission.rules",
    "permission.set_rules",
    "collaboration.model_policy",
    "collaboration.set_model_policy",
    "host.exec_shell.get",
    "host.exec_shell.update",
    "host.execution.backend",
    "mcp.catalog",
    "mcp.configuration",
    "mcp.inspect",
    "mcp.test",
    "mcp.update",
    "mcp.temporary.capabilities",
    "mcp.save",
    "mcp.remove",
    "mcp.set_enabled",
    "mcp.disconnect",
    "mcp.auth.start",
    "mcp.auth.complete",
    "mcp.auth.cancel",
    "mcp.auth.logout",
    "plugin.list",
    "plugin.install",
    "plugin.enable",
    "plugin.disable",
    "plugin.reload",
    "plugin.update",
    "plugin.remove",
];

pub const WEB_SETTINGS_SOURCES: &[&str] = &["model-log", "execution"];

pub const LOCAL_EXECUTION_METHODS: [&str; 10] = [
    "session.permission.get",
    "session.permission.update",
    "session.exec.stop",
    "session.skills",
    "session.model.update",
    "session.prompt.refresh",
    "session.mcp",
    "session.mcp.refresh",
    "session.mcp.app.widget",
    "session.mcp.app.call",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStateResult {
    pub session_id: String,
    pub servers: Vec<McpServerStatus>,
    pub catalog_error: Option<String>,
    pub closed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub id: String,
    pub state: &'static str,
    pub version: Option<String>,
    pub message: Option<String>,
    pub adopted_version: Option<String>,
    pub rejection: Option<McpRejection>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpRejection {
    pub version: String,
    pub reason: String,
}

/// `session.mcp.app.widget`: the mount read starts from the published tool
/// name a `tool/call` fact carries (MCP Apps D4).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppWidget {
    pub session_id: String,
    pub tool: String,
}
/// `session.mcp.app.call`: the server's own tool name in the entry the view
/// was mounted from (MCP Apps §4.2).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppCall {
    pub session_id: String,
    pub server: String,
    pub tool: String,
    pub arguments: JsonObject,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchRequest {
    pub session_id: String,
    pub branch_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePermission {
    pub session_id: String,
    pub branch_id: String,
    #[serde(deserialize_with = "cursor")]
    pub expected_version: u64,
    pub policy: PermissionPolicy,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopExec {
    pub session_id: String,
    pub branch_id: String,
    pub task_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateModel {
    pub session_id: String,
    pub branch_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub provider_id: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_preferences: ModelPreferenceUpdate,
}

/// One field of a Session preference update: a value sets it and
/// `{action:"clear"}` removes the Session's value.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum PreferenceUpdate {
    Value(String),
    Clear(ClearPreference),
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearPreference {
    action: ClearAction,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ClearAction {
    Clear,
}

impl ClearPreference {
    fn is_clear(&self) -> bool {
        matches!(self.action, ClearAction::Clear)
    }
}

/// The per-field patch `session.model.update` accepts. An omitted field keeps
/// its adopted value; a clear contributes no value.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelPreferenceUpdate {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub reasoning_effort: Option<PreferenceUpdate>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub service_tier: Option<PreferenceUpdate>,
}

impl ModelPreferenceUpdate {
    pub fn is_empty(&self) -> bool {
        self.reasoning_effort.is_none() && self.service_tier.is_none()
    }

    /// The values to apply; a cleared field contributes none.
    pub fn values(&self) -> crate::provider::ModelPreferences {
        crate::provider::ModelPreferences {
            reasoning_effort: self
                .reasoning_effort
                .as_ref()
                .and_then(PreferenceUpdate::value)
                .cloned(),
            service_tier: self
                .service_tier
                .as_ref()
                .and_then(PreferenceUpdate::value)
                .cloned(),
        }
    }

    pub fn clears_reasoning(&self) -> bool {
        self.reasoning_effort
            .as_ref()
            .is_some_and(PreferenceUpdate::is_clear)
    }

    pub fn clears_service_tier(&self) -> bool {
        self.service_tier
            .as_ref()
            .is_some_and(PreferenceUpdate::is_clear)
    }
}

impl PreferenceUpdate {
    fn value(&self) -> Option<&String> {
        match self {
            Self::Value(value) => Some(value),
            Self::Clear(_) => None,
        }
    }

    fn is_clear(&self) -> bool {
        matches!(self, Self::Clear(clear) if clear.is_clear())
    }
}

#[derive(Debug, Serialize)]
pub struct PermissionResult {
    pub version: u64,
    pub policy: PermissionPolicy,
}
#[derive(Debug, Serialize)]
pub struct ConfigurationResult {
    pub outcome: &'static str,
}
#[derive(Debug, Serialize)]
pub struct ModelUpdateResult {
    pub model: ModelId,
    pub outcome: &'static str,
}
/// The `session.methods` reply: the declared methods of the namespaces the
/// Session lists whose providers are active, each `{name, mutating}`.
#[derive(Debug, Serialize)]
pub struct SessionMethodsResult {
    pub methods: Vec<dim_agent_capabilities::methods::MethodDecl>,
}
#[derive(Debug, Serialize)]
pub struct SessionSkillsResult {
    pub skills: Vec<dim_agent_capabilities::skills::SkillSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    #[serde(deserialize_with = "protocol_version")]
    pub protocol_version: u32,
    pub client_kind: ClientKind,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub client_name: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Cli,
    Web,
    Other,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    #[serde(default)]
    pub force: bool,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelection {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub provider: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub model_id: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    pub cwd: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub model: Option<ModelSelection>,
    #[serde(default)]
    pub model_preferences: crate::provider::ModelPreferences,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub system_prompt: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub append_system_prompt: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub permission_policy: Option<PermissionPolicy>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub agent_id: Option<String>,
    /// Absent keeps the Agent's Session config value, interactive by default;
    /// clients send it only when `false` because older daemons reject
    /// unknown fields.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub interactive: Option<bool>,
    /// The creation origin the client declares: `client` when absent, or
    /// `acp` from an editor (#557). Any other value is refused at the boundary.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub origin: Option<SessionOrigin>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCreate {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub profile: Option<crate::agents::AgentProfile>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentClose {
    pub agent_id: String,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionList {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub agent_id: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub limit: Option<usize>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cursor: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub query: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub with_preview: bool,
    /// Whether Sessions whose origin is `internal` are listed (#557).
    #[serde(default)]
    pub with_internal: bool,
    /// Whether Sessions whose origin is `acp` are listed (#557).
    #[serde(default)]
    pub with_acp_sessions: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResult {
    pub sessions: Vec<SessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogCancel {
    pub request_id: RequestId,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionId {
    pub session_id: String,
}
/// `session.fork`: `at` is the last `main` sequence the fork inherits.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fork {
    pub session_id: String,
    #[serde(deserialize_with = "cursor")]
    pub at: u64,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attach {
    pub session_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Submit {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_submission_id")]
    pub submission_id: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub user: Option<UserInput>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub mode: Option<InputMode>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputLookup {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
    #[serde(deserialize_with = "submission_id")]
    pub submission_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
    #[serde(default, deserialize_with = "optional_cursor")]
    pub after: Option<u64>,
    #[serde(default, deserialize_with = "optional_page_limit")]
    pub limit: Option<usize>,
    #[serde(default, deserialize_with = "optional_cursor")]
    pub through: Option<u64>,
    /// The newest turn at or below `through`, newest page first
    /// (session-history-window §4.2).
    #[serde(default)]
    pub latest_turn: bool,
    /// Exclusive upper cursor of a backward page.
    #[serde(default, deserialize_with = "optional_cursor")]
    pub before: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stop {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub reason: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Idle {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
}
pub use dim_agent_types::ApprovalDecision;
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalRespond {
    pub session_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub branch_id: Option<String>,
    pub request_id: String,
    pub decision: ApprovalDecision,
}

/// `session.branch_end`: the line the user ends (design §10).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchEnd {
    pub session_id: String,
    pub branch_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchCreate {
    pub session_id: String,
    pub parent_branch_id: String,
    #[serde(deserialize_with = "cursor")]
    pub at: u64,
    pub prompt: String,
}

/// Methods still decoded by the built-in protocol cannot be shadowed by
/// runtime contributions. Session-looking names not listed here remain valid
/// contributed names (for example `session.shell`).
const BUILTIN_METHODS: [&str; 35] = [
    "hello",
    "daemon.ping",
    "daemon.shutdown",
    "web.open",
    "web.toggle",
    "web.disable",
    "web.status",
    "ui.open",
    "ui.close",
    "ui.attach",
    "ui.status",
    "ui.shutdown",
    "ui.replace",
    "session.create",
    "session.list",
    "session.summaries",
    "session.catalog",
    "session.catalog.cancel",
    "session.attach",
    "session.detach",
    "session.submit",
    "session.input_lookup",
    "session.history",
    "session.stop",
    "session.idle",
    "session.close",
    "session.delete",
    "session.fork",
    "approval.respond",
    "session.branch_create",
    "session.branch_end",
    "agent.create",
    "agent.list",
    "agent.close",
    "session.methods",
];

/// Every exact built-in method name.
pub(crate) fn builtin_methods() -> impl Iterator<Item = &'static str> {
    BUILTIN_METHODS.into_iter().chain(LOCAL_EXECUTION_METHODS)
}

pub(crate) fn is_builtin_method(method: &str) -> bool {
    builtin_methods().any(|builtin| builtin == method)
}

/// Validates the envelope and built-in calls. Runtime contributions retain raw
/// params until the owning adapter validates its concrete request type.
pub fn decode(bytes: &[u8]) -> Result<Request, (Option<RequestId>, RpcError)> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| (None, RpcError::wire(-32700, "Invalid JSON")))?;
    let Value::Object(mut obj) = value else {
        return Err((None, RpcError::wire(-32600, "Expected a JSON-RPC request")));
    };
    let id = obj
        .remove("id")
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok());
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || id.is_none() {
        return Err((id, RpcError::wire(-32600, "Invalid JSON-RPC envelope")));
    }
    let id = id.expect("checked request identity");
    let method = obj
        .remove("method")
        .and_then(|value| match value {
            Value::String(method) => Some(method),
            _ => None,
        })
        .ok_or_else(|| (Some(id.clone()), RpcError::wire(-32600, "Expected method")))?;
    let raw_params = obj
        .remove("params")
        .unwrap_or_else(|| Value::Object(Default::default()));
    let result = (|| -> Result<Call, RpcError> {
        Ok(match method.as_str() {
            "hello" => Call::Hello(params(raw_params)?),
            "daemon.ping" => {
                let _: Empty = params(raw_params)?;
                Call::Ping
            }
            "daemon.shutdown" => Call::Shutdown(params(raw_params)?),
            "session.create" => Call::Create(params(raw_params)?),
            "session.list" => Call::List(params(raw_params)?),
            "session.summaries" => Call::Summaries(params(raw_params)?),
            "session.catalog" => Call::Catalog(params(raw_params)?),
            "session.catalog.cancel" => Call::CatalogCancel(params(raw_params)?),
            "session.attach" => Call::Attach(params(raw_params)?),
            "session.detach" => Call::Detach(params(raw_params)?),
            "session.submit" => Call::Submit(params(raw_params)?),
            "session.input_lookup" => Call::InputLookup(params(raw_params)?),
            "session.history" => Call::History(params(raw_params)?),
            "session.stop" => Call::Stop(params(raw_params)?),
            "session.idle" => Call::Idle(params(raw_params)?),
            "session.close" => Call::Close(params(raw_params)?),
            "session.delete" => Call::Delete(params(raw_params)?),
            "session.fork" => Call::Fork(params(raw_params)?),
            "approval.respond" => Call::ApprovalRespond(params(raw_params)?),
            "session.branch_create" => Call::BranchCreate(params(raw_params)?),
            "session.branch_end" => Call::BranchEnd(params(raw_params)?),
            "agent.create" => Call::AgentCreate(params(raw_params)?),
            "agent.list" => {
                let _: Empty = params(raw_params)?;
                Call::AgentList
            }
            "agent.close" => Call::AgentClose(params(raw_params)?),
            "session.permission.get" => Call::PermissionGet(params(raw_params)?),
            "session.permission.update" => Call::PermissionUpdate(params(raw_params)?),
            "session.exec.stop" => Call::ExecStop(params(raw_params)?),
            "session.skills" => Call::SessionSkills(params(raw_params)?),
            "session.model.update" => Call::ModelUpdate(params(raw_params)?),
            "session.prompt.refresh" => Call::PromptRefresh(params(raw_params)?),
            "session.mcp" => Call::SessionMcp(params(raw_params)?),
            "session.mcp.refresh" => Call::McpRefresh(params(raw_params)?),
            "session.mcp.app.widget" => Call::McpAppWidget(params(raw_params)?),
            "session.mcp.app.call" => Call::McpAppCall(params(raw_params)?),
            "session.methods" => Call::SessionMethods(params(raw_params)?),
            "web.open" => Call::WebOpen(params(raw_params)?),
            "web.toggle" => Call::WebToggle(params(raw_params)?),
            "web.disable" => {
                let _: Empty = params(raw_params)?;
                Call::WebDisable
            }
            "web.status" => {
                let _: Empty = params(raw_params)?;
                Call::WebStatus
            }
            "ui.open" => Call::UiOpen(params(raw_params)?),
            "ui.close" => Call::UiClose(params(raw_params)?),
            "ui.attach" => Call::UiAttach(params(raw_params)?),
            "ui.status" => {
                let _: Empty = params(raw_params)?;
                Call::UiStatus
            }
            "ui.shutdown" => Call::UiShutdown(params(raw_params)?),
            "ui.replace" => Call::UiReplace(params(raw_params)?),
            _ => Call::Contributed {
                method,
                params: raw_params,
            },
        })
    })();
    result
        .map(|call| Request {
            id: id.clone(),
            call,
        })
        .map_err(|error| (Some(id), error))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebOpen {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub port: Option<u16>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebOpened {
    /// Carries the bootstrap token in its fragment; never logged.
    pub url: String,
    pub address: std::net::SocketAddr,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebStatus {
    pub enabled: bool,
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<std::net::SocketAddr>,
    pub connections: usize,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebToggled {
    pub enabled: bool,
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<std::net::SocketAddr>,
}
/// `ui.attach`: the calling connection as one interface of a Session it is
/// attached to (Tui design §3.6).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiAttach {
    pub session_id: String,
    pub kind: String,
    /// Present for a UI leaf: its id and the attach token from `ui.open`'s
    /// reply.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub ui_id: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub token: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiAttached {
    pub ui_id: String,
    pub generation: u64,
}
/// `ui.open`: open a session with a UI leaf, which runs its own `Tui`
/// (Tui design §3.6).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiOpen {
    pub session_id: String,
    pub kind: String,
    /// The leaf's node id; it becomes the `uiId`.
    pub peer: String,
    /// Where the leaf waits for the daemon's link.
    pub address: String,
    /// An interface of the same Session to close once this leaf attaches.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub replace: Option<String>,
    /// The generation `replace` must still have.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub expected_generation: Option<u64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiOpened {
    pub ui_id: String,
    pub generation: u64,
    /// The one-time token the leaf's `Tui` passes to `ui.attach`.
    pub token: String,
}
/// `ui.close`: remove a registration; a leaf's session is closed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiClose {
    pub ui_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub reason: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLeave {
    pub ui_id: String,
    pub token: String,
    pub exit_code: i32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiShutdown {
    pub ui_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub reason: Option<String>,
}
/// `ui.replace`: the caller registers on `session_id` and the old `ui_id`
/// is asked to leave.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiReplace {
    pub ui_id: String,
    pub session_id: String,
    pub kind: String,
    /// The old generation the caller observed. A replacement that lost a
    /// race is refused rather than closing a newer interface.
    #[serde(default, deserialize_with = "optional_non_null")]
    pub generation: Option<u64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiInstance {
    pub ui_id: String,
    pub session_id: String,
    pub kind: String,
    pub since_ms: u64,
    pub generation: u64,
    /// `online` for a connection registration; a leaf's `starting`,
    /// `online` or `replacing` otherwise.
    pub state: &'static str,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStatusResult {
    pub instances: Vec<UiInstance>,
    /// Browser connections on the web entry: counted, not registered.
    pub web_connections: usize,
}
/// `ui.shutdown`, sent on the interface's own connection.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiShutdownNotice {
    pub ui_id: String,
    pub reason: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub instance_id: String,
    pub server_version: &'static str,
    pub protocol_version: u32,
    pub daemon_profile: DaemonProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum DaemonProfile {
    Product,
    Configured {
        #[serde(rename = "modelConfigSha256")]
        model_config_sha256: String,
    },
    Mock,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    pub sessions: usize,
    pub connections: usize,
    pub server_version: &'static str,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub session_id: String,
    pub model: ModelId,
    /// The preferences the Session's adopted configuration sends, derived from
    /// it; only on this reply, not on `session.list` items, and absent when it
    /// carries none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<dim_agent_types::ModelPreferences>,
    pub sequence: u64,
}
/// The fork's id and its `main` head once it is published.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkResult {
    pub session_id: String,
    pub sequence: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrigin {
    Client,
    Acp,
    Internal,
    Unknown,
}
impl SessionOrigin {
    /// The value the stored Session record holds for this origin.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Acp => "acp",
            Self::Internal => "internal",
            Self::Unknown => "unknown",
        }
    }
    /// The origin a stored value names; `None` for a value this build does not
    /// know, which the column's `CHECK` and the store's format gate keep out.
    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "client" => Some(Self::Client),
            "acp" => Some(Self::Acp),
            "internal" => Some(Self::Internal),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub model: ModelId,
    pub created_at: u64,
    pub running: bool,
    pub sequence: u64,
    pub attached_clients: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The Session's creation origin; protocol v3 with a native store (#557).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
    /// The leader Session a Collaboration member belongs to; absent when the
    /// store records none (#557).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// `title`, `titleSource` and `titleRevision` from SessionStore (S5b);
    /// protocol v3 only.
    #[serde(flatten)]
    pub title: Option<crate::storage::TitleFields>,
}
/// The `session.attach` reply: the attached Session's directory record, as
/// `session.list` gives it, plus the preferences its adopted configuration
/// sends. Only this reply and `session.create` name them; list items do not.
#[derive(Debug, Serialize)]
pub struct AttachResult {
    #[serde(flatten)]
    pub session: SessionInfo,
    #[serde(rename = "modelPreferences", skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<dim_agent_types::ModelPreferences>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    #[serde(flatten)]
    pub session: SessionInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub updated_at: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCreated {
    pub agent_id: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub agent_id: String,
    pub default: bool,
    pub name: String,
    pub description: String,
    pub sessions: usize,
}
#[derive(Debug, Serialize)]
pub struct SubmitResult {
    pub event: Arc<AgentEvent>,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum InputLookupResult {
    Accepted { event: Arc<AgentEvent> },
    Unknown,
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Reply {
    Hello(HelloResult),
    Contributed(Value),
    Ping(PingResult),
    Create(CreateResult),
    Fork(ForkResult),
    List(Vec<SessionInfo>),
    /// The attached Session's directory record, with `model` and
    /// `modelPreferences` from its latest adopted configuration, falling back
    /// to the creation one; `session.list` items keep the creation model until
    /// #795.
    Attach(AttachResult),
    Summaries(Vec<SessionSummary>),
    Catalog(CatalogResult),
    Submit(SubmitResult),
    InputLookup(InputLookupResult),
    History(crate::tree_protocol::HistoryResult),
    BranchCreate(crate::tree_protocol::CreateResult),
    Barrier(crate::tree_protocol::BarrierResult),
    AgentCreated(AgentCreated),
    Agents(Vec<AgentInfo>),
    Permission(PermissionResult),
    ExecEnded(dim_agent_types::ExecEndedData),
    SessionSkills(SessionSkillsResult),
    ModelUpdate(ModelUpdateResult),
    Configuration(ConfigurationResult),
    McpState(McpStateResult),
    McpWidget(crate::mcp::McpWidgetResult),
    McpAppCall(crate::mcp::McpAppCallResult),
    SessionMethods(SessionMethodsResult),
    WebOpened(WebOpened),
    WebToggled(WebToggled),
    WebStatus(WebStatus),
    UiOpened(UiOpened),
    UiAttached(UiAttached),
    UiStatus(UiStatusResult),
    Empty(Empty),
}
impl Reply {
    pub fn empty() -> Self {
        Self::Empty(Empty {})
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalCall {
    pub name: String,
    pub input: JsonObject,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub session_id: String,
    pub request_id: String,
    pub call: ApprovalCall,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCancelled {
    pub session_id: String,
    pub request_id: String,
    pub reason: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNotice {
    pub session_id: String,
    pub event: v1::SharedEvent,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateNotice {
    pub session_id: String,
    pub running: bool,
    pub stopping: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedNotice {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
/// Progress of the contributed call `id` (methods contract A.2), sent only
/// to the connection that made the call and before its reply.
#[derive(Debug, Serialize)]
pub struct ProgressNotice {
    pub id: RequestId,
    pub method: String,
    pub data: Value,
}
#[derive(Debug, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    #[serde(rename = "session.mcp.state")]
    McpState(McpStateResult),
    #[serde(rename = "ui.shutdown")]
    UiShutdown(UiShutdownNotice),
    #[serde(rename = "session.event")]
    Event(EventNotice),
    #[serde(rename = "session.state")]
    State(StateNotice),
    #[serde(rename = "approval.request")]
    ApprovalRequest(ApprovalRequest),
    #[serde(rename = "approval.cancelled")]
    ApprovalCancelled(ApprovalCancelled),
    #[serde(rename = "session.closed")]
    Closed(ClosedNotice),
    #[serde(rename = "session.event")]
    TreeEvent(crate::tree_protocol::EventNotice),
    #[serde(rename = "session.state")]
    BranchState(crate::tree_protocol::StateNotice),
    #[serde(rename = "approval.request")]
    TreeApprovalRequest(crate::tree_protocol::ApprovalRequest),
    #[serde(rename = "approval.cancelled")]
    TreeApprovalCancelled(crate::tree_protocol::ApprovalCancelled),
    #[serde(rename = "approval.resolved")]
    TreeApprovalResolved(crate::tree_protocol::ApprovalResolved),
    #[serde(rename = "session.model.progress")]
    ModelProgress(crate::tree_protocol::ModelProgressNotice),
    #[serde(rename = "session.notice")]
    Notice(crate::tree_protocol::ForwardedNotice),
    #[serde(rename = "management.progress")]
    Progress(ProgressNotice),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outbound {
    Success {
        jsonrpc: &'static str,
        id: RequestId,
        result: Reply,
    },
    Error {
        jsonrpc: &'static str,
        id: Option<RequestId>,
        error: RpcError,
    },
    Notice {
        jsonrpc: &'static str,
        #[serde(flatten)]
        notification: Notification,
    },
}
impl Outbound {
    pub fn result(id: RequestId, result: Result<Reply, RpcError>) -> Self {
        match result {
            Ok(result) => Self::Success {
                jsonrpc: "2.0",
                id,
                result,
            },
            Err(error) => Self::Error {
                jsonrpc: "2.0",
                id: Some(id),
                error,
            },
        }
    }
    pub fn notice(notification: Notification) -> Self {
        Self::Notice {
            jsonrpc: "2.0",
            notification,
        }
    }
}

fn cursor<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    optional_cursor(deserializer).map(|value| value.expect("cursor parser returns a number"))
}

fn page_limit<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let value = cursor(deserializer)?;
    if value == 0 {
        return Err(serde::de::Error::custom("Expected a positive page limit"));
    }
    usize::try_from(value).map_err(serde::de::Error::custom)
}

fn optional_page_limit<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<usize>, D::Error> {
    page_limit(deserializer).map(Some)
}

fn submission_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    uuid::Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn optional_submission_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    submission_id(deserializer).map(Some)
}

fn optional_cursor<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    let value = number
        .as_f64()
        .ok_or_else(|| serde::de::Error::custom("Expected cursor"))?;
    if value < 0.0 || value > JS_MAX_SAFE_INTEGER as f64 || value.fract() != 0.0 {
        return Err(serde::de::Error::custom(
            "Expected a nonnegative safe integer cursor",
        ));
    }
    Ok(Some(value as u64))
}

fn protocol_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    let value = number
        .as_f64()
        .ok_or_else(|| serde::de::Error::custom("Expected protocol version"))?;
    if value < 0.0 || value > u32::MAX as f64 || value.fract() != 0.0 {
        return Err(serde::de::Error::custom(
            "Expected an integer protocol version",
        ));
    }
    Ok(value as u32)
}

pub(crate) fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
