//! Mcp's process-scope OAuth credential authority for HTTP and SSE servers
//! ([design](../../../../docs/design/appserver/mcp-oauth.md)). It owns the
//! credential store, the pending sign-in attempts and the per-key challenge
//! state; connections take a bearer from it for every request.
mod oauth;
mod store;

use super::config::{BearerAuth, McpOAuthSettings, McpServerSettings};
use dim_agent_capabilities::file_storage::FileStorage;
use reqwest_mcp::Url;
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Instant,
};
#[cfg(feature = "testing")]
pub use store::testing;
use store::{Document, Record, Store};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

const DEFAULT_CALLBACK_PORT: u16 = 1456;
const ATTEMPT_MS: u64 = 10 * 60 * 1000;
const FRESH_MS: u64 = 30_000;

#[derive(Debug, thiserror::Error)]
pub enum McpAuthError {
    #[error("OAuth is not available for this MCP server or credential store")]
    Unsupported,
    #[error("OAuth discovery for this MCP server failed")]
    DiscoveryFailed,
    #[error("no usable OAuth client for this MCP server; set oauth.clientId")]
    ClientRequired,
    #[error("the sign-in attempt is no longer pending")]
    AttemptClosed,
    #[error("the callback URL does not match the sign-in attempt")]
    CallbackInvalid,
    #[error("the authorization server refused the sign-in ({0})")]
    Failed(&'static str),
    #[error("the authorization server or the credential store is unavailable")]
    Unavailable,
    #[error("MCP OAuth is closed")]
    Closed,
    #[error("MCP OAuth operation cancelled")]
    Cancelled,
}
impl McpAuthError {
    /// The `appError` code (design §9.3).
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "mcp_auth_unsupported",
            Self::DiscoveryFailed => "mcp_auth_discovery_failed",
            Self::ClientRequired => "mcp_auth_client_required",
            Self::AttemptClosed => "mcp_auth_attempt_closed",
            Self::CallbackInvalid => "mcp_auth_callback_invalid",
            Self::Failed(_) => "mcp_auth_failed",
            Self::Unavailable => "mcp_auth_unavailable",
            Self::Closed => "service_reloading",
            Self::Cancelled => "operation_interrupted",
        }
    }
}

/// Credentials are shared by every source and Session that configures the
/// same server `id` at the same URL (design D8).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct CredentialKey {
    server_id: String,
    url: String,
}

impl CredentialKey {
    /// The URL serialized by the URL parser, with a lone `/` path dropped.
    fn new(id: &str, url: &Url) -> Self {
        let mut canonical = url.to_string();
        if url.path() == "/" && url.query().is_none() {
            canonical.pop();
        }
        Self {
            server_id: id.to_owned(),
            url: canonical,
        }
    }
}

fn remote_url(settings: &McpServerSettings) -> Option<Url> {
    let McpServerSettings::Remote { url, .. } = settings else {
        return None;
    };
    Url::parse(url).ok().filter(|url| {
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none()
    })
}

/// OAuth applies to a remote entry without static credentials whose URL is
/// HTTPS or loopback HTTP (design D15). Returns its key and parsed URL.
pub(crate) fn credential_key(
    id: &str,
    settings: &McpServerSettings,
) -> Option<(CredentialKey, Url)> {
    let McpServerSettings::Remote { headers, auth, .. } = settings else {
        return None;
    };
    let url = remote_url(settings)?;
    let static_credentials = matches!(auth, Some(BearerAuth::Bearer { .. }))
        || headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"));
    if static_credentials
        || !(url.scheme() == "https" || (url.scheme() == "http" && oauth::loopback(&url)))
    {
        return None;
    }
    Some((CredentialKey::new(id, &url), url))
}

/// The key a remote entry's credentials are stored under, even once the
/// entry no longer uses OAuth, so they can still be signed out.
pub(crate) fn stored_key(id: &str, settings: &McpServerSettings) -> Option<CredentialKey> {
    remote_url(settings).map(|url| CredentialKey::new(id, &url))
}

/// Why a connection cannot authorize a request.
pub(crate) enum Refusal {
    /// No credential: the user has not signed in.
    Required,
    /// The credential exists but can no longer be used.
    Expired,
    /// The credential store or the authorization server is unavailable.
    Unavailable,
}

/// The catalog's view of one key.
pub(crate) enum AuthState {
    Required,
    Authorized { missing_scopes: Vec<String> },
    Expired,
}

