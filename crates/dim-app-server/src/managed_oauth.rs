use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::file_storage::{FileLease, FilePath, FileStorage, FileStorageError};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    io,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;
use uuid::Uuid;

const AUTH_FILE: &str = "auth.json";
const FRESH_MS: u64 = 30_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_ATTEMPTS: usize = 32;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error(transparent)]
    Storage(#[from] FileStorageError),
    #[error("invalid OAuth store: {0}")]
    InvalidStore(&'static str),
    #[error("OAuth account schema is unsupported")]
    UnsupportedSchema,
    #[error("OAuth credentials require explicit recovery or logout before a new login")]
    RecoveryRequired,
    #[error("invalid OAuth configuration: {0}")]
    Config(&'static str),
    #[error("too many pending OAuth attempts")]
    TooManyAttempts,
    #[error("OAuth attempt not found")]
    AttemptNotFound,
    #[error("OAuth attempt is no longer pending")]
    AttemptClosed,
    #[error("invalid OAuth callback: {0}")]
    InvalidCallback(&'static str),
    #[error("OAuth service temporarily unavailable ({0:?})")]
    Transient(Option<u16>),
    #[error("OAuth credential was rejected")]
    Rejected,
    #[error("OAuth credential is missing")]
    Missing,
    #[error("OAuth account changed while signing in")]
    AccountChanged,
    #[error("OAuth service is closed")]
    Closed,
    #[error("invalid OAuth response: {0}")]
    InvalidResponse(&'static str),
    #[error("OAuth worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}
pub type Result<T> = std::result::Result<T, OAuthError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Pending,
    Completed,
    Failed(String),
    Cancelled,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub authenticated: bool,
    pub recovery_required: bool,
    pub expires_at: Option<u64>,
    pub account_id_preview: Option<String>,
}

#[derive(Clone)]
pub struct BrowserBegin {
    pub attempt_id: Uuid,
    pub authorization_url: Url,
    pub expires_at: u64,
}

#[derive(Clone)]
pub struct DeviceBegin {
    pub attempt_id: Uuid,
    pub verification_uri: Url,
    pub verification_uri_complete: Option<Url>,
    pub user_code: String,
    pub expires_at: u64,
    pub interval_seconds: u64,
}

/// A one-request secret lease. It intentionally has no `Debug` or serialization.
#[derive(Clone)]
pub struct CredentialLease {
    access: String,
    pub account_id: Option<String>,
    pub relay_base_url: String,
    authority: &'static str,
    generation: Uuid,
    revision: u64,
}
impl CredentialLease {
    pub fn bearer(&self) -> &str {
        &self.access
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    #[serde(rename = "type")]
    kind: String,
    refresh: String,
    access: String,
    expires: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    issuer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorization_endpoint: Option<String>,
    token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_authorization_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_token_endpoint: Option<String>,
    client_id: String,
    scope: String,
    relay_base_url: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub issuer: Url,
    pub client_id: String,
    pub scope: String,
    pub authorization: Option<Url>,
    pub token: Url,
    pub device: Option<Url>,
    pub redirect: Option<Url>,
    pub relay: Url,
}
impl Config {
    fn validate(&self, production: &Config) -> Result<()> {
        if self == production {
            return Ok(());
        }
        let fixture = self.issuer.scheme() == "http"
            && self.issuer.host_str() == Some("127.0.0.1")
            && [&self.issuer, &self.token, &self.relay]
                .into_iter()
                .chain(self.authorization.iter())
                .chain(self.device.iter())
                .all(|url| safe_url(url) && url.origin() == self.issuer.origin());
        if !fixture
            || self.redirect != production.redirect
            || self.authorization.is_some() != production.authorization.is_some()
            || self.device.is_some() != production.device.is_some()
            || self.client_id.trim().is_empty()
            || self.scope.trim().is_empty()
        {
            return Err(OAuthError::Config("provider endpoint set"));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountState {
    schema_version: u8,
    generation: Uuid,
    credential_revision: u64,
    token_digest: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, PartialEq, Eq)]
enum Revision {
    Managed(Uuid),
    Legacy(Option<String>),
}

/// Decoded once at the file boundary; the raw document preserves other owners.
struct Record {
    document: Map<String, Value>,
    tokens: Option<Tokens>,
    state: Option<AccountState>,
    digest: Option<String>,
}
impl Record {
    fn revision(&self) -> Result<Revision> {
        match &self.state {
            Some(state) if state.token_digest == self.digest => {
                Ok(Revision::Managed(state.generation))
            }
            Some(_) => Err(OAuthError::RecoveryRequired),
            None => Ok(Revision::Legacy(self.digest.clone())),
        }
    }
}

struct Attempt {
    revision: Revision,
    status: AttemptStatus,
    cancel: CancellationToken,
    expires_at: u64,
    exchanging: bool,
    kind: AttemptKind,
}
enum AttemptKind {
    Browser { state: String, verifier: String },
    Device,
}

pub struct Service {
    files: Arc<FileStorage>,
    path: FilePath,
    key: &'static str,
    state_key: &'static str,
    config: Config,
    client: OnceCell<Client>,
    #[cfg(feature = "testing")]
    fail_client_build: std::sync::atomic::AtomicBool,
    stopping: CancellationToken,
    attempts: Mutex<HashMap<Uuid, Attempt>>,
    tasks: TaskTracker,
    admission: std::sync::Mutex<()>,
}
impl Service {
    /// Caller futures may disappear; the Plugin still owns admitted I/O.
    async fn owned<T, F, Fut>(self: &Arc<Self>, cancel: &CancellationToken, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Service>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let task = {
            let _guard = self.admission.lock().unwrap();
            if self.stopping.is_cancelled() || cancel.is_cancelled() {
                return Err(OAuthError::Closed);
            }
            self.tasks.spawn(f(self.clone(), cancel.clone()))
        };
        task.await?
    }
    pub(crate) async fn close(&self) {
        {
            let _guard = self.admission.lock().unwrap();
            self.stopping.cancel();
            self.tasks.close();
        }
        for attempt in self.attempts.lock().await.values_mut() {
            attempt.cancel.cancel();
            if attempt.status == AttemptStatus::Pending {
                attempt.status = AttemptStatus::Cancelled;
            }
        }
        self.tasks.wait().await;
    }
    pub(crate) async fn status(self: &Arc<Self>, c: &CancellationToken) -> Result<AuthStatus> {
        self.owned(c, |s, c| async move { s.status_inner(&c).await })
            .await
    }
    pub(crate) async fn begin_browser(
        self: &Arc<Self>,
        c: &CancellationToken,
    ) -> Result<BrowserBegin> {
        self.owned(c, |s, c| async move { s.begin_browser_inner(&c).await })
            .await
    }
    pub(crate) async fn complete_browser(
        self: &Arc<Self>,
        id: Uuid,
        callback: Url,
        c: &CancellationToken,
    ) -> Result<()> {
        self.owned(c, move |s, c| async move {
            s.complete_browser_inner(id, callback, &c).await
        })
        .await
    }
    pub(crate) async fn begin_device(
        self: &Arc<Self>,
        c: &CancellationToken,
    ) -> Result<DeviceBegin> {
        self.owned(c, |s, c| async move { s.begin_device_inner(&c).await })
            .await
    }
    pub(crate) async fn attempt_status(self: &Arc<Self>, id: Uuid) -> Result<AttemptStatus> {
        self.owned(&CancellationToken::new(), move |s, _| async move {
            s.attempt_status_inner(id).await
        })
        .await
    }
    pub(crate) async fn cancel_attempt(self: &Arc<Self>, id: Uuid) -> Result<()> {
        self.owned(&CancellationToken::new(), move |s, _| async move {
            s.cancel_attempt_inner(id).await
        })
        .await
    }
    pub(crate) async fn logout(self: &Arc<Self>, c: &CancellationToken) -> Result<()> {
        self.owned(c, |s, c| async move { s.logout_inner(&c).await })
            .await
    }
    pub(crate) async fn recover(self: &Arc<Self>, c: &CancellationToken) -> Result<()> {
        self.owned(c, |s, c| async move {
            let mut lease = s.lease(&c).await?;
            let record = s.read(&mut lease, &c).await?;
            s.refresh(&mut lease, record, true, &c).await?;
            Ok(())
        })
        .await
    }
    pub(crate) async fn credential(
        self: &Arc<Self>,
        rejected: Option<&CredentialLease>,
        c: &CancellationToken,
    ) -> Result<CredentialLease> {
        let rejected = rejected.cloned();
        self.owned(c, move |s, c| async move {
            s.credential_inner(rejected.as_ref(), &c).await
        })
        .await
    }
    pub(crate) async fn finish_rejected(
        self: &Arc<Self>,
        rejected: &CredentialLease,
        c: &CancellationToken,
    ) -> Result<()> {
        let rejected = rejected.clone();
        self.owned(c, move |s, c| async move {
            s.finish_rejected_inner(&rejected, &c).await
        })
        .await
    }
}
impl Service {
    pub fn new(
        files: Arc<FileStorage>,
        root: usize,
        key: &'static str,
        config: Config,
        production: &Config,
    ) -> Result<Arc<Self>> {
        config.validate(production)?;
        let state_key = match key {
            "openaiCodex" => "openaiCodexAccountState",
            "xaiGrokBuild" => "xaiGrokBuildAccountState",
            _ => return Err(OAuthError::Config("credential authority")),
        };
        Ok(Arc::new(Self {
            path: files.path(root, AUTH_FILE)?,
            files,
            key,
            state_key,
            config,
            client: OnceCell::new(),
            #[cfg(feature = "testing")]
            fail_client_build: std::sync::atomic::AtomicBool::new(false),
            stopping: CancellationToken::new(),
            attempts: Mutex::new(HashMap::new()),
            tasks: TaskTracker::new(),
            admission: std::sync::Mutex::new(()),
        }))
    }
    async fn client(&self) -> Result<&Client> {
        self.client
            .get_or_try_init(|| async {
                #[cfg(feature = "testing")]
                if self
                    .fail_client_build
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(OAuthError::Transient(None));
                }
                Client::builder()
                    .timeout(Duration::from_secs(15))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| OAuthError::Transient(None))
            })
            .await
    }
    /// Feature `testing` only: whether a token operation has built the client.
    #[cfg(feature = "testing")]
    pub(crate) fn client_initialized(&self) -> bool {
        self.client.initialized()
    }
    /// Feature `testing` only: fail the next HTTP client initialization.
    #[cfg(feature = "testing")]
    pub(crate) fn fail_next_client_build(&self) {
        self.fail_client_build
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    async fn lease(&self, cancel: &CancellationToken) -> Result<FileLease> {
        loop {
            if cancel.is_cancelled() || self.stopping.is_cancelled() {
                return Err(OAuthError::Closed);
            }
            match self.files.lock(&self.path, cancel).await {
                Ok(x) => return Ok(x),
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = self.stopping.cancelled() => return Err(OAuthError::Closed),
                    _ = cancel.cancelled() => return Err(OAuthError::Closed),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                },
                Err(e) => return Err(e.into()),
            }
        }
    }
    async fn read(&self, lease: &mut FileLease, cancel: &CancellationToken) -> Result<Record> {
        let document = match lease.read(cancel).await {
            Ok(bytes) => Self::document(&bytes),
            Err(FileStorageError::Io(e)) if e.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(e.into()),
        }?;
        self.parse_record(document)
    }
    /// Reads the current record without the account lease: writes are atomic
    /// replaces, so a fresh record never needs the marker or its fsync.
    async fn read_current(&self, cancel: &CancellationToken) -> Result<Record> {
        let document = match self.files.read(&self.path, cancel).await {
            Ok(bytes) => Self::document(&bytes),
            Err(FileStorageError::Io(e)) if e.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(e.into()),
        }?;
        self.parse_record(document)
    }
    fn document(bytes: &[u8]) -> Result<Map<String, Value>> {
        serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|x| x.as_object().cloned())
            .ok_or(OAuthError::InvalidStore("root must be an object"))
    }
    async fn write(lease: &mut FileLease, doc: Map<String, Value>) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(doc))
            .map_err(|_| OAuthError::InvalidStore("cannot encode"))?;
        bytes.push(b'\n');
        #[cfg(unix)]
        let permissions = Some(std::os::unix::fs::PermissionsExt::from_mode(0o600));
        #[cfg(not(unix))]
        let permissions = None;
        lease
            .replace(bytes, permissions, &CancellationToken::new())
            .await?;
        Ok(())
    }
    fn parse_record(&self, doc: Map<String, Value>) -> Result<Record> {
        if let Some(raw) = doc.get(self.state_key)
            && raw.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        {
            return Err(OAuthError::UnsupportedSchema);
        }
        let state = doc
            .get(self.state_key)
            .map(|raw| {
                serde_json::from_value::<AccountState>(raw.clone())
                    .map_err(|_| OAuthError::InvalidStore("account state"))
            })
            .transpose()?;
        if let Some(state) = &state
            && (state.generation.is_nil()
                || state.credential_revision > MAX_SAFE_INTEGER
                || state.token_digest.is_some() != (state.credential_revision > 0)
                || state
                    .token_digest
                    .as_ref()
                    .is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit())))
        {
            return Err(OAuthError::InvalidStore("account state"));
        }
        let digest = doc.get(self.key).map(hash_value).transpose()?;
        let tokens = doc
            .get(self.key)
            .map(|raw| {
                let mut value = raw.clone();
                if state.is_none() {
                    let fields = value
                        .as_object_mut()
                        .ok_or(OAuthError::InvalidStore("provider record"))?;
                    for (key, value) in [
                        (
                            "issuer",
                            self.config
                                .issuer
                                .to_string()
                                .trim_end_matches('/')
                                .to_owned(),
                        ),
                        ("tokenEndpoint", self.config.token.to_string()),
                        ("clientId", self.config.client_id.clone()),
                        ("scope", self.config.scope.clone()),
                        ("relayBaseUrl", self.config.relay.to_string()),
                    ] {
                        if !fields.contains_key(key) {
                            fields.insert(key.into(), Value::String(value));
                        }
                    }
                }
                serde_json::from_value::<Tokens>(value)
                    .map_err(|_| OAuthError::InvalidStore("provider record"))
            })
            .transpose()?;
        if let Some(tokens) = &tokens
            && (tokens.kind != "oauth"
                || tokens.access.trim().is_empty()
                || tokens.access.chars().any(char::is_control)
                || tokens.refresh.trim().is_empty()
                || tokens.expires > MAX_SAFE_INTEGER
                || tokens
                    .account_id
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty() || s.chars().any(char::is_control))
                || tokens.issuer.trim_end_matches('/')
                    != self.config.issuer.as_str().trim_end_matches('/')
                || tokens.token_endpoint != self.config.token.as_str()
                || tokens.client_id != self.config.client_id
                || tokens.relay_base_url.trim_end_matches('/')
                    != self.config.relay.as_str().trim_end_matches('/'))
        {
            return Err(OAuthError::InvalidStore("untrusted provider record"));
        }
        Ok(Record {
            document: doc,
            tokens,
            state,
            digest,
        })
    }
    async fn persist(
        &self,
        lease: &mut FileLease,
        record: Record,
        tokens: Option<&Tokens>,
        new_account: bool,
    ) -> Result<AccountState> {
        let mut doc = record.document;
        let mut state = record.state.unwrap_or(AccountState {
            schema_version: 1,
            generation: Uuid::new_v4(),
            credential_revision: 0,
            token_digest: None,
            extra: Map::new(),
        });
        if new_account {
            state.generation = Uuid::new_v4();
            state.credential_revision = 0;
        }
        if let Some(tokens) = tokens {
            state.credential_revision = state
                .credential_revision
                .checked_add(1)
                .filter(|n| *n <= MAX_SAFE_INTEGER)
                .ok_or(OAuthError::InvalidStore("credential revision exhausted"))?;
            let value = serde_json::to_value(tokens)
                .map_err(|_| OAuthError::InvalidStore("provider record"))?;
            state.token_digest = Some(hash_value(&value)?);
            doc.insert(self.key.into(), value);
        } else {
            state.token_digest = None;
            state.credential_revision = 0;
            doc.remove(self.key);
        }
        doc.insert(
            self.state_key.into(),
            serde_json::to_value(&state).map_err(|_| OAuthError::InvalidStore("account state"))?,
        );
        Self::write(lease, doc).await?;
        Ok(state)
    }
    async fn status_inner(&self, cancel: &CancellationToken) -> Result<AuthStatus> {
        let mut lease = self.lease(cancel).await?;
        let record = self.read(&mut lease, cancel).await?;
        let recovery_required =
            record.revision().is_err() || (record.state.is_none() && record.tokens.is_some());
        let t = record.tokens;
        Ok(AuthStatus {
            authenticated: t.is_some() && !recovery_required,
            recovery_required,
            expires_at: t.as_ref().map(|x| x.expires),
            account_id_preview: t.and_then(|x| x.account_id).map(|x| mask(&x)),
        })
    }
    async fn begin_browser_inner(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<BrowserBegin> {
        let authorization = self
            .config
            .authorization
            .clone()
            .ok_or(OAuthError::Config("browser flow unavailable"))?;
        let redirect = self
            .config
            .redirect
            .clone()
            .ok_or(OAuthError::Config("redirect URI"))?;
        let (verifier, challenge) = pkce();
        let state = random();
        let id = Uuid::new_v4();
        let expires_at = Self::now() + 300_000;
        let mut lease = self.lease(cancel).await?;
        let revision = self.read(&mut lease, cancel).await?.revision()?;
        drop(lease);
        let mut url = authorization;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair("scope", &self.config.scope)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", "codex_cli_rs");
        self.insert_attempt(
            id,
            Attempt {
                revision,
                status: AttemptStatus::Pending,
                cancel: CancellationToken::new(),
                expires_at,
                exchanging: false,
                kind: AttemptKind::Browser { state, verifier },
            },
        )
        .await?;
        Ok(BrowserBegin {
            attempt_id: id,
            authorization_url: url,
            expires_at,
        })
    }
    async fn complete_browser_inner(
        self: &Arc<Self>,
        id: Uuid,
        callback: Url,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (revision, state, verifier, attempt_cancel, expires_at) = {
            let mut attempts = self.attempts.lock().await;
            let a = attempts.get_mut(&id).ok_or(OAuthError::AttemptNotFound)?;
            if a.status != AttemptStatus::Pending {
                return Err(OAuthError::AttemptClosed);
            }
            if Self::now() >= a.expires_at {
                a.status = AttemptStatus::Expired;
                return Err(OAuthError::AttemptClosed);
            }
            if a.exchanging || a.cancel.is_cancelled() {
                return Err(OAuthError::AttemptClosed);
            }
            match &a.kind {
                AttemptKind::Browser { state, verifier } => (
                    a.revision.clone(),
                    state.clone(),
                    verifier.clone(),
                    a.cancel.clone(),
                    a.expires_at,
                ),
                _ => return Err(OAuthError::AttemptClosed),
            }
        };
        if callback.scheme() != "http"
            || callback.host_str() != Some("localhost")
            || callback.port() != Some(1455)
            || callback.path() != "/auth/callback"
            || !safe_url(&callback)
        {
            return Err(OAuthError::InvalidCallback("redirect URI"));
        }
        let mut q = HashMap::new();
        for (key, value) in callback.query_pairs().into_owned() {
            if q.insert(key, value).is_some() {
                return Err(OAuthError::InvalidCallback("duplicate parameter"));
            }
        }
        if q.get("state") != Some(&state) {
            return Err(OAuthError::InvalidCallback("state"));
        }
        let code = q
            .get("code")
            .filter(|x| !x.trim().is_empty() && !q.contains_key("error"))
            .ok_or(OAuthError::InvalidCallback("authorization code"))?;
        {
            let mut attempts = self.attempts.lock().await;
            let attempt = attempts.get_mut(&id).ok_or(OAuthError::AttemptNotFound)?;
            if attempt.status != AttemptStatus::Pending || attempt.exchanging {
                return Err(OAuthError::AttemptClosed);
            }
            attempt.exchanging = true;
        }
        let exchange = async {
            let redirect = self.config.redirect.as_ref().unwrap().as_str();
            let response = self
                .token_request()
                .await?
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", redirect),
                    ("client_id", &self.config.client_id),
                    ("code_verifier", &verifier),
                ])
                .send()
                .await
                .map_err(|_| OAuthError::Transient(None))?;
            let (status, value) = response_json(response).await?;
            if !status.is_success() {
                return Err(classify(status, &value));
            }
            self.tokens_from_value(value, None)
        };
        let result = match self
            .cancellable(exchange, cancel, &attempt_cancel, expires_at)
            .await
        {
            Ok(tokens) => self.commit_attempt(id, revision, tokens, cancel).await,
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            let mut attempts = self.attempts.lock().await;
            if let Some(attempt) = attempts.get_mut(&id)
                && attempt.status == AttemptStatus::Pending
            {
                attempt.status = if attempt.cancel.is_cancelled()
                    || cancel.is_cancelled()
                    || self.stopping.is_cancelled()
                {
                    AttemptStatus::Cancelled
                } else if Self::now() >= attempt.expires_at {
                    AttemptStatus::Expired
                } else {
                    AttemptStatus::Failed(error.to_string())
                };
            }
        }
        result
    }
    async fn begin_device_inner(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<DeviceBegin> {
        let endpoint = self
            .config
            .device
            .clone()
            .ok_or(OAuthError::Config("device flow unavailable"))?;
        let mut lease = self.lease(cancel).await?;
        let revision = self.read(&mut lease, cancel).await?.revision()?;
        drop(lease);
        let started = Self::now();
        let id = Uuid::new_v4();
        let attempt_cancel = CancellationToken::new();
        self.insert_attempt(
            id,
            Attempt {
                revision,
                status: AttemptStatus::Pending,
                cancel: attempt_cancel.clone(),
                expires_at: started + 15_000,
                exchanging: false,
                kind: AttemptKind::Device,
            },
        )
        .await?;
        let result = async {
            let request = async {
                let response = self
                    .client()
                    .await?
                    .post(endpoint)
                    .form(&[
                        ("client_id", self.config.client_id.as_str()),
                        ("scope", self.config.scope.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(|_| OAuthError::Transient(None))?;
                let (status, value) = response_json(response).await?;
                if !status.is_success() {
                    return Err(classify(status, &value));
                }
                Ok(value)
            };
            let v = self
                .cancellable(request, cancel, &attempt_cancel, started + 15_000)
                .await?;
            let device_code = req(&v, "device_code")?;
            let user_code = req(&v, "user_code")?;
            let verification_uri = Url::parse(&req(&v, "verification_uri")?)
                .map_err(|_| OAuthError::InvalidResponse("verification_uri"))?;
            self.verify_uri(&verification_uri)?;
            let complete = opt(&v, "verification_uri_complete")?
                .map(|x| {
                    Url::parse(&x)
                        .map_err(|_| OAuthError::InvalidResponse("verification_uri_complete"))
                })
                .transpose()?;
            if let Some(x) = &complete {
                self.verify_uri(x)?;
            }
            let interval = seconds(&v, "interval", Some(5))?;
            let expires_at = expiry(started, seconds(&v, "expires_in", None)?)?;
            if expires_at <= Self::now() {
                return Err(OAuthError::AttemptClosed);
            }
            let mut attempts = self.attempts.lock().await;
            attempts
                .get_mut(&id)
                .ok_or(OAuthError::AttemptClosed)?
                .expires_at = expires_at;
            drop(attempts);
            let _admission = self.admission.lock().unwrap();
            if self.stopping.is_cancelled()
                || cancel.is_cancelled()
                || attempt_cancel.is_cancelled()
            {
                attempt_cancel.cancel();
                return Err(OAuthError::Closed);
            }
            let owner = self.clone();
            self.tasks.spawn(async move {
                owner
                    .poll_device(id, device_code, interval, expires_at, attempt_cancel)
                    .await;
            });
            Ok(DeviceBegin {
                attempt_id: id,
                verification_uri,
                verification_uri_complete: complete,
                user_code,
                expires_at,
                interval_seconds: interval,
            })
        }
        .await;
        if result.is_err() {
            self.attempts.lock().await.remove(&id);
        }
        result
    }
    async fn insert_attempt(&self, id: Uuid, attempt: Attempt) -> Result<()> {
        let mut attempts = self.attempts.lock().await;
        attempts.retain(|_, a| a.expires_at > Self::now());
        if attempts.len() >= MAX_ATTEMPTS {
            attempts.retain(|_, a| a.status == AttemptStatus::Pending);
        }
        if attempts.len() >= MAX_ATTEMPTS {
            return Err(OAuthError::TooManyAttempts);
        }
        if self.stopping.is_cancelled() {
            return Err(OAuthError::Closed);
        }
        attempts.insert(id, attempt);
        Ok(())
    }
    async fn poll_device(
        self: Arc<Self>,
        id: Uuid,
        code: String,
        mut interval: u64,
        expires: u64,
        cancel: CancellationToken,
    ) {
        let result: Result<()> = async {
            loop {
                let poll = async {
                    tokio::time::sleep(Duration::from_secs(interval)).await;
                    let response = self
                        .token_request()
                        .await?
                        .form(&[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("device_code", code.as_str()),
                            ("client_id", self.config.client_id.as_str()),
                        ])
                        .send()
                        .await
                        .map_err(|_| OAuthError::Transient(None))?;
                    response_json(response).await
                };
                let (status, value) = self.cancellable(poll, &cancel, &cancel, expires).await?;
                if status.is_success() {
                    let tokens = self.tokens_from_value(value, None)?;
                    let revision = self
                        .attempts
                        .lock()
                        .await
                        .get(&id)
                        .ok_or(OAuthError::AttemptNotFound)?
                        .revision
                        .clone();
                    return self.commit_attempt(id, revision, tokens, &cancel).await;
                }
                match (status, value.get("error").and_then(Value::as_str)) {
                    (StatusCode::BAD_REQUEST, Some("authorization_pending")) => {}
                    (StatusCode::BAD_REQUEST, Some("slow_down")) => {
                        interval = interval.saturating_add(5)
                    }
                    _ => return Err(classify(status, &value)),
                }
            }
        }
        .await;
        if let Err(error) = result {
            let status = if cancel.is_cancelled() || self.stopping.is_cancelled() {
                AttemptStatus::Cancelled
            } else if Self::now() >= expires {
                AttemptStatus::Expired
            } else {
                AttemptStatus::Failed(error.to_string())
            };
            self.set_attempt(id, status).await;
        }
    }
    async fn set_attempt(&self, id: Uuid, status: AttemptStatus) {
        if let Some(a) = self.attempts.lock().await.get_mut(&id)
            && a.status == AttemptStatus::Pending
        {
            a.status = status;
        }
    }
    async fn attempt_status_inner(&self, id: Uuid) -> Result<AttemptStatus> {
        let mut attempts = self.attempts.lock().await;
        let attempt = attempts.get_mut(&id).ok_or(OAuthError::AttemptNotFound)?;
        if attempt.status == AttemptStatus::Pending && Self::now() >= attempt.expires_at {
            attempt.cancel.cancel();
            attempt.status = AttemptStatus::Expired;
        }
        Ok(attempt.status.clone())
    }
    async fn cancel_attempt_inner(&self, id: Uuid) -> Result<()> {
        let mut a = self.attempts.lock().await;
        let x = a.get_mut(&id).ok_or(OAuthError::AttemptNotFound)?;
        x.cancel.cancel();
        if x.status == AttemptStatus::Pending {
            x.status = AttemptStatus::Cancelled;
        }
        Ok(())
    }
    async fn commit_attempt(
        &self,
        id: Uuid,
        revision: Revision,
        tokens: Tokens,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (attempt_cancel, expires) = {
            let attempts = self.attempts.lock().await;
            let attempt = attempts.get(&id).ok_or(OAuthError::AttemptNotFound)?;
            (attempt.cancel.clone(), attempt.expires_at)
        };
        let mut lease = self
            .cancellable(self.lease(cancel), cancel, &attempt_cancel, expires)
            .await?;
        let record = self.read(&mut lease, cancel).await?;
        if record.revision()? != revision {
            return Err(OAuthError::AccountChanged);
        }
        let mut attempts = self.attempts.lock().await;
        let attempt = attempts.get_mut(&id).ok_or(OAuthError::AttemptNotFound)?;
        if attempt.status != AttemptStatus::Pending
            || attempt.cancel.is_cancelled()
            || Self::now() >= attempt.expires_at
            || self.stopping.is_cancelled()
            || cancel.is_cancelled()
        {
            return Err(OAuthError::AttemptClosed);
        }
        self.persist(&mut lease, record, Some(&tokens), true)
            .await?;
        attempt.status = AttemptStatus::Completed;
        Ok(())
    }
    async fn credential_inner(
        &self,
        rejected: Option<&CredentialLease>,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease> {
        // A fresh record is read without the account lease, so another
        // provider's refresh does not block it. Anything else re-reads under
        // the lease, where the decision and any rotation are serialized.
        if let Ok(record) = self.read_current(cancel).await
            && let Some(lease) = self.reusable(&record, rejected)?
        {
            return Ok(lease);
        }
        let mut lease = self.lease(cancel).await?;
        let record = self.read(&mut lease, cancel).await?;
        if let Some(lease) = self.reusable(&record, rejected)? {
            return Ok(lease);
        }
        self.refresh(&mut lease, record, false, cancel).await
    }
    /// The record's lease when it needs no token request, after checking that
    /// it still belongs to the rejected account.
    fn reusable(
        &self,
        record: &Record,
        rejected: Option<&CredentialLease>,
    ) -> Result<Option<CredentialLease>> {
        record.revision()?;
        let tokens = record.tokens.as_ref().ok_or(OAuthError::Missing)?;
        let state = record.state.as_ref().ok_or(OAuthError::RecoveryRequired)?;
        if rejected.is_some_and(|old| {
            old.generation != state.generation
                || old.account_id != tokens.account_id
                || old.authority != self.key
        }) {
            return Err(OAuthError::AccountChanged);
        }
        let fresh = tokens.expires > Self::now() + FRESH_MS;
        if rejected.is_some_and(|x| x.revision != state.credential_revision && fresh)
            || (rejected.is_none() && fresh)
        {
            return Ok(Some(to_lease(tokens.clone(), state, self.key)));
        }
        Ok(None)
    }
    async fn refresh(
        &self,
        lease: &mut FileLease,
        mut record: Record,
        new_account: bool,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease> {
        if self.stopping.is_cancelled() || cancel.is_cancelled() {
            return Err(OAuthError::Closed);
        }
        if !new_account
            && record
                .state
                .as_ref()
                .is_some_and(|s| s.credential_revision == MAX_SAFE_INTEGER)
        {
            return Err(OAuthError::InvalidStore("credential revision exhausted"));
        }
        let tokens = record.tokens.take().ok_or(OAuthError::Missing)?;
        let response = self
            .token_request()
            .await?
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", tokens.refresh.as_str()),
                ("client_id", tokens.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Transient(None))?;
        let (status, value) = response_json(response).await?;
        if !status.is_success() {
            let error = classify(status, &value);
            if matches!(error, OAuthError::Rejected) {
                self.persist(lease, record, None, true).await?;
            }
            return Err(error);
        }
        let account_id = tokens.account_id.clone();
        let tokens = self.tokens_from_value(value, Some(tokens))?;
        if !new_account && tokens.account_id != account_id {
            self.persist(lease, record, Some(&tokens), true).await?;
            return Err(OAuthError::AccountChanged);
        }
        let state = self
            .persist(lease, record, Some(&tokens), new_account)
            .await?;
        if self.stopping.is_cancelled() || cancel.is_cancelled() {
            return Err(OAuthError::Closed);
        }
        Ok(to_lease(tokens, &state, self.key))
    }
    async fn finish_rejected_inner(
        &self,
        rejected: &CredentialLease,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut lease = self.lease(cancel).await?;
        let record = self.read(&mut lease, cancel).await?;
        record.revision()?;
        if let (Some(tokens), Some(state)) = (&record.tokens, &record.state)
            && rejected.authority == self.key
            && rejected.generation == state.generation
            && rejected.revision == state.credential_revision
            && rejected.access == tokens.access
        {
            self.persist(&mut lease, record, None, true).await?;
        }
        Ok(())
    }
    async fn logout_inner(&self, cancel: &CancellationToken) -> Result<()> {
        let mut lease = self.lease(cancel).await?;
        let record = self.read(&mut lease, cancel).await?;
        let mut attempts = self.attempts.lock().await;
        if self.stopping.is_cancelled() || cancel.is_cancelled() {
            return Err(OAuthError::Closed);
        }
        self.persist(&mut lease, record, None, true).await?;
        for a in attempts.values_mut() {
            a.cancel.cancel();
            if a.status == AttemptStatus::Pending {
                a.status = AttemptStatus::Cancelled;
            }
        }
        Ok(())
    }
    async fn token_request(&self) -> Result<reqwest::RequestBuilder> {
        let request = self
            .client()
            .await?
            .post(self.config.token.clone())
            .header("Accept", "application/json");
        Ok(if self.key == "openaiCodex" {
            request.header("originator", "codex_cli_rs").header(
                "User-Agent",
                "codex_cli_rs/0.144.1 (Ubuntu 22.4.0; x86_64) xterm-256color",
            )
        } else {
            request
        })
    }
    fn verify_uri(&self, uri: &Url) -> Result<()> {
        let trusted = if self.config.issuer.scheme() == "http" {
            uri.origin() == self.config.issuer.origin()
        } else {
            uri.scheme() == "https"
                && uri.port_or_known_default() == Some(443)
                && uri
                    .host_str()
                    .is_some_and(|host| host == "x.ai" || host.ends_with(".x.ai"))
        };
        if !safe_url(uri) || !trusted {
            return Err(OAuthError::InvalidResponse("untrusted verification URI"));
        }
        Ok(())
    }
    async fn cancellable<T>(
        &self,
        operation: impl Future<Output = Result<T>>,
        caller: &CancellationToken,
        attempt: &CancellationToken,
        expires: u64,
    ) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.stopping.cancelled() => Err(OAuthError::Closed),
            _ = caller.cancelled() => Err(OAuthError::Closed),
            _ = attempt.cancelled() => Err(OAuthError::AttemptClosed),
            _ = tokio::time::sleep(Duration::from_millis(expires.saturating_sub(Self::now()))) => Err(OAuthError::AttemptClosed),
            result = operation => result,
        }
    }
    fn tokens_from_value(&self, value: Value, previous: Option<Tokens>) -> Result<Tokens> {
        let access = req(&value, "access_token")?;
        let refresh = opt(&value, "refresh_token")?
            .or_else(|| previous.as_ref().map(|x| x.refresh.clone()))
            .ok_or(OAuthError::InvalidResponse("refresh_token"))?;
        if let Some(kind) = opt(&value, "token_type")?
            && !kind.eq_ignore_ascii_case("bearer")
        {
            return Err(OAuthError::InvalidResponse("token_type"));
        }
        let access_claims = claims(value.get("access_token").and_then(Value::as_str));
        let id_claims = claims(value.get("id_token").and_then(Value::as_str));
        let (account_label, plan_type) = if self.key == "openaiCodex" {
            (
                claim(&id_claims, &["name", "preferred_username", "email"]),
                claim(
                    &access_claims,
                    &[
                        "https://api.openai.com/auth.plan_type",
                        "plan_type",
                        "chatgpt_plan_type",
                    ],
                ),
            )
        } else {
            let label_keys = &["email", "name", "preferred_username"];
            let plan_keys = &["plan_type", "planType", "tier", "subscription_tier"];
            (
                claim(&id_claims, label_keys).or_else(|| claim(&access_claims, label_keys)),
                claim(&access_claims, plan_keys).or_else(|| claim(&id_claims, plan_keys)),
            )
        };
        let account_id = if self.key == "openaiCodex" {
            codex_account_id(&access_claims)
                .or_else(|| codex_account_id(&id_claims))
                .or_else(|| previous.as_ref().and_then(|x| x.account_id.clone()))
        } else {
            claim(&access_claims, &["sub", "account_id", "accountId"])
                .or_else(|| claim(&id_claims, &["sub", "account_id", "accountId"]))
                .or_else(|| previous.as_ref().and_then(|x| x.account_id.clone()))
        };
        let mut extra = previous
            .as_ref()
            .map(|x| x.extra.clone())
            .unwrap_or_default();
        extra.remove("clientSecret");
        if account_id
            .as_ref()
            .is_some_and(|id| id.chars().any(char::is_control))
        {
            return Err(OAuthError::InvalidResponse("account ID"));
        }
        Ok(Tokens {
            kind: "oauth".into(),
            refresh,
            access,
            expires: expiry(Self::now(), seconds(&value, "expires_in", Some(3600))?)?,
            account_id,
            account_label: account_label
                .or_else(|| previous.as_ref().and_then(|x| x.account_label.clone())),
            plan_type: plan_type.or_else(|| previous.as_ref().and_then(|x| x.plan_type.clone())),
            email: claim(&id_claims, &["email"])
                .or_else(|| claim(&access_claims, &["email"]))
                .or_else(|| previous.as_ref().and_then(|x| x.email.clone())),
            issuer: self.config.issuer.to_string().trim_end_matches('/').into(),
            authorization_endpoint: self.config.authorization.as_ref().map(ToString::to_string),
            token_endpoint: self.config.token.to_string(),
            device_authorization_endpoint: self.config.device.as_ref().map(ToString::to_string),
            device_token_endpoint: self
                .config
                .device
                .as_ref()
                .map(|_| self.config.token.to_string()),
            client_id: self.config.client_id.clone(),
            scope: opt(&value, "scope")?.unwrap_or_else(|| {
                previous
                    .as_ref()
                    .map_or_else(|| self.config.scope.clone(), |t| t.scope.clone())
            }),
            relay_base_url: self.config.relay.to_string().trim_end_matches('/').into(),
            extra,
        })
    }
}

async fn response_json(mut response: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| OAuthError::Transient(None))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(OAuthError::InvalidResponse("response too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| OAuthError::InvalidResponse("JSON"))?;
    Ok((status, value))
}

fn req(v: &Value, key: &'static str) -> Result<String> {
    opt(v, key)?.ok_or(OAuthError::InvalidResponse(key))
}
fn opt(v: &Value, key: &'static str) -> Result<Option<String>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(x)) if !x.trim().is_empty() && !x.chars().any(char::is_control) => {
            Ok(Some(x.clone()))
        }
        _ => Err(OAuthError::InvalidResponse(key)),
    }
}
fn classify(status: StatusCode, v: &Value) -> OAuthError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        OAuthError::Transient(Some(status.as_u16()))
    } else if status.is_client_error()
        && matches!(
            v.get("error").and_then(Value::as_str),
            Some("invalid_grant" | "access_denied" | "authorization_denied" | "expired_token")
        )
    {
        OAuthError::Rejected
    } else {
        OAuthError::Transient(Some(status.as_u16()))
    }
}
fn random() -> String {
    URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes())
}
fn pkce() -> (String, String) {
    let verifier = format!("{}{}{}", random(), random(), random());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}
