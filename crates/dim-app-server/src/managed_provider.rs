//! Process-owned access to the managed-provider service and its typed contract.
pub(crate) mod chatgpt;
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// The longest a Session assembly or reconciliation waits for a provider's
/// own account observation. The provider publishes the read through its watch;
/// the bound ends the caller's wait and the observation it started, so a
/// timed-out read leaves the last observation in place and no work behind.
const ACCOUNT_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);

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
            | ManagedError::PlanNotGranted
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
            | ManagedError::Cancelled
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
    PlanNotGranted,
    ResetDenied,
    Transient,
    ConfigError,
    InvalidResponse,
    InvalidCallback,
    AttemptNotFound,
    AttemptClosed,
    ServiceReloading,
    Cancelled,
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
    /// A replayable watch over whether this provider stores a signed-in
    /// account, moved at every change of that account. The binding publishes
    /// the watch's current value while its binding is current, so a login that
    /// completes after every client polling it is gone still reaches open
    /// Sessions. The first `Some` is the provider's initialized observation of
    /// its store; `None` means the provider has not read it yet. The watch is
    /// the only source of this provider's availability: account reads and
    /// login attempt results never publish over it.
    fn subscribe_account(&self) -> watch::Receiver<Option<bool>>;
    /// Re-reads the stored account and publishes the result through
    /// [`Self::subscribe_account`], so a store another process wrote becomes
    /// visible without an `account.status` call. The managed provider owner
    /// asks the provider for this observation once when managed search first
    /// becomes necessary for a Session. A failed read leaves the watch as it
    /// was.
    fn observe_account(&self, provider: &str) -> ManagedFuture<()> {
        let _ = provider;
        Box::pin(async { Ok(()) })
    }
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
    fn catalog(&self, provider: &str, cancel: CancellationToken) -> ManagedFuture<ManagedCatalog>;
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
            ManagedError::PlanNotGranted => (
                "chatgpt_plan_not_granted",
                "ChatGPT plan access was not granted. Allow plan usage in ChatGPT and sign in again.",
            ),
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
            ManagedError::Cancelled => ("account_cancelled", "Managed account operation cancelled"),
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