/// The `mcp.auth.start` reply.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Started {
    pub attempt_id: Uuid,
    pub authorization_url: String,
    pub redirect_uri: String,
    pub resource: String,
    pub issuer: String,
    pub expires_at: u64,
}

struct Attempt {
    id: Uuid,
    expires_at: u64,
    exchanging: bool,
    authorization: oauth::Authorization,
}

/// A key's sign-in attempts (design §7).
#[derive(Default)]
struct Attempts {
    /// The key's current attempt; only it may redeem its code.
    current: Option<Attempt>,
    /// Completed sign-outs. A start that read the record before one
    /// completed cannot make its attempt current.
    signouts: u64,
}
impl Attempts {
    /// Whether `id` is the current attempt.
    fn is(&self, id: Uuid) -> bool {
        self.current
            .as_ref()
            .is_some_and(|attempt| attempt.id == id)
    }
    /// The current attempt, if it is `id`.
    fn get(&mut self, id: Uuid) -> Option<&mut Attempt> {
        self.current.as_mut().filter(|attempt| attempt.id == id)
    }
}

struct KeyState {
    /// Notified on every change to the key's record; the value says whether
    /// the record holds tokens.
    usable: watch::Sender<bool>,
    /// The last `WWW-Authenticate` of a `401` or `403` from the server.
    challenge: Option<String>,
    /// Scopes a `403 insufficient_scope` asked for.
    missing_scopes: Vec<String>,
}
impl Default for KeyState {
    fn default() -> Self {
        Self {
            usable: watch::Sender::new(false),
            challenge: None,
            missing_scopes: Vec::new(),
        }
    }
}

pub struct McpAuth {
    store: Store,
    http: tokio::sync::OnceCell<reqwest_mcp::Client>,
    #[cfg(feature = "testing")]
    fail_http_build: std::sync::atomic::AtomicBool,
    keys: Mutex<HashMap<CredentialKey, KeyState>>,
    /// Each key's attempts. This lock orders attempt state (design §7):
    /// every change to it holds the lock, and an exchange holds it from its
    /// final currency check through its publication. Its holders never wait
    /// for the store lock or a network request.
    attempts: tokio::sync::Mutex<HashMap<CredentialKey, Attempts>>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    /// Token requests: once sent, settled here whoever waits for them.
    tasks: TaskTracker,
}

