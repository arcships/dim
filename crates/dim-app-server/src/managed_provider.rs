//! Process-owned access to the managed-provider service and its typed contract.
pub mod management;
use crate::model_catalog::{MAX_MODEL_TOKENS, ModelEntry};
use dim_agent_capabilities::host::ModelProtocol;
use dim_agent_runtime::Names;
use management::{
    BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
    ManagedBrowserLoginComplete, ManagedLoginAttempt, Usage,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde::{Deserialize, Serialize};
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
                && [
                    m.entry.capabilities.context_window,
                    m.entry.capabilities.max_output_tokens,
                ]
                .iter()
                .all(|n| (1..=MAX_MODEL_TOKENS).contains(n))
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

pub struct ManagedProvidersPlugin;
impl DynPlugin for ManagedProvidersPlugin {
    fn name(&self) -> &str {
        "ManagedProviders"
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
            ctx.provide_as::<ManagedProviders>(service)?;
            Ok(Effect::Done)
        })
    }
}
/// Process owner of the `account` management namespace.
pub struct ManagedProviderMethodsPlugin {
    owner: Key<ManagedProviders>,
    methods: Key<crate::methods::ManagementMethods>,
}
impl ManagedProviderMethodsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            owner: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for ManagedProviderMethodsPlugin {
    fn name(&self) -> &str {
        "ManagedProviderMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.owner.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&self.owner)?;
            let methods = ctx.require_at(&self.methods)?;
            management::provide(ctx, &methods, owner)?;
            Ok(Effect::Done)
        })
    }
}
/// Publishes a provider to ManagedProviders while both are active.
pub struct ManagedProviderBinding {
    owner: Key<ManagedProviders>,
    provider: Key<dyn ManagedProvider>,
}
impl ManagedProviderBinding {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            owner: names.key()?,
            provider: names.key()?,
        })
    }
}
impl DynPlugin for ManagedProviderBinding {
    fn name(&self) -> &str {
        "ManagedProviderBinding"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.owner.erase(), self.provider.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&self.owner)?;
            let provider = ctx.require_at(&self.provider)?;
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