fn claims(token: Option<&str>) -> Option<Map<String, Value>> {
    let payload = token?.split('.').nth(1)?;
    serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(payload).ok()?)
        .ok()?
        .as_object()
        .cloned()
}
fn claim(claims: &Option<Map<String, Value>>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        claims
            .as_ref()?
            .get(*key)?
            .as_str()
            .filter(|x| !x.trim().is_empty())
            .map(str::to_owned)
    })
}
fn codex_account_id(claims: &Option<Map<String, Value>>) -> Option<String> {
    let direct = claim(
        claims,
        &[
            "https://api.openai.com/auth.chatgpt_account_id",
            "chatgpt_account_id",
            "account_id",
            "accountId",
            "https://api.openai.com/auth/account_id",
        ],
    );
    direct.or_else(|| {
        let auth = claims
            .as_ref()?
            .get("https://api.openai.com/auth")?
            .as_object()?;
        claim(&Some(auth.clone()), &["chatgpt_account_id"])
    })
}
fn mask(value: &str) -> String {
    let chars: Vec<_> = value.chars().collect();
    if chars.len() <= 6 {
        "***".into()
    } else {
        format!(
            "{}…{}",
            chars[..3].iter().collect::<String>(),
            chars[chars.len() - 3..].iter().collect::<String>()
        )
    }
}
fn to_lease(t: Tokens, state: &AccountState, authority: &'static str) -> CredentialLease {
    CredentialLease {
        access: t.access,
        account_id: t.account_id,
        relay_base_url: t.relay_base_url,
        authority,
        generation: state.generation,
        revision: state.credential_revision,
    }
}

fn safe_url(url: &Url) -> bool {
    url.username().is_empty() && url.password().is_none() && url.fragment().is_none()
}
fn hash_value(value: &Value) -> Result<String> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| OAuthError::InvalidStore("record digest"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn seconds(value: &Value, key: &'static str, default: Option<u64>) -> Result<u64> {
    let seconds = match value.get(key) {
        None => default,
        Some(Value::String(value)) => value.parse().ok(),
        Some(value) => value.as_u64(),
    };
    seconds
        .filter(|n| *n > 0 && *n <= MAX_SAFE_INTEGER / 1000)
        .ok_or(OAuthError::InvalidResponse(key))
}
fn expiry(now: u64, seconds: u64) -> Result<u64> {
    seconds
        .checked_mul(1000)
        .and_then(|ms| now.checked_add(ms))
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or(OAuthError::InvalidResponse("expires_in"))
}
