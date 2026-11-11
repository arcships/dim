//! Process-owned access to the managed-provider service and its typed contract.
pub mod management;
use crate::model_catalog::{MAX_MODEL_TOKENS, ModelEntry, ModelStatus};
use dim_agent_capabilities::host::ModelProtocol;
use management::{
    ApplyReset, BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
    ManagedBrowserLoginComplete, ManagedLoginAttempt, Referral, ResetApplied, ResetBatches, Usage,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis_bridge::session::{Error as BridgeError, HostDispatch, Reply, Value as ServiceValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDescriptor {
    pub provider_id: String,
    pub display_name: String,
    pub login_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_directory: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSnapshot {
    pub credential_epoch: String,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountView>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountView {
    pub issuer: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub granted_scopes: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedModel {
    pub entry: ModelEntry,
    pub protocol: ModelProtocol,
    pub unavailable_reason: Option<String>,
    pub zdr: bool,
}
#[derive(Clone)]
pub struct ManagedCatalog {
    pub account: AccountSnapshot,
    pub models: Vec<ManagedModel>,
}

impl ManagedModel {
    /// Whether this entry is excluded from every use. An `unavailable` status
    /// says so whatever reason was stored; otherwise only a stored reason
    /// that excludes every use does. The managed catalog derivation and a
    /// managed account's credential lease read this one rule.
    pub fn excluded_from_every_use(&self) -> bool {
        self.entry.status == ModelStatus::Unavailable
            || self
                .unavailable_reason
                .as_deref()
                .is_some_and(reason_excludes_modality)
    }
}

/// Whether a stored `unavailableReason` excludes a non-conversational use too.
/// `unsupported_model_type` says the entry is not a chat model and
/// `limits_undeclared` that it declares no chat limits; both leave a
/// non-conversational use open. Every other stored reason — the model's
/// status, a missing route, a disabled provider, a missing credential, a
/// signed-out account — means the entry serves no use at all. This is the
/// reason half of [`ManagedModel::excluded_from_every_use`], which also reads
/// the entry's status and is what the catalog and a credential lease consult
/// when they hold the whole model.
pub fn reason_excludes_modality(reason: &str) -> bool {
    !matches!(reason, "unsupported_model_type" | "limits_undeclared")
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchRequest {
    pub query: String,
    pub count: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchResponse {
    pub results: Vec<WebSearchHit>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSearchError {
    #[error("Managed search is unavailable")]
    Unavailable,
    #[error("Managed search failed")]
    SearchFailed,
}
impl From<ManagedError> for ManagedSearchError {
    fn from(error: ManagedError) -> Self {
        match error {
            ManagedError::TokenMissing
            | ManagedError::RefreshRejected
            | ManagedError::InvalidStore
            | ManagedError::UnsupportedSchema
            | ManagedError::AccountRecoveryRequired
            | ManagedError::AccountResetBlocked
            | ManagedError::IdentityRejected
            | ManagedError::ConfigError => Self::Unavailable,
            ManagedError::AccountChanged
            | ManagedError::StorageError
            | ManagedError::Transient
            | ManagedError::InvalidResponse
            | ManagedError::InvalidCallback
            | ManagedError::AttemptNotFound
            | ManagedError::AttemptClosed
            | ManagedError::ServiceReloading
            | ManagedError::InvalidInput
            | ManagedError::ResetDenied
            | ManagedError::UnknownModel
            | ManagedError::LeaseNotFound
            | ManagedError::LeaseCapacity => Self::SearchFailed,
        }
    }
}

/// Canonical managed-provider failures. Only fixed public diagnostics derived
/// from them cross the client boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedError {
    InvalidStore,
    StorageError,
    UnsupportedSchema,
    AccountRecoveryRequired,
    AccountResetBlocked,
    AccountChanged,
    IdentityRejected,
    TokenMissing,
    RefreshRejected,
    ResetDenied,
    Transient,
    ConfigError,
    InvalidResponse,
    InvalidCallback,
    AttemptNotFound,
    AttemptClosed,
    ServiceReloading,
    InvalidInput,
    UnknownModel,
    LeaseNotFound,
    LeaseCapacity,
}

/// Releases a provider's lease when the last holder drops it.
pub trait LeaseOwner: Send + Sync {
    fn release(&self, lease_id: &str);
}
struct LeaseHandle {
    id: String,
    owner: Weak<dyn LeaseOwner>,
}
impl Drop for LeaseHandle {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.release(&self.id);
        }
    }
}
/// A one-request credential. Secrets are internal: never Debug or serialized.
pub struct CredentialLease {
    pub account: AccountSnapshot,
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub headers: HashMap<String, String>,
    pub bearer: String,
    handle: LeaseHandle,
    /// The binding that issued the lease, stamped by `ManagedProviders`:
    /// recovery and final rejection go to that binding only.
    binding: u64,
}
impl CredentialLease {
    pub fn new(
        account: AccountSnapshot,
        protocol: ModelProtocol,
        base_url: String,
        headers: HashMap<String, String>,
        bearer: String,
        id: String,
        owner: Weak<dyn LeaseOwner>,
    ) -> Self {
        Self {
            account,
            protocol,
            base_url,
            headers,
            bearer,
            handle: LeaseHandle { id, owner },
            binding: 0,
        }
    }
    pub fn id(&self) -> &str {
        &self.handle.id
    }
}

pub type ManagedFuture<T> = BoxFuture<'static, Result<T, ManagedError>>;

/// Registers the process row of a managed provider plugin built outside this
/// repository, given the product root's FileStorage index, and returns the
/// row name. The row provides `Arc<dyn ManagedProvider>`.
#[derive(Clone)]
pub struct ManagedProviderRow(pub Arc<RegisterRow>);
pub type RegisterRow = dyn Fn(&mut dim_agent_runtime::Modules, usize) -> String + Send + Sync;
impl std::fmt::Debug for ManagedProviderRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ManagedProviderRow")
    }
}

/// The managed-provider service. Implementations own every accepted
/// operation: a caller dropping a returned future never abandons a token
/// rotation or a persisted login.
pub trait ManagedProvider: Send + Sync + 'static {
    fn providers(&self) -> Vec<ProviderDescriptor>;
    fn status(&self, provider: &str) -> ManagedFuture<AccountSnapshot>;
    fn recover(&self, provider: &str) -> ManagedFuture<AccountSnapshot>;
    fn begin_browser(&self, login: BrowserLoginBegin) -> ManagedFuture<BrowserLogin>;
    fn complete_browser(
        &self,
        login: ManagedBrowserLoginComplete,
    ) -> ManagedFuture<AccountSnapshot>;
    fn begin_device(&self, login: LoginBegin) -> ManagedFuture<DeviceLogin>;
    fn attempt_status(&self, attempt: &ManagedLoginAttempt) -> Result<LoginState, ManagedError>;
    fn cancel_attempt(&self, attempt: ManagedLoginAttempt) -> ManagedFuture<()>;
    fn logout(&self, provider: &str) -> ManagedFuture<AccountSnapshot>;
    fn usage(&self, provider: &str) -> ManagedFuture<Usage>;
    fn referral(&self, _provider: &str) -> ManagedFuture<Referral> {
        Box::pin(async { Err(ManagedError::InvalidInput) })
    }
    fn reset_batches(&self, _provider: &str) -> ManagedFuture<ResetBatches> {
        Box::pin(async { Err(ManagedError::InvalidInput) })
    }
    /// Applies one credit with a caller-stable idempotency key. Retrying the
    /// same action must retain that key, including after a lost reply.
    fn apply_reset(&self, _request: ApplyReset) -> ManagedFuture<ResetApplied> {
        Box::pin(async { Err(ManagedError::InvalidInput) })
    }
    /// `cancel` releases the relay request; a token refresh it started still
    /// completes.
    fn web_search(
        &self,
        request: WebSearchRequest,
        cancel: CancellationToken,
    ) -> ManagedFuture<WebSearchResponse>;
    fn catalog(&self, provider: &str) -> ManagedFuture<ManagedCatalog>;
    fn credential(&self, provider: &str, model: &str) -> ManagedFuture<CredentialLease>;
    /// One controlled recovery after the relay rejected `lease`.
    fn recover_rejected(&self, lease: &CredentialLease) -> ManagedFuture<CredentialLease>;
    /// Settles a rejection of the recovered credential.
    fn finish_rejected(&self, lease: &CredentialLease) -> ManagedFuture<()>;
    /// Stops admission and cancels lock waits and login attempts, without
    /// abandoning a refresh that already holds the account lock.
    fn quiesce(&self) -> BoxFuture<'static, ()>;
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ManagedProviderError {
    pub code: &'static str,
    pub message: &'static str,
}
fn unavailable() -> ManagedProviderError {
    ManagedProviderError {
        code: "account_provider_unavailable",
        message: "Managed provider plugin is unavailable",
    }
}
impl From<ManagedError> for ManagedProviderError {
    fn from(error: ManagedError) -> Self {
        let (code, message) = match error {
            ManagedError::AccountChanged => ("account_changed", "Managed account changed"),
            ManagedError::TokenMissing | ManagedError::RefreshRejected => {
                ("account_required", "Provider login is required")
            }
            ManagedError::InvalidStore => {
                ("account_invalid_store", "Managed account store is invalid")
            }
            ManagedError::UnsupportedSchema => (
                "account_unsupported_schema",
                "Managed account schema is unsupported",
            ),
            ManagedError::AccountRecoveryRequired => (
                "account_recovery_required",
                "Managed account requires explicit recovery",
            ),
            ManagedError::AccountResetBlocked => {
                ("account_reset_blocked", "Managed account reset is blocked")
            }
            ManagedError::IdentityRejected => (
                "account_identity_rejected",
                "Managed account identity was rejected",
            ),
            ManagedError::InvalidInput
            | ManagedError::InvalidCallback
            | ManagedError::ConfigError => {
                ("account_invalid_input", "Managed account input is invalid")
            }
            ManagedError::AttemptNotFound => ("login_attempt_unknown", "Login attempt is unknown"),
            ManagedError::AttemptClosed => ("login_attempt_closed", "Login attempt is closed"),
            ManagedError::ResetDenied => (
                "account_reset_denied",
                "No applicable usage reset credit is available",
            ),
            ManagedError::InvalidResponse => (
                "account_invalid_response",
                "Managed provider returned an invalid response",
            ),
            ManagedError::ServiceReloading => {
                ("service_reloading", "Managed provider service is reloading")
            }
            ManagedError::StorageError
            | ManagedError::Transient
            | ManagedError::UnknownModel
            | ManagedError::LeaseNotFound
            | ManagedError::LeaseCapacity => {
                ("account_unavailable", "Managed account operation failed")
            }
        };
        Self { code, message }
    }
}

/// Process-owned access to the bound provider. Without a binding the process
/// has no managed providers.
pub struct ManagedProviders {
    provider: Mutex<Option<Bound>>,
    stop: CancellationToken,
}
/// A published provider and the identity of the binding that published it.
/// A replacement, even of an equal provider, is a new binding.
#[derive(Clone)]
struct Bound {
    provider: Arc<dyn ManagedProvider>,
    binding: u64,
}
static NEXT_BINDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn retired() -> ManagedProviderError {
    ManagedProviderError {
        code: "service_reloading",
        message: "Managed provider service is reloading",
    }
}
impl ManagedProviders {
    /// Called before Agent/RPC settlement, not only during plugin disposal.
    pub async fn quiesce(&self) {
        self.stop.cancel();
        let bound = self.provider.lock().unwrap().clone();
        if let Some(bound) = bound {
            bound.provider.quiesce().await;
        }
    }
    fn bound(&self, cancel: &CancellationToken) -> Result<Bound, ManagedProviderError> {
        if self.stop.is_cancelled() || cancel.is_cancelled() {
            return Err(unavailable());
        }
        self.provider
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(unavailable)
    }
    fn provider(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Arc<dyn ManagedProvider>, ManagedProviderError> {
        Ok(self.bound(cancel)?.provider)
    }
    /// The binding that issued `lease`, if it is still the published one.
    fn issuer(
        &self,
        lease: &CredentialLease,
        cancel: &CancellationToken,
    ) -> Result<Bound, ManagedProviderError> {
        let bound = self.bound(cancel)?;
        if bound.binding != lease.binding {
            return Err(retired());
        }
        Ok(bound)
    }
    pub async fn providers(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<ProviderDescriptor>, ManagedProviderError> {
        let Some(Bound { provider, .. }) = self.provider.lock().unwrap().clone() else {
            return Ok(vec![]);
        };
        if cancel.is_cancelled() {
            return Err(unavailable());
        }
        let providers = provider.providers();
        if !valid_descriptors(&providers) {
            return Err(invalid());
        }
        Ok(providers)
    }
    pub async fn contains(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, ManagedProviderError> {
        Ok(self
            .providers(cancel)
            .await?
            .iter()
            .any(|p| p.provider_id == id))
    }
    pub async fn status(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot, ManagedProviderError> {
        checked_account(self.provider(cancel)?.status(id).await?)
    }
    pub async fn recover_account(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot, ManagedProviderError> {
        checked_account(self.provider(cancel)?.recover(id).await?)
    }
    pub async fn begin_browser(
        &self,
        login: BrowserLoginBegin,
        cancel: &CancellationToken,
    ) -> Result<BrowserLogin, ManagedProviderError> {
        Ok(self.provider(cancel)?.begin_browser(login).await?)
    }
    pub async fn complete_browser(
        &self,
        login: ManagedBrowserLoginComplete,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot, ManagedProviderError> {
        checked_account(self.provider(cancel)?.complete_browser(login).await?)
    }
    pub async fn begin_device(
        &self,
        login: LoginBegin,
        cancel: &CancellationToken,
    ) -> Result<DeviceLogin, ManagedProviderError> {
        Ok(self.provider(cancel)?.begin_device(login).await?)
    }
    pub fn attempt_status(
        &self,
        attempt: &ManagedLoginAttempt,
        cancel: &CancellationToken,
    ) -> Result<LoginState, ManagedProviderError> {
        let state = self.provider(cancel)?.attempt_status(attempt)?;
        if let LoginState::Completed { account } = &state {
            checked_account(account.clone())?;
        }
        Ok(state)
    }
    pub async fn cancel_attempt(
        &self,
        attempt: ManagedLoginAttempt,
        cancel: &CancellationToken,
    ) -> Result<(), ManagedProviderError> {
        Ok(self.provider(cancel)?.cancel_attempt(attempt).await?)
    }
    pub async fn logout(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot, ManagedProviderError> {
        checked_account(self.provider(cancel)?.logout(id).await?)
    }
    pub async fn usage(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Usage, ManagedProviderError> {
        Ok(self.provider(cancel)?.usage(id).await?)
    }
    pub async fn referral(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Referral, ManagedProviderError> {
        Ok(self.provider(cancel)?.referral(id).await?)
    }
    pub async fn reset_batches(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<ResetBatches, ManagedProviderError> {
        Ok(self.provider(cancel)?.reset_batches(id).await?)
    }
    pub async fn apply_reset(
        &self,
        request: ApplyReset,
        cancel: &CancellationToken,
    ) -> Result<ResetApplied, ManagedProviderError> {
        Ok(self.provider(cancel)?.apply_reset(request).await?)
    }
    pub async fn search(
        &self,
        request: WebSearchRequest,
        cancel: &CancellationToken,
    ) -> Result<WebSearchResponse, ManagedSearchError> {
        let provider = self
            .provider(cancel)
            .map_err(|_| ManagedSearchError::Unavailable)?;
        Ok(provider.web_search(request, cancel.clone()).await?)
    }
    pub async fn catalog(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<ManagedCatalog, ManagedProviderError> {
        let catalog = self.provider(cancel)?.catalog(id).await?;
        if !valid_catalog(&catalog) {
            return Err(invalid());
        }
        Ok(catalog)
    }
    pub async fn credential(
        &self,
        id: &str,
        model: &str,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease, ManagedProviderError> {
        let bound = self.bound(cancel)?;
        let mut lease = checked_lease(bound.provider.credential(id, model).await?)?;
        lease.binding = bound.binding;
        Ok(lease)
    }
    pub async fn recover(
        &self,
        lease: &CredentialLease,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease, ManagedProviderError> {
        let bound = self.issuer(lease, cancel)?;
        let mut next = checked_lease(bound.provider.recover_rejected(lease).await?)?;
        next.binding = bound.binding;
        if next.account != lease.account {
            return Err(ManagedProviderError {
                code: "account_changed",
                message: "Managed account changed during recovery",
            });
        }
        Ok(next)
    }
    pub async fn finish_rejected(
        &self,
        lease: &CredentialLease,
        cancel: &CancellationToken,
    ) -> Result<(), ManagedProviderError> {
        Ok(self
            .issuer(lease, cancel)?
            .provider
            .finish_rejected(lease)
            .await?)
    }
}

fn invalid() -> ManagedProviderError {
    ManagedError::InvalidResponse.into()
}
fn supported(protocol: ModelProtocol) -> bool {
    matches!(
        protocol,
        ModelProtocol::OpenaiCompatible | ModelProtocol::OpenaiResponses | ModelProtocol::Anthropic
    )
}
fn valid_account(account: &AccountSnapshot) -> bool {
    !account.credential_epoch.is_empty()
        && account.generation <= MAX_MODEL_TOKENS
        && account
            .account
            .as_ref()
            .is_none_or(|a| !a.issuer.is_empty() && !a.subject.is_empty())
}
fn checked_account(account: AccountSnapshot) -> Result<AccountSnapshot, ManagedProviderError> {
    if !valid_account(&account) {
        return Err(invalid());
    }
    Ok(account)
}
fn valid_descriptors(providers: &[ProviderDescriptor]) -> bool {
    let mut ids = std::collections::HashSet::new();
    providers.iter().all(|p| {
        !p.provider_id.is_empty()
            && ids.insert(&p.provider_id)
            && !p.display_name.trim().is_empty()
            && p.login_methods
                .iter()
                .all(|m| matches!(m.as_str(), "browser" | "device"))
            && p.default_directory.as_ref().is_none_or(|directory| {
                url::Url::parse(directory).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.has_host()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                })
            })
    })
}
fn valid_catalog(catalog: &ManagedCatalog) -> bool {
    let mut ids = std::collections::HashSet::new();
    valid_account(&catalog.account)
        && catalog.account.account.is_some()
        && catalog.models.iter().all(|m| {
            !m.entry.model_id.is_empty()
                && ids.insert(&m.entry.model_id)
                && !m.entry.display_name.trim().is_empty()
                && supported(m.protocol)
                && crate::model_catalog::limits_valid(&m.entry.capabilities)
                && (m.entry.capabilities.missing_limits().is_empty()
                    || m.unavailable_reason.is_some())
        })
}
/// A lease that fails the checks is dropped, which releases it.
fn checked_lease(lease: CredentialLease) -> Result<CredentialLease, ManagedProviderError> {
    let url_ok = url::Url::parse(&lease.base_url).is_ok_and(|url| {
        matches!(url.scheme(), "https" | "http")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    let headers_ok = lease
        .headers
        .iter()
        .all(|(k, v)| !k.is_empty() && !k.contains(['\r', '\n']) && !v.contains(['\r', '\n']));
    if lease.id().is_empty()
        || lease.bearer.is_empty()
        || lease.account.account.is_none()
        || !valid_account(&lease.account)
        || !supported(lease.protocol)
        || !url_ok
        || !headers_ok
    {
        return Err(invalid());
    }
    Ok(lease)
}

/// The managed provider's implementation handle for the App Server's own
/// parts. `managedProviders` is no service: the face is `managedSearch`, and
/// the account surface is the `account` method namespace, so the owner is
/// provided under a key with no catalog name.
pub fn owner_key() -> Key<ManagedProviders> {
    Key::named("managedProviderOwner")
}

/// Provides the managed provider implementation behind `managedSearch`, and
/// the `account` namespace where the profile hosts it.
pub struct ManagedProvidersPlugin {
    account: Option<Key<crate::methods::ManagementMethods>>,
}
impl ManagedProvidersPlugin {
    /// `account` is the method table to place the `account` namespace on, as
    /// the product profile does; `None` serves the search face alone.
    pub fn new(account: Option<Key<crate::methods::ManagementMethods>>) -> Self {
        Self { account }
    }
}
impl DynPlugin for ManagedProvidersPlugin {
    fn name(&self) -> &str {
        "ManagedProviders"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.account.iter().map(Key::erase).collect()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let service = Arc::new(ManagedProviders {
                provider: Mutex::new(None),
                stop: CancellationToken::new(),
            });
            let owner = service.clone();
            ctx.effect(move || async move {
                owner.stop.cancel();
                Ok(())
            })?;
            if let Some(methods) = &self.account {
                let methods = ctx.require_at(methods)?;
                management::provide(ctx, &methods, service.clone())?;
            }
            let face = Arc::new(ManagedSearchFace(service.clone())) as Arc<dyn HostDispatch>;
            ctx.provide_at(owner_key(), service)?;
            ctx.provide_at(rutis_bridge::session::host_key("managedSearch"), face)?;
            Ok(Effect::Done)
        })
    }
}

/// The `managedSearch` data face ([service faces](../../../docs/design/service-faces.md)
/// §4.6): only the account's search operation crosses; its login and tokens
/// stay on the owner implementation behind it.
struct ManagedSearchFace(Arc<ManagedProviders>);

impl HostDispatch for ManagedSearchFace {
    fn methods(&self) -> Option<Value> {
        Some(json!({ "search": "async" }))
    }

    fn invoke(&self, method: &str, args: ServiceValue) -> Reply {
        if method != "search" {
            return Err(BridgeError::Value(format!(
                "unknown managedSearch method {method}"
            )));
        }
        let request: WebSearchRequest = request(args)?;
        let owner = self.0.clone();
        Ok(ServiceValue::cancellable_future(async move {
            let cancel = CancellationToken::new();
            let _dropped = cancel.clone().drop_guard();
            match owner.search(request, &cancel).await {
                Ok(reply) => rutis_bridge::session::arg(&reply),
                Err(error) => Err(failure(error)),
            }
        }))
    }
}

/// One request object; anything else is the caller's programming error.
fn request<T: DeserializeOwned>(args: ServiceValue) -> Result<T, BridgeError> {
    let invalid = |detail: String| {
        BridgeError::Value(format!(
            "expected one managedSearch request object: {detail}"
        ))
    };
    let Value::Array(args) = args.json()? else {
        return Err(invalid("not an argument list".into()));
    };
    let [request] = <[Value; 1]>::try_from(args)
        .map_err(|args| invalid(format!("{} arguments", args.len())))?;
    serde_json::from_value(request).map_err(|error| invalid(error.to_string()))
}

fn failure(error: ManagedSearchError) -> BridgeError {
    let code = match error {
        ManagedSearchError::Unavailable => "unavailable",
        ManagedSearchError::SearchFailed => "search_failed",
    };
    rutis_bridge::exports::failure("ManagedSearchError", code, error.to_string(), None)
}

/// Publishes a provider to the managed provider owner while both are active.
pub struct ManagedProviderBinding;
impl DynPlugin for ManagedProviderBinding {
    fn name(&self) -> &str {
        "ManagedProviderBinding"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![owner_key().erase(), TypeKey::of::<dyn ManagedProvider>()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&owner_key())?;
            let provider = ctx.require::<dyn ManagedProvider>()?;
            if !valid_descriptors(&provider.providers()) {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    "Invalid managed provider description".into(),
                )));
            }
            let binding = NEXT_BINDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *owner.provider.lock().unwrap() = Some(Bound {
                provider: provider.clone(),
                binding,
            });
            ctx.effect(move || async move {
                {
                    let mut slot = owner.provider.lock().unwrap();
                    if slot.as_ref().is_some_and(|bound| bound.binding == binding) {
                        *slot = None;
                    }
                }
                provider.quiesce().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}