impl McpAuth {
    pub(super) fn new(
        files: Arc<FileStorage>,
        root: usize,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Arc::new(Self {
            store: Store::new(files, root)?,
            http: tokio::sync::OnceCell::new(),
            #[cfg(feature = "testing")]
            fail_http_build: std::sync::atomic::AtomicBool::new(false),
            keys: Mutex::default(),
            attempts: tokio::sync::Mutex::default(),
            stopping: CancellationToken::new(),
            gate: Mutex::new(()),
            tasks: TaskTracker::new(),
        }))
    }

    async fn http(&self) -> Result<&reqwest_mcp::Client, McpAuthError> {
        self.http
            .get_or_try_init(|| async {
                #[cfg(feature = "testing")]
                if self
                    .fail_http_build
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(McpAuthError::Unavailable);
                }
                oauth::http()
            })
            .await
    }

    /// Feature `testing` only: fail the next OAuth HTTP client initialization.
    #[cfg(feature = "testing")]
    pub fn fail_next_oauth_client_build(&self) {
        self.fail_http_build
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Feature `testing` only: whether an OAuth request has built the client.
    #[cfg(feature = "testing")]
    pub fn oauth_client_initialized(&self) -> bool {
        self.http.initialized()
    }

    /// Drops pending attempts and waits for token requests already sent.
    pub(super) async fn close(&self) {
        {
            let _gate = self.gate.lock().unwrap();
            self.stopping.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }

    pub(crate) fn credential(self: &Arc<Self>, key: CredentialKey) -> Credential {
        Credential {
            auth: self.clone(),
            key,
        }
    }

    fn key<T>(&self, key: &CredentialKey, change: impl FnOnce(&mut KeyState) -> T) -> T {
        change(self.keys.lock().unwrap().entry(key.clone()).or_default())
    }

    fn changed(&self, key: &CredentialKey, usable: bool) {
        self.key(key, |state| state.usable.send_replace(usable));
    }

    /// The attempts lock, for a caller that may leave while a publication
    /// holds it.
    async fn lock_attempts(
        &self,
        cancel: &CancellationToken,
    ) -> Result<tokio::sync::MutexGuard<'_, HashMap<CredentialKey, Attempts>>, McpAuthError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpAuthError::Cancelled),
            attempts = self.attempts.lock() => Ok(attempts),
        }
    }

    /// Runs `work` on Mcp's own task: the caller leaving does not abandon a
    /// token request that was sent, and Mcp's unload waits for it.
    async fn owned<T, Fut>(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        work: impl FnOnce(Arc<Self>) -> Fut,
    ) -> Result<T, McpAuthError>
    where
        T: Send + 'static,
        Fut: Future<Output = Result<T, McpAuthError>> + Send + 'static,
    {
        let task = {
            let _gate = self.gate.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(McpAuthError::Closed);
            }
            self.tasks.spawn(work(self.clone()))
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpAuthError::Cancelled),
            result = task => result.map_err(|_| McpAuthError::Unavailable)?,
        }
    }

    /// The catalog's auth state of each key, from one store read. An
    /// unreadable store reports only what this process has seen.
    pub(crate) async fn states(&self, cancel: &CancellationToken) -> States {
        States {
            document: self.store.read(cancel).await.ok(),
            keys: self
                .keys
                .lock()
                .unwrap()
                .iter()
                .map(|(key, state)| {
                    (
                        key.clone(),
                        (state.challenge.is_some(), state.missing_scopes.clone()),
                    )
                })
                .collect(),
        }
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        key: &CredentialKey,
        server: &Url,
        settings: Option<&McpOAuthSettings>,
        client_secret: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<Started, McpAuthError> {
        let started = Instant::now();
        let result = async {
            if self.stopping.is_cancelled() {
                return Err(McpAuthError::Closed);
            }
            let signouts = self
                .lock_attempts(cancel)
                .await?
                .get(key)
                .map_or(0, |slot| slot.signouts);
            let previous = self.store.read(cancel).await?.get(key).cloned();
            let (challenge, missing_scopes) = self.key(key, |state| {
                (state.challenge.clone(), state.missing_scopes.clone())
            });
            let port = settings
                .and_then(|settings| settings.callback_port)
                .unwrap_or(DEFAULT_CALLBACK_PORT);
            let redirect_uri = format!("http://localhost:{port}/callback");
            let request = oauth::Request {
                server,
                challenge: challenge.as_deref(),
                settings,
                redirect_uri: &redirect_uri,
                client_secret,
                previous: previous.as_ref(),
                missing_scopes: &missing_scopes,
            };
            let authorization = tokio::select! {
                biased;
                _ = self.stopping.cancelled() => return Err(McpAuthError::Closed),
                _ = cancel.cancelled() => return Err(McpAuthError::Cancelled),
                result = async { oauth::authorize(self.http().await?, request).await } => result?,
            };
            let id = Uuid::new_v4();
            let expires_at = oauth::now() + ATTEMPT_MS;
            let reply = Started {
                attempt_id: id,
                authorization_url: authorization.url.clone(),
                redirect_uri,
                resource: authorization.binding.resource.clone(),
                issuer: authorization.binding.issuer.clone(),
                expires_at,
            };
            let mut attempts = self.lock_attempts(cancel).await?;
            let slot = attempts.entry(key.clone()).or_default();
            if slot.signouts != signouts {
                return Err(McpAuthError::AttemptClosed);
            }
            slot.current = Some(Attempt {
                id,
                expires_at,
                exchanging: false,
                authorization,
            });
            Ok(reply)
        }
        .await;
        trace(
            key,
            result.as_ref().ok().map(|r| r.issuer.as_str()),
            "start",
            outcome(&result),
            started,
        );
        result
    }

    pub(crate) async fn cancel_attempt(
        &self,
        attempt_id: Uuid,
        cancel: &CancellationToken,
    ) -> Result<(), McpAuthError> {
        let mut attempts = self.lock_attempts(cancel).await?;
        let slot = attempts
            .values_mut()
            .find(|slot| slot.is(attempt_id))
            .ok_or(McpAuthError::AttemptClosed)?;
        slot.current = None;
        Ok(())
    }

    pub(crate) async fn complete(
        self: &Arc<Self>,
        attempt_id: Uuid,
        callback: &str,
        cancel: &CancellationToken,
    ) -> Result<(), McpAuthError> {
        let started = Instant::now();
        let (key, issuer, code) = {
            let mut attempts = self.lock_attempts(cancel).await?;
            let (key, slot) = attempts
                .iter_mut()
                .find(|(_, slot)| slot.is(attempt_id))
                .ok_or(McpAuthError::AttemptClosed)?;
            let attempt = slot.current.as_mut().unwrap();
            if attempt.exchanging {
                return Err(McpAuthError::AttemptClosed);
            }
            if oauth::now() >= attempt.expires_at {
                slot.current = None;
                return Err(McpAuthError::AttemptClosed);
            }
            let authorization = &attempt.authorization;
            let issuer = authorization.binding.issuer.clone();
            match oauth::callback(
                callback,
                &authorization.client.redirect_uri,
                &authorization.state,
                &authorization.binding.issuer,
                authorization.iss_required,
            )? {
                oauth::Callback::Refused(code) => {
                    slot.current = None;
                    return Err(McpAuthError::Failed(code));
                }
                oauth::Callback::Code(code) => {
                    attempt.exchanging = true;
                    (key.clone(), issuer, code)
                }
            }
        };
        let exchange_key = key.clone();
        let result = self
            .owned(cancel, move |auth| async move {
                auth.exchange(exchange_key, attempt_id, code).await
            })
            .await;
        trace(&key, Some(&issuer), "complete", outcome(&result), started);
        result
    }

    /// Redeems the code of `attempt_id` for as long as it is the key's
    /// current attempt (design §7). A sign-out or a newer start supersedes
    /// it until the final currency check, once the response is in. From
    /// that check through the store write and the wake the attempts lock is
    /// held, so anything that supersedes it later is ordered after its
    /// publication.
    async fn exchange(
        self: Arc<Self>,
        key: CredentialKey,
        attempt_id: Uuid,
        code: String,
    ) -> Result<(), McpAuthError> {
        let mut locked = match self.store.lock(&self.stopping).await {
            Ok(locked) => locked,
            Err(error) => {
                if let Some(attempt) = self
                    .attempts
                    .lock()
                    .await
                    .get_mut(&key)
                    .and_then(|slot| slot.get(attempt_id))
                {
                    attempt.exchanging = false;
                }
                return Err(closed(error));
            }
        };
        let current = self
            .attempts
            .lock()
            .await
            .get_mut(&key)
            .and_then(|slot| slot.get(attempt_id))
            .map(|attempt| {
                let a = &attempt.authorization;
                (a.verifier.clone(), a.client.clone(), a.binding.clone())
            });
        let Some((verifier, client, binding)) = current else {
            return Err(McpAuthError::AttemptClosed);
        };
        let response = match self.http().await {
            Ok(http) => {
                oauth::token(
                    http,
                    &binding.token_endpoint,
                    &client,
                    &binding.resource,
                    oauth::Grant::Code {
                        code: &code,
                        verifier: &verifier,
                    },
                    &binding.requested_scopes,
                )
                .await
            }
            Err(_) => Err(oauth::TokenError::Unavailable),
        };
        let mut attempts = self.attempts.lock().await;
        let Some(slot) = attempts.get_mut(&key).filter(|slot| slot.is(attempt_id)) else {
            drop(attempts);
            if let (Ok(tokens), Some(endpoint)) = (&response, &binding.revocation_endpoint) {
                self.revoke(endpoint, &key.url, &client, tokens).await;
            }
            return Err(McpAuthError::AttemptClosed);
        };
        let mut unwritten = None;
        let result = match response {
            Ok(tokens) => {
                let record = Record::new(&key, binding.clone(), client.clone(), tokens.clone());
                locked.document.put(record);
                let written = locked.write().await;
                if written.is_ok() {
                    self.key(&key, |state| {
                        state.challenge = None;
                        state.missing_scopes.clear();
                    });
                    self.changed(&key, true);
                } else {
                    unwritten = Some(tokens);
                }
                written
            }
            Err(oauth::TokenError::Unavailable) => {
                if let Some(attempt) = &mut slot.current {
                    attempt.exchanging = false;
                }
                return Err(McpAuthError::Unavailable);
            }
            Err(oauth::TokenError::InvalidClient) => {
                if let Some(record) = locked.document.get_mut(&key)
                    && record
                        .client
                        .as_ref()
                        .is_some_and(|stored| stored.client_id == client.client_id)
                {
                    record.client = None;
                    record.tokens = None;
                    if locked.write().await.is_ok() {
                        self.changed(&key, false);
                    }
                }
                Err(McpAuthError::Failed("invalid_client"))
            }
            Err(oauth::TokenError::Rejected) => Err(McpAuthError::Failed("invalid_grant")),
            Err(oauth::TokenError::Failed(code)) => Err(McpAuthError::Failed(code)),
        };
        slot.current = None;
        drop(attempts);
        if let (Some(tokens), Some(endpoint)) = (&unwritten, &binding.revocation_endpoint) {
            self.revoke(endpoint, &key.url, &client, tokens).await;
        }
        result
    }

    /// Revokes the refresh token, then the access token (RFC 7009), best
    /// effort. Returns whether the authorization server confirmed both.
    async fn revoke(
        &self,
        endpoint: &str,
        resource: &str,
        client: &store::Client,
        tokens: &store::Tokens,
    ) -> bool {
        let Some(endpoint) = oauth::stored_endpoint(endpoint, resource) else {
            return false;
        };
        let Ok(http) = self.http().await else {
            return false;
        };
        let mut revoked = true;
        if let Some(refresh) = &tokens.refresh_token {
            revoked &= oauth::revoke(http, &endpoint, client, refresh, "refresh_token").await;
        }
        revoked &= oauth::revoke(
            http,
            &endpoint,
            client,
            &tokens.access_token,
            "access_token",
        )
        .await;
        revoked
    }

    pub(crate) async fn logout(
        self: &Arc<Self>,
        key: &CredentialKey,
        cancel: &CancellationToken,
    ) -> Result<bool, McpAuthError> {
        let started = Instant::now();
        let target = key.clone();
        let result = async {
            if let Some(slot) = self.lock_attempts(cancel).await?.get_mut(key) {
                slot.current = None;
            }
            self.owned(cancel, move |auth| async move {
                let mut locked = auth.store.lock(&auth.stopping).await.map_err(closed)?;
                let record = locked.document.remove(&target);
                let revoked = match &record {
                    Some(Record {
                        revocation_endpoint: Some(endpoint),
                        client: Some(client),
                        tokens: Some(tokens),
                        ..
                    }) => auth.revoke(endpoint, &target.url, client, tokens).await,
                    _ => false,
                };
                if record.is_some() {
                    locked.write().await?;
                }
                {
                    let mut attempts = auth.attempts.lock().await;
                    let slot = attempts.entry(target.clone()).or_default();
                    slot.current = None;
                    slot.signouts += 1;
                    auth.key(&target, |state| {
                        state.challenge = None;
                        state.missing_scopes.clear();
                    });
                    auth.changed(&target, false);
                }
                Ok((revoked, record.map(|record| record.issuer)))
            })
            .await
        }
        .await;
        let issuer = result.as_ref().ok().and_then(|(_, issuer)| issuer.clone());
        trace(key, issuer.as_deref(), "logout", outcome(&result), started);
        result.map(|(revoked, _)| revoked)
    }

    /// Refreshes the key's tokens, holding the store lock from the re-read
    /// through the request to the replace, so one refresh runs per key
    /// across Sessions and daemons sharing the home (design §5.6).
    async fn refresh(self: Arc<Self>, key: CredentialKey, stale: Stale) -> Result<String, Refusal> {
        let started = Instant::now();
        let mut locked = self
            .store
            .lock(&self.stopping)
            .await
            .map_err(|_| Refusal::Unavailable)?;
        let record = locked.document.get(&key).ok_or(Refusal::Required)?;
        let tokens = record.tokens.as_ref().ok_or(Refusal::Expired)?;
        match &stale {
            Stale::Expiring if fresh(tokens) => return Ok(tokens.access_token.clone()),
            Stale::Rejected(sent) if *sent != tokens.access_token => {
                return Ok(tokens.access_token.clone());
            }
            _ => {}
        }
        let issuer = record.issuer.clone();
        let still_valid = tokens.expires_at.is_none_or(|at| at > oauth::now());
        let current = tokens.access_token.clone();
        let (Some(refresh_token), Some(client)) =
            (tokens.refresh_token.clone(), record.client.clone())
        else {
            if matches!(stale, Stale::Expiring) && still_valid {
                return Ok(current);
            }
            locked.document.get_mut(&key).unwrap().tokens = None;
            locked.write().await.map_err(|_| Refusal::Unavailable)?;
            self.changed(&key, false);
            return Err(Refusal::Expired);
        };
        if self.stopping.is_cancelled() {
            return Err(Refusal::Unavailable);
        }
        let response = match self.http().await {
            Ok(http) => {
                oauth::token(
                    http,
                    &record.token_endpoint,
                    &client,
                    &record.resource,
                    oauth::Grant::Refresh(&refresh_token),
                    &tokens.scopes,
                )
                .await
            }
            Err(_) => Err(oauth::TokenError::Unavailable),
        };
        let record = locked.document.get_mut(&key).unwrap();
        let result = match response {
            Ok(mut tokens) => {
                tokens.refresh_token.get_or_insert(refresh_token);
                let access = tokens.access_token.clone();
                record.tokens = Some(tokens);
                locked.write().await.map_err(|_| Refusal::Unavailable)?;
                self.changed(&key, true);
                Ok(access)
            }
            // Only a refused grant or client ends the credential (§5.6).
            Err(error @ (oauth::TokenError::Rejected | oauth::TokenError::InvalidClient)) => {
                record.tokens = None;
                if matches!(error, oauth::TokenError::InvalidClient) {
                    record.client = None;
                }
                locked.write().await.map_err(|_| Refusal::Unavailable)?;
                self.changed(&key, false);
                Err(Refusal::Expired)
            }
            Err(oauth::TokenError::Unavailable | oauth::TokenError::Failed(_)) => match stale {
                Stale::Expiring if still_valid => Ok(current),
                _ => Err(Refusal::Unavailable),
            },
        };
        trace(
            &key,
            Some(&issuer),
            "refresh",
            match &result {
                Ok(_) => "ok",
                Err(refusal) => refusal.code(),
            },
            started,
        );
        result
    }

    /// Marks the key expired after a refreshed token was refused too.
    async fn expire(self: Arc<Self>, key: CredentialKey, sent: String) -> Result<(), Refusal> {
        let mut locked = self
            .store
            .lock(&self.stopping)
            .await
            .map_err(|_| Refusal::Unavailable)?;
        if let Some(record) = locked.document.get_mut(&key)
            && record
                .tokens
                .as_ref()
                .is_some_and(|tokens| tokens.access_token == sent)
        {
            record.tokens = None;
            locked.write().await.map_err(|_| Refusal::Unavailable)?;
            self.changed(&key, false);
        }
        Ok(())
    }
}

