use dim_agent_runtime::{AgentError, ModelId, PermissionPolicy};
use dim_agent_types::{AgentEvent, InputMode, JS_MAX_SAFE_INTEGER, JsonObject, UserInput, v1};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::sync::Arc;

pub const PROTOCOL_VERSION: u32 = 3;
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(serde_json::Number),
    String(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    pub app_error: &'static str,
}
impl RpcError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::app(-32602, "invalid_params", message)
    }
    pub fn app(code: i32, app_error: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(ErrorData { app_error }),
        }
    }
    pub fn wire(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    /// The server service is reloading; the request did not run and may be retried.
    pub fn reloading() -> Self {
        Self::app(
            -32000,
            "service_reloading",
            "Server service is reloading; retry",
        )
    }
    pub fn is_closed(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|data| data.app_error == "session_closed")
    }
    pub fn closed() -> Self {
        Self::app(
            -32000,
            "session_closed",
            "Session is closed or does not exist",
        )
    }
    pub fn agent_unknown() -> Self {
        Self::app(-32000, "agent_unknown", "Agent is closed or does not exist")
    }
}
impl From<AgentError> for RpcError {
    fn from(error: AgentError) -> Self {
        let app = match &error {
            AgentError::InvalidInput(_) | AgentError::UnknownBranch(_) => "session_invalid_input",
            AgentError::Closed(_) => "session_closed",
            AgentError::Build(_) => {
                return Self::app(
                    -32000,
                    "session_build_failed",
                    "Session initialization failed",
                );
            }
            AgentError::Reloading(_) => return Self::reloading(),
            _ => return Self::wire(-32603, "Session operation failed"),
        };
        Self::app(-32000, app, error.to_string())
    }
}
impl From<crate::agents::AgentOpError> for RpcError {
    fn from(error: crate::agents::AgentOpError) -> Self {
        match error {
            crate::agents::AgentOpError::Unknown(_) => {
                Self::app(-32000, "agent_unknown", "Agent is closed or does not exist")
            }
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
    Ping,
    Shutdown(Shutdown),
    Create(Create),
    List(SessionList),
    Summaries(SessionList),
    Attach(Attach),
    Detach(SessionId),
    Submit(Submit),
    InputLookup(InputLookup),
    History(History),
    Stop(Stop),
    Idle(Idle),
    Close(SessionId),
    ApprovalRespond(ApprovalRespond),
    BranchCreate(BranchCreate),
    AgentCreate(AgentCreate),
    AgentList,
    AgentClose(AgentClose),
    AccountProviders,
    AccountStatus(ProviderTarget),
    AccountRecover(ProviderTarget),
    LoginBegin(LoginBegin),
    BrowserLoginBegin(BrowserLoginBegin),
    BrowserLoginComplete(ManagedBrowserLoginComplete),
    LoginStatus(ManagedLoginAttempt),
    LoginCancel(ManagedLoginAttempt),
    Logout(ProviderTarget),
    CodexAccountStatus,
    CodexAccountRecover,
    CodexBrowserLoginBegin,
    CodexBrowserLoginComplete(BrowserLoginComplete),
    CodexLoginStatus(LoginAttempt),
    CodexLoginCancel(LoginAttempt),
    CodexLogout,
    GrokAccountStatus,
    GrokAccountRecover,
    GrokLoginBegin,
    GrokLoginStatus(LoginAttempt),
    GrokLoginCancel(LoginAttempt),
    GrokLogout,
    ProviderCatalog(ProviderTarget),
    ProviderRefresh(ProviderTarget),
    ProviderModelsSetEnabled(SetEnabledModels),
    ProviderModelEdit(EditModel),
    ProviderModelReset(ResetModel),
    ProviderSelection(SelectionScope),
    ProviderSelect(SelectModel),
    ProviderList,
    ProviderSave(SaveProvider),
    ProviderDelete(DeleteProvider),
    ProviderImportLegacy(ImportLegacyProviders),
    PermissionGet(BranchRequest),
    PermissionUpdate(UpdatePermission),
    ExecStop(StopExec),
    SkillsCatalog(SelectionScope),
    SkillSetEnabled(SetSkillEnabled),
    SkillsRefresh(SelectionScope),
    SessionSkills(BranchRequest),
    ModelUpdate(UpdateModel),
    PromptRefresh(BranchRequest),
    McpCatalog(SelectionScope),
    McpSetEnabled(SetMcpEnabled),
    McpSave(SaveMcp),
    McpRemove(RemoveMcp),
    McpDisconnect(McpTarget),
    McpApprove(ApproveMcp),
    McpRevoke(RevokeMcp),
    SessionMcp(SessionId),
    McpRefresh(SessionId),
    WebOpen(WebOpen),
    WebStatus,
    WebClose,
}

impl Call {
    pub(crate) fn is_management(&self) -> bool {
        matches!(
            self,
            Self::AccountProviders
                | Self::AccountStatus(_)
                | Self::AccountRecover(_)
                | Self::LoginBegin(_)
                | Self::BrowserLoginBegin(_)
                | Self::BrowserLoginComplete(_)
                | Self::LoginStatus(_)
                | Self::LoginCancel(_)
                | Self::Logout(_)
                | Self::CodexAccountStatus
                | Self::CodexAccountRecover
                | Self::CodexBrowserLoginBegin
                | Self::CodexBrowserLoginComplete(_)
                | Self::CodexLoginStatus(_)
                | Self::CodexLoginCancel(_)
                | Self::CodexLogout
                | Self::GrokAccountStatus
                | Self::GrokAccountRecover
                | Self::GrokLoginBegin
                | Self::GrokLoginStatus(_)
                | Self::GrokLoginCancel(_)
                | Self::GrokLogout
                | Self::ProviderCatalog(_)
                | Self::ProviderRefresh(_)
                | Self::ProviderModelsSetEnabled(_)
                | Self::ProviderModelEdit(_)
                | Self::ProviderModelReset(_)
                | Self::ProviderSelection(_)
                | Self::ProviderSelect(_)
                | Self::ProviderList
                | Self::ProviderSave(_)
                | Self::ProviderDelete(_)
                | Self::ProviderImportLegacy(_)
                | Self::SkillsCatalog(_)
                | Self::SkillSetEnabled(_)
                | Self::SkillsRefresh(_)
                | Self::McpCatalog(_)
                | Self::McpSetEnabled(_)
                | Self::McpSave(_)
                | Self::McpRemove(_)
                | Self::McpDisconnect(_)
                | Self::McpApprove(_)
                | Self::McpRevoke(_)
        )
    }

    pub(crate) fn is_execution_management(&self) -> bool {
        matches!(
            self,
            Self::SkillsCatalog(_)
                | Self::SkillSetEnabled(_)
                | Self::SkillsRefresh(_)
                | Self::McpCatalog(_)
                | Self::McpSetEnabled(_)
                | Self::McpSave(_)
                | Self::McpRemove(_)
                | Self::McpDisconnect(_)
                | Self::McpApprove(_)
                | Self::McpRevoke(_)
        )
    }

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
        )
    }
    /// Methods that open and close the local web entry; local transport only.
    pub(crate) fn is_web_entry(&self) -> bool {
        matches!(self, Self::WebOpen(_) | Self::WebStatus | Self::WebClose)
    }
    /// Management reads served to web entry connections; replies carry no credentials.
    pub(crate) fn is_web_readable(&self) -> bool {
        matches!(
            self,
            Self::ProviderList | Self::ProviderCatalog(_) | Self::ProviderSelection(_)
        )
    }
}