/// Process-owned access to the configured account binding and the product ChatGPT account.
pub struct ManagedProviders {
    provider: Mutex<Option<Bound>>,
    chatgpt: Option<(u64, Arc<chatgpt::ChatGpt>)>,
    stop: CancellationToken,
    /// Whether the bound provider's watch last reported a signed-in account.
    /// The owner publishes it from that watch and reads it in-process for
    /// capabilities that depend on a usable account.
    usable: watch::Sender<bool>,
}
/// A published provider and the identity of the binding that published it.
/// A replacement, even of an equal provider, is a new binding.
#[derive(Clone)]
struct Bound {
    provider: Arc<dyn ManagedProvider>,
    binding: u64,
    /// The provider's own account watch. It is authoritative for
    /// availability; an RPC result is only a historical observation for it.
    /// The binding reads the current value from this receiver while
    /// publishing, so a value subscribed earlier cannot overwrite a newer one.
    account: watch::Receiver<Option<bool>>,
}
static NEXT_BINDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn retired() -> ManagedProviderError {
    ManagedProviderError {
        code: "service_reloading",
        message: "Managed provider service is reloading",
    }
}
impl ManagedProviders {
    /// The account availability the bound provider's watch last reported.
    /// Without a binding it is `false`.
    pub fn usable(&self) -> bool {
        *self.usable.borrow()
    }
    /// Notified when [`Self::usable`] changes.
    pub fn subscribe_usable(&self) -> watch::Receiver<bool> {
        self.usable.subscribe()
    }
    /// Observes the bound provider's account once and returns whether it is
    /// signed in. The provider reads its own store and publishes through its
    /// watch; only that watch is published here. Without a binding, while
    /// `cancel` ends the wait, after the bounded wait, or when the observation
    /// fails, the last value stays: a failed read is not a confirmed signed-out
    /// account. The wait is bounded so a stalled read cannot hold assembly or
    /// Settings reconciliation past [`ACCOUNT_OBSERVATION_TIMEOUT`].
    pub async fn refresh_usable(&self, cancel: &CancellationToken) -> bool {
        let Ok(bound) = self.bound(cancel) else {
            return self.usable();
        };
        for descriptor in bound.provider.providers() {
            let observation = tokio::select! {
                biased;
                _ = cancel.cancelled() => return self.usable(),
                observation = tokio::time::timeout(
                    ACCOUNT_OBSERVATION_TIMEOUT,
                    bound.provider.observe_account(&descriptor.provider_id),
                ) => observation,
            };
            // A failed or timed-out observation leaves the watch as it was,
            // which is what this publishes below.
            let _ = observation;
        }
        if !cancel.is_cancelled() {
            self.observe_watch(bound.binding);
        }
        self.usable()
    }
    /// Publishes the binding's current account watch value while `binding` is
    /// still the bound one. The watch read and the publication share the lock
    /// with every other watch publication, so a value obtained earlier cannot
    /// overwrite a newer one, and a retired binding cannot overwrite its
    /// replacement.
    fn observe_watch(&self, binding: u64) {
        let slot = self.provider.lock().unwrap();
        let Some(bound) = slot.as_ref() else {
            return;
        };
        if bound.binding != binding {
            return;
        }
        if let Some(usable) = *bound.account.borrow() {
            self.usable.send_replace(usable);
        }
    }
    /// Called before Agent/RPC settlement, not only during plugin disposal.
    pub async fn quiesce(&self) {
        self.stop.cancel();
        if let Some((_, chatgpt)) = &self.chatgpt {
            chatgpt.close().await;
        }
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
        let mut providers = self.external_providers(cancel).await?;
        if self.chatgpt.is_some() {
            providers.push(chatgpt::descriptor());
        }
        Ok(providers)
    }
    pub(crate) async fn legacy_codex_hidden(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, ManagedProviderError> {
        match &self.chatgpt {
            Some((_, owner)) => Ok(owner.legacy_codex_hidden(cancel).await?),
            None => Ok(false),
        }
    }

    async fn external_providers(
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
        if !valid_descriptors(&providers)
            || providers
                .iter()
                .any(|provider| provider.provider_id == chatgpt::ID)
        {
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
        if id == chatgpt::ID {
            return checked_account(
                self.chatgpt
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .1
                    .status(cancel)
                    .await?,
            );
        }
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
    pub async fn catalog(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<ManagedCatalog, ManagedProviderError> {
        let catalog = if id == chatgpt::ID {
            self.chatgpt
                .as_ref()
                .ok_or_else(unavailable)?
                .1
                .catalog(cancel)
                .await?
        } else {
            self.provider(cancel)?.catalog(id, cancel.clone()).await?
        };
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
        let (binding, lease) = if id == chatgpt::ID {
            let (binding, chatgpt) = self.chatgpt.as_ref().ok_or_else(unavailable)?;
            (*binding, chatgpt.credential(None, cancel).await?)
        } else {
            let bound = self.bound(cancel)?;
            (bound.binding, bound.provider.credential(id, model).await?)
        };
        let mut lease = checked_lease(lease)?;
        lease.binding = binding;
        Ok(lease)
    }
    pub async fn recover(
        &self,
        lease: &CredentialLease,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease, ManagedProviderError> {
        let (binding, next) = if let Some((binding, chatgpt)) = &self.chatgpt
            && *binding == lease.binding
        {
            (
                *binding,
                chatgpt
                    .credential(Some((&lease.account, &lease.bearer)), cancel)
                    .await?,
            )
        } else {
            let bound = self.issuer(lease, cancel)?;
            (bound.binding, bound.provider.recover_rejected(lease).await?)
        };
        let mut next = checked_lease(next)?;
        next.binding = binding;
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
    chatgpt: Option<(
        Key<dim_agent_capabilities::file_storage::FileStorage>,
        usize,
    )>,
}
impl ManagedProvidersPlugin {
    /// `account` is the method table to place the `account` namespace on, as
    /// the product profile does; `None` serves the search face alone.
    pub fn new(account: Option<Key<crate::methods::ManagementMethods>>) -> Self {
        Self {
            account,
            chatgpt: None,
        }
    }
    pub(crate) fn with_chatgpt(
        mut self,
        files: Key<dim_agent_capabilities::file_storage::FileStorage>,
        root: usize,
    ) -> Self {
        self.chatgpt = Some((files, root));
        self
    }
}
impl DynPlugin for ManagedProvidersPlugin {
    fn name(&self) -> &str {
        "ManagedProviders"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.account
            .iter()
            .map(Key::erase)
            .chain(self.chatgpt.iter().map(|(files, _)| files.erase()))
            .collect()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let chatgpt = match &self.chatgpt {
                Some((files, root)) => {
                    let owner =
                        chatgpt::ChatGpt::new(ctx.require_at(files)?, *root).map_err(|error| {
                            rutis::Error::from(ErrorKind::PluginFailed(
                                std::io::Error::other(ManagedProviderError::from(error).message)
                                    .into(),
                            ))
                        })?;
                    Some((
                        NEXT_BINDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        owner,
                    ))
                }
                None => None,
            };
            let service = Arc::new(ManagedProviders {
                provider: Mutex::new(None),
                chatgpt,
                stop: CancellationToken::new(),
                usable: watch::Sender::new(false),
            });
            let owner = service.clone();
            ctx.effect(move || async move {
                owner.stop.cancel();
                if let Some((_, chatgpt)) = &owner.chatgpt {
                    chatgpt.close().await;
                }
                Ok(())
            })?;
            if let Some(methods) = &self.account {
                let methods = ctx.require_at(methods)?;
                management::provide(ctx, &methods, service.clone())?;
                if let Some((_, chatgpt)) = &service.chatgpt {
                    chatgpt::provide(ctx, &methods, chatgpt.clone())?;
                }
            }
            let search = Arc::new(ManagedSearch(service.clone()));
            ctx.provide_at(owner_key(), service)?;
            ctx.provide_as::<ManagedSearch>(search.clone())?;
            ctx.provide_at(rutis_bridge::session::host_key("managedSearch"), search)?;
            Ok(Effect::Done)
        })
    }
}

/// The `managedSearch` data face ([service faces](../../../docs/design/service-faces.md)
/// §4.6): only the account's search operation crosses; its login and tokens
/// stay on the owner implementation behind it.
#[derive(Clone)]
pub struct ManagedSearch(Arc<ManagedProviders>);

impl ManagedSearch {
    /// The account availability the bound provider last reported, read
    /// in-process by capabilities that depend on a usable signed-in account.
    pub fn usable(&self) -> bool {
        self.0.usable()
    }
    /// Notified when [`Self::usable`] changes.
    pub fn subscribe_usable(&self) -> watch::Receiver<bool> {
        self.0.subscribe_usable()
    }
    /// Re-reads the bound provider's account and publishes the result.
    pub async fn refresh_usable(&self, cancel: &CancellationToken) -> bool {
        self.0.refresh_usable(cancel).await
    }
    /// Searches with the current managed provider. Dropping or cancelling the
    /// call cancels its provider request without cancelling the caller's token.
    pub async fn search(
        &self,
        request: WebSearchRequest,
        cancel: &CancellationToken,
    ) -> Result<WebSearchResponse, ManagedSearchError> {
        let child = cancel.child_token();
        let _dropped = child.clone().drop_guard();
        let provider = self
            .0
            .provider(&child)
            .map_err(|_| ManagedSearchError::Unavailable)?;
        Ok(provider.web_search(request, child).await?)
    }
}

impl HostDispatch for ManagedSearch {
    fn methods(&self) -> Option<Value> {
        Some(json!({ "search": "async" }))
    }

    fn invoke(&self, method: &str, args: ServiceValue) -> Reply {
        if method != "search" {
            return Err(BridgeError::value(format!(
                "unknown managedSearch method {method}"
            )));
        }
        let [request]: [WebSearchRequest; 1] =
            serde_json::from_value(args.json()?).map_err(|error| {
                BridgeError::value(format!(
                    "expected one managedSearch request object: {error}"
                ))
            })?;
        let search = self.clone();
        Ok(ServiceValue::cancellable_future(async move {
            match search.search(request, &CancellationToken::new()).await {
                Ok(reply) => rutis_bridge::session::arg(&reply),
                Err(error) => Err(failure(error)),
            }
        }))
    }
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
            let descriptors = provider.providers();
            if !valid_descriptors(&descriptors)
                || descriptors
                    .iter()
                    .any(|provider| provider.provider_id == chatgpt::ID)
            {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    "Invalid managed provider description".into(),
                )));
            }
            let binding = NEXT_BINDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut account = provider.subscribe_account();
            let initial = *account.borrow();
            {
                let mut slot = owner.provider.lock().unwrap();
                *slot = Some(Bound {
                    provider: provider.clone(),
                    binding,
                    account: account.clone(),
                });
                // A replacement starts from its own observation: a new
                // generation that has not read its store must not inherit the
                // retired generation's signed-in state.
                owner.usable.send_replace(initial.unwrap_or(false));
            }
            let stop = ctx.cancellation_token();
            let mirror = {
                let owner = owner.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            moved = account.changed() => if moved.is_err() {
                                return;
                            },
                        }
                        owner.observe_watch(binding);
                    }
                })
            };
            ctx.effect(move || async move {
                mirror.abort();
                let _ = mirror.await;
                {
                    let mut slot = owner.provider.lock().unwrap();
                    if slot.as_ref().is_some_and(|bound| bound.binding == binding) {
                        *slot = None;
                        owner.usable.send_replace(false);
                    }
                }
                provider.quiesce().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}