enum Stale {
    /// Less than 30 s left.
    Expiring,
    /// The server answered `401` to this access token.
    Rejected(String),
}

fn fresh(tokens: &store::Tokens) -> bool {
    tokens
        .expires_at
        .is_none_or(|at| at > oauth::now() + FRESH_MS)
}

fn closed(error: McpAuthError) -> McpAuthError {
    match error {
        McpAuthError::Cancelled => McpAuthError::Closed,
        other => other,
    }
}

fn outcome<T>(result: &Result<T, McpAuthError>) -> &'static str {
    result.as_ref().map_or_else(McpAuthError::code, |_| "ok")
}

/// One event per auth operation, without secrets: server id, issuer host,
/// outcome code and duration (design §11).
fn trace(
    key: &CredentialKey,
    issuer: Option<&str>,
    operation: &'static str,
    outcome: &'static str,
    started: Instant,
) {
    let issuer_host = issuer
        .and_then(|issuer| Url::parse(issuer).ok())
        .and_then(|url| url.host_str().map(str::to_owned));
    tracing::info!(
        target: "mcp.auth",
        server = %key.server_id,
        issuer_host = issuer_host.as_deref().unwrap_or("-"),
        operation,
        outcome,
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

impl Refusal {
    fn code(&self) -> &'static str {
        match self {
            Self::Required => "auth_required",
            Self::Expired => "auth_expired",
            Self::Unavailable => "mcp_auth_unavailable",
        }
    }
}

pub(crate) struct States {
    document: Option<Document>,
    keys: HashMap<CredentialKey, (bool, Vec<String>)>,
}
impl States {
    /// `None` is the `absent` state: no record and no challenge seen.
    pub(crate) fn get(&self, key: &CredentialKey) -> Option<AuthState> {
        let (challenged, missing) = self
            .keys
            .get(key)
            .map_or((false, &[][..]), |(challenged, missing)| {
                (*challenged, missing.as_slice())
            });
        match self
            .document
            .as_ref()
            .and_then(|document| document.get(key))
        {
            Some(Record {
                tokens: Some(tokens),
                ..
            }) => Some(AuthState::Authorized {
                missing_scopes: missing
                    .iter()
                    .filter(|scope| !tokens.scopes.contains(scope))
                    .cloned()
                    .collect(),
            }),
            Some(_) => Some(AuthState::Expired),
            None => challenged.then_some(AuthState::Required),
        }
    }
}

/// One connection's credential: a bearer per request (design §5.6).
#[derive(Clone)]
pub(crate) struct Credential {
    auth: Arc<McpAuth>,
    key: CredentialKey,
}
impl Credential {
    /// Notified whenever the key's record changes, for example after
    /// sign-in; the value says whether it holds tokens.
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.auth.key(&self.key, |state| state.usable.subscribe())
    }

    /// The current access token, refreshed first when less than 30 s are
    /// left. `None` sends the request unauthenticated: no record exists.
    async fn bearer(&self) -> Result<Option<String>, Refusal> {
        let document = match self.auth.store.read(&self.auth.stopping).await {
            Ok(document) => document,
            Err(McpAuthError::Unsupported) => return Ok(None),
            Err(_) => return Err(Refusal::Unavailable),
        };
        match document.get(&self.key) {
            None => Ok(None),
            Some(Record { tokens: None, .. }) => Err(Refusal::Expired),
            Some(Record {
                tokens: Some(tokens),
                ..
            }) if fresh(tokens) => Ok(Some(tokens.access_token.clone())),
            Some(_) => self.refresh(Stale::Expiring).await.map(Some),
        }
    }

    async fn refresh(&self, stale: Stale) -> Result<String, Refusal> {
        let key = self.key.clone();
        self.auth
            .owned(&CancellationToken::new(), move |auth| async move {
                Ok(auth.refresh(key, stale).await)
            })
            .await
            .map_err(|_| Refusal::Unavailable)?
    }

    fn challenge(&self, challenge: Option<String>) {
        if let Some(challenge) = challenge {
            self.auth
                .key(&self.key, |state| state.challenge = Some(challenge));
        }
    }

    /// Runs one HTTP request with the bearer. On `401` it refreshes, or takes
    /// a token another Session refreshed meanwhile, and retries once; a
    /// second `401` expires the credential. `unauthorized` recognizes a
    /// `401` and returns its `WWW-Authenticate` value, if any.
    pub(crate) async fn send<T, E, Fut>(
        &self,
        call: impl Fn(Option<String>) -> Fut,
        unauthorized: impl Fn(&Result<T, E>) -> Option<Option<String>>,
    ) -> Result<Result<T, E>, Refusal>
    where
        Fut: Future<Output = Result<T, E>>,
    {
        let sent = match self.bearer().await {
            Err(Refusal::Unavailable) => None,
            bearer => bearer?,
        };
        let result = call(sent.clone()).await;
        let Some(challenge) = unauthorized(&result) else {
            return Ok(result);
        };
        self.challenge(challenge);
        let retry = match sent {
            Some(sent) => self.refresh(Stale::Rejected(sent)).await?,
            None => self.bearer().await?.ok_or(Refusal::Required)?,
        };
        let result = call(Some(retry.clone())).await;
        let Some(challenge) = unauthorized(&result) else {
            return Ok(result);
        };
        self.challenge(challenge);
        let key = self.key.clone();
        self.auth
            .owned(&CancellationToken::new(), move |auth| async move {
                Ok(auth.expire(key, retry).await)
            })
            .await
            .map_err(|_| Refusal::Unavailable)??;
        Err(Refusal::Expired)
    }

    /// A `403` whose challenge says `insufficient_scope` (design §5.7).
    /// Records the challenge for the next sign-in and returns the scopes.
    pub(crate) fn insufficient_scope(&self, header: &str, server: &Url) -> Option<Vec<String>> {
        let params = rmcp::transport::auth::WWWAuthenticateParams::parse(header, server);
        if !params.is_insufficient_scope() {
            return None;
        }
        let scopes: Vec<String> = params
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        self.auth.key(&self.key, |state| {
            state.challenge = Some(header.to_owned());
            for scope in &scopes {
                if !state.missing_scopes.contains(scope) {
                    state.missing_scopes.push(scope.clone());
                }
            }
        });
        Some(scopes)
    }
}