pub const WEB_ENTRY_METHODS: [&str; 3] = ["web.open", "web.status", "web.close"];

/// Methods a web entry connection may call beyond the Session built-ins.
pub const WEB_SESSION_METHODS: [&str; 11] = [
    "session.permission.get",
    "session.permission.update",
    "session.exec.stop",
    "session.skills",
    "session.model.update",
    "session.prompt.refresh",
    "session.mcp",
    "session.mcp.refresh",
    "provider.list",
    "provider.catalog",
    "provider.selection",
];

pub const LOCAL_EXECUTION_METHODS: [&str; 9] = [
    "session.permission.get",
    "session.permission.update",
    "session.exec.stop",
    "skills.catalog",
    "skills.set_enabled",
    "skills.refresh",
    "session.skills",
    "session.model.update",
    "session.prompt.refresh",
];

pub const MCP_METHODS: [&str; 9] = [
    "mcp.catalog",
    "mcp.set_enabled",
    "mcp.save",
    "mcp.remove",
    "mcp.disconnect",
    "mcp.approve",
    "mcp.revoke",
    "session.mcp",
    "session.mcp.refresh",
];

pub const MANAGEMENT_METHODS: [&str; 33] = [
    "account.providers",
    "account.status",
    "account.recover",
    "account.login.begin",
    "account.login.browser.begin",
    "account.login.browser.complete",
    "account.login.status",
    "account.login.cancel",
    "account.logout",
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
    "provider.catalog",
    "provider.refresh",
    "provider.models.set_enabled",
    "provider.models.edit",
    "provider.models.reset",
    "provider.selection",
    "provider.select",
    "provider.list",
    "provider.save",
    "provider.delete",
    "provider.import_legacy",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportLegacyProviders {
    pub source_path: std::path::PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProvider {
    pub provider: crate::provider::ProviderInput,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub expected_version: Option<u32>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProvider {
    pub provider_id: String,
    pub expected_version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginBegin {
    pub provider_id: String,
    pub directory: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub email: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLoginBegin {
    pub provider_id: String,
    pub directory: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub email: Option<String>,
    pub redirect_uri: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedBrowserLoginComplete {
    pub provider_id: String,
    pub attempt_id: String,
    pub callback_url: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedLoginAttempt {
    pub provider_id: String,
    pub attempt_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserLoginComplete {
    pub attempt_id: String,
    pub callback_url: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetEnabledModels {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_ids: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTarget {
    pub provider_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditModel {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_id: String,
    pub edit: crate::model_catalog::ModelEdit,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResetModel {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginAttempt {
    pub attempt_id: String,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionScope {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    User,
    Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveMcp {
    pub cwd: String,
    pub id: String,
    pub approval_token: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeMcp {
    pub cwd: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTarget {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetMcpEnabled {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
    pub enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
    pub configuration: crate::settings::McpServerSettings,
}
impl std::fmt::Debug for SaveMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveMcp").finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveMcp {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub source: McpScope,
    pub expected_version: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogResult {
    pub user_revision: String,
    pub project_revision: Option<String>,
    pub user_application: crate::mcp::McpApplication,
    pub project_application: Option<crate::mcp::McpApplication>,
    pub servers: Vec<McpCatalogEntry>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogEntry {
    pub id: String,
    pub source: McpScope,
    pub enabled: bool,
    pub transport: &'static str,
    pub approval: crate::mcp::McpApproval,
    pub approval_token: Option<String>,
}
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectModel {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub expected_version: Option<u32>,
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
pub struct SetSkillEnabled {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub enabled: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateModel {
    pub session_id: String,
    pub branch_id: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub model_preferences: crate::provider::ModelPreferences,
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
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub allowed_tools: Vec<String>,
    pub enabled: bool,
    pub shadowed_by: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct SkillIssue {
    pub path: String,
    pub message: String,
}
#[derive(Debug, Serialize)]
pub struct SkillsCatalogResult {
    pub version: u64,
    pub entries: Vec<SkillEntry>,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
pub struct SkillsRefreshResult {
    pub version: u64,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
pub struct SkillsVersion {
    pub version: u64,
}
#[derive(Debug, Serialize)]
pub struct SessionSkillsResult {
    pub skills: Vec<dim_agent_capabilities::skills::SkillSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserLogin {
    pub attempt_id: String,
    pub authorization_url: String,
    pub expires_at: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLogin {
    pub attempt_id: String,
    pub verification_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub expires_at: u64,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ManagedLoginState {
    Pending,
    Completed,
    Failed { error: String },
    Cancelled,
    Expired,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LoginState {
    Pending,
    Completed {
        account: crate::managed_provider::AccountSnapshot,
    },
    Failed {
        error: String,
    },
    Cancelled,
    Expired,
}

#[derive(Debug, Serialize)]
pub struct CatalogResult {
    pub catalog: Option<crate::provider::ProviderCatalog>,
}
#[derive(Debug, Serialize)]
pub struct SelectionResult {
    pub selection: Option<crate::provider::ProviderSelection>,
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
#[serde(untagged)]
pub enum TraceOption {
    Enabled(bool),
    Path(String),
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
    pub trace: Option<TraceOption>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub agent_id: Option<String>,
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
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionId {
    pub session_id: String,
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
    #[serde(deserialize_with = "cursor")]
    pub after: u64,
    #[serde(deserialize_with = "page_limit")]
    pub limit: usize,
    #[serde(default, deserialize_with = "optional_cursor")]
    pub through: Option<u64>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchCreate {
    pub session_id: String,
    pub parent_branch_id: String,
    #[serde(deserialize_with = "cursor")]
    pub at: u64,
    pub prompt: String,
}

/// This is the external boundary; validated requests never carry untyped params.
pub fn decode(bytes: &[u8]) -> Result<Request, (Option<RequestId>, RpcError)> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| (None, RpcError::wire(-32700, "Invalid JSON")))?;
    let Value::Object(mut obj) = value else {
        return Err((None, RpcError::wire(-32600, "Expected a JSON-RPC request")));
    };
    let id = obj
        .remove("id")
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok())
        .map(|id| match id {
            RequestId::Number(number) => {
                let number = number.as_f64().expect("finite JSON number");
                RequestId::Number(
                    if number.fract() == 0.0 && number >= 0.0 && number < (u64::MAX as f64) {
                        serde_json::Number::from(number as u64)
                    } else if number.fract() == 0.0 && number < 0.0 && number >= i64::MIN as f64 {
                        serde_json::Number::from(number as i64)
                    } else {
                        serde_json::Number::from_f64(number).expect("finite JSON number")
                    },
                )
            }
            id => id,
        });
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
    fn params<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|_| RpcError::invalid("Invalid request parameters"))
    }
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
            "session.attach" => Call::Attach(params(raw_params)?),
            "session.detach" => Call::Detach(params(raw_params)?),
            "session.submit" => Call::Submit(params(raw_params)?),
            "session.input_lookup" => Call::InputLookup(params(raw_params)?),
            "session.history" => Call::History(params(raw_params)?),
            "session.stop" => Call::Stop(params(raw_params)?),
            "session.idle" => Call::Idle(params(raw_params)?),
            "session.close" => Call::Close(params(raw_params)?),
            "approval.respond" => Call::ApprovalRespond(params(raw_params)?),
            "session.branch_create" => Call::BranchCreate(params(raw_params)?),
            "agent.create" => Call::AgentCreate(params(raw_params)?),
            "agent.list" => {
                let _: Empty = params(raw_params)?;
                Call::AgentList
            }
            "agent.close" => Call::AgentClose(params(raw_params)?),
            "account.providers" => {
                let _: Empty = params(raw_params)?;
                Call::AccountProviders
            }
            "account.status" => Call::AccountStatus(params(raw_params)?),
            "account.recover" => Call::AccountRecover(params(raw_params)?),
            "account.login.begin" => Call::LoginBegin(params(raw_params)?),
            "account.login.browser.begin" => Call::BrowserLoginBegin(params(raw_params)?),
            "account.login.browser.complete" => Call::BrowserLoginComplete(params(raw_params)?),
            "account.login.status" => Call::LoginStatus(params(raw_params)?),
            "account.login.cancel" => Call::LoginCancel(params(raw_params)?),
            "account.logout" => Call::Logout(params(raw_params)?),
            "account.codex.status" => {
                let _: Empty = params(raw_params)?;
                Call::CodexAccountStatus
            }
            "account.codex.recover" => {
                let _: Empty = params(raw_params)?;
                Call::CodexAccountRecover
            }
            "account.codex.login.browser.begin" => {
                let _: Empty = params(raw_params)?;
                Call::CodexBrowserLoginBegin
            }
            "account.codex.login.browser.complete" => {
                Call::CodexBrowserLoginComplete(params(raw_params)?)
            }
            "account.codex.login.status" => Call::CodexLoginStatus(params(raw_params)?),
            "account.codex.login.cancel" => Call::CodexLoginCancel(params(raw_params)?),
            "account.codex.logout" => {
                let _: Empty = params(raw_params)?;
                Call::CodexLogout
            }
            "account.grok.status" => {
                let _: Empty = params(raw_params)?;
                Call::GrokAccountStatus
            }
            "account.grok.recover" => {
                let _: Empty = params(raw_params)?;
                Call::GrokAccountRecover
            }
            "account.grok.login.begin" => {
                let _: Empty = params(raw_params)?;
                Call::GrokLoginBegin
            }
            "account.grok.login.status" => Call::GrokLoginStatus(params(raw_params)?),
            "account.grok.login.cancel" => Call::GrokLoginCancel(params(raw_params)?),
            "account.grok.logout" => {
                let _: Empty = params(raw_params)?;
                Call::GrokLogout
            }
            "provider.catalog" => Call::ProviderCatalog(params(raw_params)?),
            "provider.refresh" => Call::ProviderRefresh(params(raw_params)?),
            "provider.models.set_enabled" => Call::ProviderModelsSetEnabled(params(raw_params)?),
            "provider.models.edit" => Call::ProviderModelEdit(params(raw_params)?),
            "provider.models.reset" => Call::ProviderModelReset(params(raw_params)?),
            "provider.selection" => Call::ProviderSelection(params(raw_params)?),
            "provider.select" => Call::ProviderSelect(params(raw_params)?),
            "provider.list" => {
                let _: Empty = params(raw_params)?;
                Call::ProviderList
            }
            "provider.save" => Call::ProviderSave(params(raw_params)?),
            "provider.delete" => Call::ProviderDelete(params(raw_params)?),
            "provider.import_legacy" => Call::ProviderImportLegacy(params(raw_params)?),
            "session.permission.get" => Call::PermissionGet(params(raw_params)?),
            "session.permission.update" => Call::PermissionUpdate(params(raw_params)?),
            "session.exec.stop" => Call::ExecStop(params(raw_params)?),
            "skills.catalog" => Call::SkillsCatalog(params(raw_params)?),
            "skills.set_enabled" => Call::SkillSetEnabled(params(raw_params)?),
            "skills.refresh" => Call::SkillsRefresh(params(raw_params)?),
            "session.skills" => Call::SessionSkills(params(raw_params)?),
            "session.model.update" => Call::ModelUpdate(params(raw_params)?),
            "session.prompt.refresh" => Call::PromptRefresh(params(raw_params)?),
            "mcp.catalog" => Call::McpCatalog(params(raw_params)?),
            "mcp.set_enabled" => Call::McpSetEnabled(params(raw_params)?),
            "mcp.save" => Call::McpSave(params(raw_params)?),
            "mcp.remove" => Call::McpRemove(params(raw_params)?),
            "mcp.disconnect" => Call::McpDisconnect(params(raw_params)?),
            "mcp.approve" => Call::McpApprove(params(raw_params)?),
            "mcp.revoke" => Call::McpRevoke(params(raw_params)?),
            "session.mcp" => Call::SessionMcp(params(raw_params)?),
            "session.mcp.refresh" => Call::McpRefresh(params(raw_params)?),
            "web.open" => Call::WebOpen(params(raw_params)?),
            "web.status" => {
                let _: Empty = params(raw_params)?;
                Call::WebStatus
            }
            "web.close" => {
                let _: Empty = params(raw_params)?;
                Call::WebClose
            }
            _ => return Err(RpcError::app(-32601, "unknown_method", "Unknown method")),
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
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<std::net::SocketAddr>,
    pub connections: usize,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub instance_id: String,
    pub server_version: &'static str,
    pub protocol_version: u32,
    pub daemon_profile: DaemonProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<&'static str>>,
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
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_path: Option<String>,
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
    Ping(PingResult),
    Create(CreateResult),
    List(Vec<SessionInfo>),
    Summaries(Vec<SessionSummary>),
    Submit(SubmitResult),
    InputLookup(InputLookupResult),
    History(crate::tree_protocol::HistoryResult),
    BranchCreate(crate::tree_protocol::CreateResult),
    Barrier(crate::tree_protocol::BarrierResult),
    AgentCreated(AgentCreated),
    Agents(Vec<AgentInfo>),
    AccountProviders(Vec<crate::managed_provider::ProviderDescriptor>),
    Account(crate::managed_provider::AccountSnapshot),
    DeviceLogin(DeviceLogin),
    BrowserLogin(BrowserLogin),
    LoginState(LoginState),
    ManagedAuthStatus(crate::codex_oauth::AuthStatus),
    ManagedLoginState(ManagedLoginState),
    Catalog(CatalogResult),
    ProviderCatalog(crate::provider::ProviderCatalog),
    Selection(SelectionResult),
    ProviderSelection(crate::provider::ProviderSelection),
    ProviderList(crate::provider::ProviderList),
    ProviderSummary(crate::provider::ProviderSummary),
    ProviderImport(crate::provider::ImportResult),
    Permission(PermissionResult),
    ExecEnded(dim_agent_types::ExecEndedData),
    SkillsCatalog(SkillsCatalogResult),
    SkillsRefresh(SkillsRefreshResult),
    SkillsVersion(SkillsVersion),
    SessionSkills(SessionSkillsResult),
    ModelUpdate(ModelUpdateResult),
    Configuration(ConfigurationResult),
    McpCatalog(McpCatalogResult),
    McpState(McpStateResult),
    WebOpened(WebOpened),
    WebStatus(WebStatus),
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
#[derive(Debug, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    #[serde(rename = "session.mcp.state")]
    McpState(McpStateResult),
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
    #[serde(rename = "session.model.progress")]
    ModelProgress(crate::tree_protocol::ModelProgressNotice),
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
