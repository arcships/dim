//! DimOAuth: the Dim account provider (`dimcode-api-oauth`). It owns account
//! login (browser PKCE and device flow), the `auth.json` account record,
//! token refresh, the relay model catalog and request credential leases, and
//! publishes them as the process's `ManagedProvider`. The account build's
//! `dimcode` executable installs it through [`row`].
mod http;
mod models;
mod store;
mod usage;

pub use http::ClientIds;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::file_storage::FileStorage;
use dim_agent_runtime::{Names, register_part};
use dim_app_server::{
    managed_provider::{
        AccountSnapshot, CredentialLease, LeaseOwner, ManagedCatalog, ManagedError, ManagedFuture,
        ManagedModel, ManagedProvider, ManagedProviderRow, ProviderDescriptor, WebSearchHit,
        WebSearchRequest, WebSearchResponse,
        management::{
            BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
            ManagedBrowserLoginComplete, ManagedLoginAttempt, Usage,
        },
    },
    model_catalog::BundledCatalog,
};
use http::{Config, http_url, identity_headers, rejected, request, text, trimmed};
use reqwest::{Client, StatusCode};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use store::{Grant, Revision, Store};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;
use uuid::Uuid;

pub(crate) const PROVIDER_ID: &str = "dimcode-api-oauth";
const ATTEMPT_MS: u64 = 300_000;
// Leases outlive one model request; longer requests need a longer bound.
const LEASE_MS: u64 = 30 * 60_000;
const MAX_LEASES: usize = 1024;
/// Retry after a transient background refresh failure.
const RENEW_RETRY: Duration = Duration::from_secs(60);
/// Floor between background resolutions, so a grant issued already inside
/// the freshness window cannot spin the loop.
const RENEW_MIN: Duration = Duration::from_secs(1);

type Result<T> = std::result::Result<T, ManagedError>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A persisted grant record. Unknown fields are preserved as written.
#[derive(Clone)]
struct Tokens(Map<String, Value>);
impl Tokens {
    /// A field already validated at the file or token-response boundary.
    fn str(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_str).unwrap_or_default()
    }
}

#[derive(Clone)]
struct Identity {
    issuer: String,
    subject: String,
    email: Option<String>,
    name: Option<String>,
}
impl Identity {
    fn value(&self) -> Value {
        let mut value = json!({"issuer": self.issuer, "subject": self.subject});
        if let Some(email) = &self.email {
            value["email"] = json!(email);
        }
        if let Some(name) = &self.name {
            value["name"] = json!(name);
        }
        value
    }
}

fn same_account(a: &AccountSnapshot, b: &AccountSnapshot) -> bool {
    a.credential_epoch == b.credential_epoch && a.generation == b.generation
}
fn input(provider: &str) -> Result<()> {
    if provider != PROVIDER_ID {
        return Err(ManagedError::InvalidInput);
    }
    Ok(())
}
fn pkce() -> Result<(String, String)> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ManagedError::StorageError)?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}
fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        os => os,
    }
}
fn project_search(body: &http::Body) -> Result<WebSearchResponse> {
    let data = body.get("data").and_then(Value::as_object).unwrap_or(body);
    let list = data
        .get("results")
        .and_then(Value::as_array)
        .ok_or(ManagedError::InvalidResponse)?;
    let mut results = Vec::with_capacity(list.len());
    for item in list {
        let obj = item.as_object().ok_or(ManagedError::InvalidResponse)?;
        let title = text(obj.get("title"), ManagedError::InvalidResponse)?;
        let url = http_url(obj.get("url"), ManagedError::InvalidResponse)?.to_string();
        let snippet = obj
            .get("snippet")
            .and_then(Value::as_str)
            .or_else(|| obj.get("description").and_then(Value::as_str))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let published_at = obj
            .get("published_at")
            .or_else(|| obj.get("publishedAt"))
            .or_else(|| obj.get("age"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        results.push(WebSearchHit {
            title,
            url,
            snippet,
            published_at,
        });
    }
    Ok(WebSearchResponse { results })
}

struct Browser {
    redirect: String,
    state: String,
    verifier: String,
}
/// Attempt secrets are memory-only and cleared on completion, cancellation
/// or expiry. A terminal attempt stays queryable until its deadline.
struct Attempt {
    status: LoginState,
    revision: Revision,
    config: Config,
    expires_at: u64,
    /// Aborts the attempt's network and lock waits.
    cancel: CancellationToken,
    /// Stops the expiry timer once the attempt settles.
    timer: CancellationToken,
    /// The accepted completion or polling work, once started.
    work: Option<TaskTracker>,
    browser: Option<Browser>,
}
impl Attempt {
    fn settle(&mut self, status: LoginState) {
        if matches!(self.status, LoginState::Pending) {
            self.status = status;
        }
    }
    fn aborted(&self) -> LoginState {
        if now() >= self.expires_at {
            LoginState::Expired
        } else {
            LoginState::Cancelled
        }
    }
}

struct HeldLease {
    grant: Grant,
    model: ManagedModel,
    expires_at: u64,
}

struct Inner {
    store: Store,
    client: Client,
    ids: ClientIds,
    bundled: OnceLock<BundledCatalog>,
    /// Lease IDs carry the instance so a reload never resolves an old handle.
    generation: Uuid,
    closed: Mutex<bool>,
    tasks: TaskTracker,
    attempts: Mutex<HashMap<String, Attempt>>,
    leases: Mutex<HashMap<String, HeldLease>>,
    catalog: Mutex<Option<ManagedCatalog>>,
    catalog_gate: tokio::sync::Mutex<()>,
    /// Wakes the background refresher after this process signs an account in.
    signed_in: tokio::sync::Notify,
    /// Ends the background refresher on quiesce.
    stop: CancellationToken,
}
impl LeaseOwner for Inner {
    fn release(&self, lease_id: &str) {
        self.leases.lock().unwrap().remove(lease_id);
    }
}

pub struct DimOAuth(Arc<Inner>);
impl DimOAuth {
    /// Runs an accepted operation as owned work: a caller dropping the
    /// returned future never abandons it, and disposal joins it.
    fn owned<T, F>(&self, operation: impl FnOnce(Arc<Inner>) -> F) -> ManagedFuture<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let task = {
            let closed = self.0.closed.lock().unwrap();
            if *closed {
                return Box::pin(std::future::ready(Err(ManagedError::ServiceReloading)));
            }
            self.0.tasks.spawn(operation(self.0.clone()))
        };
        Box::pin(async move { task.await.unwrap_or(Err(ManagedError::StorageError)) })
    }
    async fn close(&self) {
        self.quiesce().await;
        self.0.tasks.close();
        self.0.tasks.wait().await;
        self.0.leases.lock().unwrap().clear();
        self.0.attempts.lock().unwrap().clear();
        *self.0.catalog.lock().unwrap() = None;
    }
}

impl Inner {
    fn bundled(&self) -> &BundledCatalog {
        self.bundled
            .get_or_init(|| BundledCatalog::load().expect("bundled model catalog is valid"))
    }
    fn forget_catalog(&self) {
        *self.catalog.lock().unwrap() = None;
    }
    /// Renews the grant as it enters the freshness window, so an idle process
    /// keeps its refresh token alive. Request-time resolution stays the
    /// correctness boundary; this only renews ahead of it. A signed-out or
    /// unusable account waits for a login in this process.
    async fn keep_fresh(self: Arc<Self>) {
        loop {
            let wait = match self.store.resolve(None).await {
                Ok(grant) => Some(grant.renews_in().max(RENEW_MIN)),
                Err(ManagedError::ServiceReloading) => return,
                Err(ManagedError::Transient) => Some(RENEW_RETRY),
                Err(_) => None,
            };
            let sleep = async {
                match wait {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = self.stop.cancelled() => return,
                _ = self.signed_in.notified() => {}
                _ = sleep => {}
            }
        }
    }
    fn update(&self, id: &str, change: impl FnOnce(&mut Attempt)) {
        if let Some(attempt) = self.attempts.lock().unwrap().get_mut(id) {
            change(attempt);
        }
    }

    /// Registers an attempt while admission is open and arms its expiry.
    fn register(self: &Arc<Self>, attempt: Attempt) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let (timer, cancel, deadline) = (
            attempt.timer.clone(),
            attempt.cancel.clone(),
            attempt.expires_at,
        );
        {
            let closed = self.closed.lock().unwrap();
            if *closed {
                return Err(ManagedError::ServiceReloading);
            }
            let mut attempts = self.attempts.lock().unwrap();
            let time = now();
            attempts.retain(|_, a| a.expires_at > time || matches!(a.status, LoginState::Pending));
            attempts.insert(id.clone(), attempt);
        }
        let owner = self.clone();
        let key = id.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                _ = timer.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(now()))) => {}
            }
            cancel.cancel();
            let work = owner
                .attempts
                .lock()
                .unwrap()
                .get(&key)
                .and_then(|a| a.work.clone());
            if let Some(work) = work {
                work.wait().await;
            }
            owner.update(&key, |attempt| {
                attempt.settle(LoginState::Expired);
                attempt.browser = None;
            });
        });
        Ok(id)
    }
    /// Starts an attempt's single accepted work.
    fn start_work(&self, id: &str) -> Result<TaskTracker> {
        let mut attempts = self.attempts.lock().unwrap();
        let attempt = attempts.get_mut(id).ok_or(ManagedError::AttemptNotFound)?;
        if !matches!(attempt.status, LoginState::Pending)
            || attempt.work.is_some()
            || attempt.browser.is_none()
            || attempt.expires_at <= now()
        {
            return Err(ManagedError::AttemptClosed);
        }
        let work = TaskTracker::new();
        work.close();
        attempt.work = Some(work.clone());
        Ok(work)
    }
    fn attempt_id(attempt: &ManagedLoginAttempt) -> Result<String> {
        input(&attempt.provider_id)?;
        text(
            Some(&Value::String(attempt.attempt_id.clone())),
            ManagedError::InvalidInput,
        )
    }

    async fn begin_browser(self: Arc<Self>, login: BrowserLoginBegin) -> Result<BrowserLogin> {
        input(&login.provider_id)?;
        let redirect = http_url(
            Some(&Value::String(login.redirect_uri.clone())),
            ManagedError::InvalidCallback,
        )?;
        if redirect.scheme() != "http"
            || !matches!(
                redirect.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            )
            || redirect.path() != "/auth/callback"
            || redirect.query().is_some_and(|q| !q.is_empty())
        {
            return Err(ManagedError::InvalidCallback);
        }
        let revision = self.store.login_revision().await?;
        let config = http::discover(
            &self.client,
            &self.ids,
            &login.directory,
            login.email.as_deref(),
        )
        .await?;
        let (verifier, challenge) = pkce()?;
        let state = Uuid::new_v4().to_string();
        let expires_at = now() + ATTEMPT_MS;
        let mut authorization =
            Url::parse(&config.authorization).map_err(|_| ManagedError::ConfigError)?;
        let mut params = vec![
            ("response_type", "code".to_owned()),
            ("client_id", config.client_id.clone()),
            ("redirect_uri", redirect.to_string()),
            ("scope", config.scope.clone()),
            ("state", state.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
        ];
        if let Some(email) = &login.email {
            params.push(("login_hint", email.trim().to_owned()));
        }
        // Like URLSearchParams.set: ours replace any the endpoint carries.
        let kept: Vec<(String, String)> = authorization
            .query_pairs()
            .filter(|(key, _)| !params.iter().any(|(name, _)| name == key))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        authorization
            .query_pairs_mut()
            .clear()
            .extend_pairs(kept)
            .extend_pairs(params);
        let attempt_id = self.register(Attempt {
            status: LoginState::Pending,
            revision,
            config,
            expires_at,
            cancel: CancellationToken::new(),
            timer: CancellationToken::new(),
            work: None,
            browser: Some(Browser {
                redirect: redirect.to_string(),
                state,
                verifier,
            }),
        })?;
        Ok(BrowserLogin {
            attempt_id,
            authorization_url: authorization.into(),
            expires_at,
        })
    }

    async fn complete_browser(
        self: Arc<Self>,
        login: ManagedBrowserLoginComplete,
    ) -> Result<AccountSnapshot> {
        let id = Self::attempt_id(&ManagedLoginAttempt {
            provider_id: login.provider_id,
            attempt_id: login.attempt_id,
        })?;
        let work = self.start_work(&id)?;
        let (config, revision, cancel, timer, redirect, state, verifier) = {
            let attempts = self.attempts.lock().unwrap();
            let attempt = &attempts[&id];
            let browser = attempt.browser.as_ref().expect("checked by start_work");
            (
                attempt.config.clone(),
                attempt.revision.clone(),
                attempt.cancel.clone(),
                attempt.timer.clone(),
                browser.redirect.clone(),
                browser.state.clone(),
                browser.verifier.clone(),
            )
        };
        let result = work
            .track_future(async {
                let callback = http_url(
                    Some(&Value::String(login.callback_url)),
                    ManagedError::InvalidCallback,
                )?;
                let expected = Url::parse(&redirect).map_err(|_| ManagedError::InvalidCallback)?;
                let values = |name: &str| -> Vec<String> {
                    callback
                        .query_pairs()
                        .filter(|(key, _)| key == name)
                        .map(|(_, value)| value.into_owned())
                        .collect()
                };
                let code = values("code");
                if callback.origin() != expected.origin()
                    || callback.path() != expected.path()
                    || values("state") != [state.clone()]
                    || !values("error").is_empty()
                    || code.len() != 1
                {
                    return Err(ManagedError::InvalidCallback);
                }
                let code = text(
                    Some(&Value::String(code[0].clone())),
                    ManagedError::InvalidCallback,
                )?;
                let (status, body) = request(
                    self.client.post(&config.token).form(&[
                        ("grant_type", "authorization_code"),
                        ("code", code.as_str()),
                        ("redirect_uri", redirect.as_str()),
                        ("client_id", config.client_id.as_str()),
                        ("code_verifier", verifier.as_str()),
                    ]),
                    Some(&cancel),
                )
                .await?;
                if !status.is_success() {
                    return Err(rejected(status, &body));
                }
                let tokens = http::token_from(&config, &body)?;
                let identity = http::userinfo(&self.client, &tokens, Some(&cancel)).await?;
                self.store
                    .commit(&revision, &tokens, &identity, &cancel)
                    .await
            })
            .await;
        self.update(&id, |attempt| {
            match &result {
                Ok(account) => {
                    attempt.status = LoginState::Completed {
                        account: account.clone(),
                    }
                }
                Err(_) if attempt.cancel.is_cancelled() => {
                    let aborted = attempt.aborted();
                    attempt.settle(aborted);
                }
                Err(_) => attempt.settle(LoginState::Failed {
                    error: "oauth_callback_error".into(),
                }),
            }
            attempt.browser = None;
        });
        timer.cancel();
        if result.is_ok() {
            self.forget_catalog();
            self.signed_in.notify_one();
        }
        result
    }

    async fn begin_device(self: Arc<Self>, login: LoginBegin) -> Result<DeviceLogin> {
        input(&login.provider_id)?;
        let revision = self.store.login_revision().await?;
        let config = http::discover(
            &self.client,
            &self.ids,
            &login.directory,
            login.email.as_deref(),
        )
        .await?;
        let device_id = self.store.device_id().await?;
        let (verifier, challenge) = pkce()?;
        let hint = login.email.as_deref().map(str::trim).unwrap_or_default();
        let name = format!("DimCode Rust ({})", platform());
        let (status, body) = request(
            self.client.post(&config.device).form(&[
                ("client_id", config.client_id.as_str()),
                ("scope", config.scope.as_str()),
                ("device_id", device_id.as_str()),
                ("device_name", name.as_str()),
                ("source", "cli"),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("login_hint", hint),
            ]),
            None,
        )
        .await?;
        if !status.is_success() {
            return Err(rejected(status, &body));
        }
        let invalid = ManagedError::InvalidResponse;
        let device_code = text(body.get("device_code"), invalid)?;
        let user_code = text(body.get("user_code"), invalid)?;
        let verification_uri = http_url(body.get("verification_uri"), invalid)?.to_string();
        let verification_uri_complete = match body.get("verification_uri_complete") {
            None | Some(Value::Null) => None,
            value => Some(http_url(value, invalid)?.to_string()),
        };
        let lifetime = http::safe_integer(body.get("expires_in"))
            .filter(|n| *n > 0)
            .ok_or(invalid)?;
        let expires_at = http::expiry(&body)?;
        let interval = match body.get("interval") {
            None => 5,
            value => http::safe_integer(value).ok_or(invalid)?,
        }
        .clamp(1, lifetime.max(1));
        let cancel = CancellationToken::new();
        let timer = CancellationToken::new();
        let attempt_id = self.register(Attempt {
            status: LoginState::Pending,
            revision: revision.clone(),
            config: config.clone(),
            expires_at,
            cancel: cancel.clone(),
            timer: timer.clone(),
            work: None,
            browser: None,
        })?;
        let work = TaskTracker::new();
        work.close();
        self.update(&attempt_id, |attempt| attempt.work = Some(work.clone()));
        let poll = self.clone().poll_device(DevicePoll {
            id: attempt_id.clone(),
            config,
            revision,
            device_code,
            verifier,
            interval,
            expires_at,
            cancel,
            timer,
        });
        self.tasks.spawn(work.track_future(poll));
        Ok(DeviceLogin {
            attempt_id,
            verification_uri,
            verification_uri_complete,
            user_code,
            expires_at,
        })
    }

    async fn poll_device(self: Arc<Self>, poll: DevicePoll) {
        let DevicePoll {
            id,
            config,
            revision,
            device_code,
            verifier,
            mut interval,
            expires_at,
            cancel,
            timer,
        } = poll;
        let result: Result<()> = async {
            loop {
                let wait = (interval * 1000).min(expires_at.saturating_sub(now()).max(1));
                tokio::select! {
                    _ = cancel.cancelled() => return Err(ManagedError::ServiceReloading),
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                }
                if now() >= expires_at {
                    self.update(&id, |a| a.status = LoginState::Expired);
                    return Ok(());
                }
                let response = request(
                    self.client.post(&config.device_token).form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("device_code", device_code.as_str()),
                        ("code_verifier", verifier.as_str()),
                        ("client_id", config.client_id.as_str()),
                    ]),
                    Some(&cancel),
                )
                .await;
                let (status, body) = match response {
                    Ok(response) => response,
                    Err(ManagedError::Transient) if !cancel.is_cancelled() => continue,
                    Err(error) => return Err(error),
                };
                if !status.is_success() {
                    let failed = match body.get("error").and_then(Value::as_str) {
                        Some("authorization_pending") => continue,
                        Some("slow_down") => {
                            interval += 5;
                            continue;
                        }
                        Some("expired_token") => LoginState::Expired,
                        Some("access_denied") => LoginState::Failed {
                            error: "access_denied".into(),
                        },
                        _ => LoginState::Failed {
                            error: "oauth_device_error".into(),
                        },
                    };
                    self.update(&id, |a| a.status = failed);
                    return Ok(());
                }
                let tokens = http::token_from(&config, &body)?;
                let identity = http::userinfo(&self.client, &tokens, Some(&cancel)).await?;
                let account = self
                    .store
                    .commit(&revision, &tokens, &identity, &cancel)
                    .await?;
                self.update(&id, |a| a.status = LoginState::Completed { account });
                self.forget_catalog();
                self.signed_in.notify_one();
                return Ok(());
            }
        }
        .await;
        if result.is_err() {
            self.update(&id, |attempt| {
                let status = if attempt.cancel.is_cancelled() {
                    attempt.aborted()
                } else {
                    LoginState::Failed {
                        error: "oauth_device_error".into(),
                    }
                };
                attempt.settle(status);
            });
        }
        timer.cancel();
    }

    /// Cancels attempts, waits for their accepted work and settles them.
    async fn cancel_attempts(&self, ids: &[String]) {
        let works: Vec<TaskTracker> = {
            let attempts = self.attempts.lock().unwrap();
            ids.iter()
                .filter_map(|id| attempts.get(id))
                .inspect(|attempt| attempt.cancel.cancel())
                .filter_map(|attempt| attempt.work.clone())
                .collect()
        };
        for work in works {
            work.wait().await;
        }
        let mut attempts = self.attempts.lock().unwrap();
        for id in ids {
            let Some(attempt) = attempts.get_mut(id) else {
                continue;
            };
            attempt.settle(LoginState::Cancelled);
            attempt.timer.cancel();
            attempt.browser = None;
        }
    }

    async fn logout(self: Arc<Self>, provider: String) -> Result<AccountSnapshot> {
        input(&provider)?;
        let ids: Vec<String> = self.attempts.lock().unwrap().keys().cloned().collect();
        self.cancel_attempts(&ids).await;
        let (account, tokens) = self.store.logout().await?;
        self.forget_catalog();
        self.leases.lock().unwrap().clear();
        if let Some(tokens) = tokens {
            // Revocation is best effort; the local sign-out already committed.
            let _ = self
                .client
                .post(format!("{}/oauth/revoke", trimmed(tokens.str("issuer"))))
                .form(&[
                    ("token", tokens.str("refresh")),
                    ("client_id", tokens.str("clientId")),
                ])
                .send()
                .await;
        }
        Ok(account)
    }

    /// One authenticated request with a single controlled refresh after 401.
    async fn authorized(
        &self,
        build: impl Fn(&Client, &Tokens) -> reqwest::RequestBuilder,
        cancel: Option<&CancellationToken>,
    ) -> Result<(Grant, StatusCode, http::Body)> {
        let mut current = self.store.resolve(None).await?;
        let expected = current.account.clone();
        let mut retried = false;
        loop {
            if !same_account(&expected, &current.account) {
                return Err(ManagedError::AccountChanged);
            }
            let mut req =
                build(&self.client, &current.tokens).bearer_auth(current.tokens.str("access"));
            for (name, value) in identity_headers() {
                req = req.header(name, value);
            }
            let (status, body) = request(req, cancel).await?;
            if status != StatusCode::UNAUTHORIZED {
                return Ok((current, status, body));
            }
            if retried {
                self.store.finish(&current).await?;
                return Err(ManagedError::RefreshRejected);
            }
            retried = true;
            current = self.store.resolve(Some(&current)).await?;
        }
    }

    async fn catalog(self: Arc<Self>, provider: String) -> Result<ManagedCatalog> {
        input(&provider)?;
        let _gate = self.catalog_gate.lock().await;
        let (grant, status, body) = self
            .authorized(
                |c, t| {
                    c.get(format!(
                        "{}/models?type=dim",
                        trimmed(t.str("relayBaseUrl"))
                    ))
                },
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(rejected(status, &body));
        }
        let models = models::project_models(&body, self.bundled())?;
        let account = self.store.status().await?;
        if !same_account(&grant.account, &account) {
            return Err(ManagedError::AccountChanged);
        }
        let catalog = ManagedCatalog { account, models };
        *self.catalog.lock().unwrap() = Some(catalog.clone());
        Ok(catalog)
    }

    async fn usage(self: Arc<Self>, provider: String) -> Result<Usage> {
        input(&provider)?;
        let (_, status, body) = self
            .authorized(
                |c, t| c.get(format!("{}/api/me/usage", trimmed(t.str("issuer")))),
                None,
            )
            .await?;
        if status == StatusCode::FORBIDDEN {
            return Err(ManagedError::RefreshRejected);
        }
        if !status.is_success() {
            return Err(rejected(status, &body));
        }
        usage::project_usage(&body)
    }

    async fn web_search(
        self: Arc<Self>,
        req: WebSearchRequest,
        cancel: CancellationToken,
    ) -> Result<WebSearchResponse> {
        let query = req.query.trim();
        if query.is_empty() || query.chars().count() > 500 || !(1..=10).contains(&req.count) {
            return Err(ManagedError::InvalidInput);
        }
        let payload = json!({
            "query": query,
            "count": req.count,
            "offset": 0,
        });
        let (_, status, body) = self
            .authorized(
                |c, t| {
                    c.post(format!("{}/web-search", trimmed(t.str("relayBaseUrl"))))
                        .json(&payload)
                },
                Some(&cancel),
            )
            .await?;
        if !status.is_success() {
            return Err(ManagedError::InvalidResponse);
        }
        project_search(&body)
    }

    fn connection(self: &Arc<Self>, lease: HeldLease) -> Result<CredentialLease> {
        let mut leases = self.leases.lock().unwrap();
        let time = now();
        leases.retain(|_, l| l.expires_at > time);
        if leases.len() >= MAX_LEASES {
            return Err(ManagedError::LeaseCapacity);
        }
        let id = format!("{}:{}", self.generation, Uuid::new_v4());
        let owner: Weak<dyn LeaseOwner> = Arc::downgrade(self) as Weak<Inner>;
        let credential = CredentialLease::new(
            lease.grant.account.clone(),
            lease.model.protocol,
            lease.grant.tokens.str("relayBaseUrl").to_owned(),
            identity_headers(),
            lease.grant.tokens.str("access").to_owned(),
            id.clone(),
            owner,
        );
        leases.insert(id, lease);
        Ok(credential)
    }

    async fn credential(
        self: Arc<Self>,
        provider: String,
        model: String,
    ) -> Result<CredentialLease> {
        input(&provider)?;
        text(
            Some(&Value::String(model.clone())),
            ManagedError::InvalidInput,
        )?;
        let current = self.store.resolve(None).await?;
        let cached = self
            .catalog
            .lock()
            .unwrap()
            .clone()
            .filter(|c| same_account(&c.account, &current.account));
        let catalog = match cached {
            Some(catalog) => catalog,
            None => self.clone().catalog(provider).await?,
        };
        let current = self.store.resolve(None).await?;
        let model = catalog
            .models
            .iter()
            .find(|m| m.entry.model_id == model && m.unavailable_reason.is_none())
            .cloned()
            .ok_or(ManagedError::UnknownModel)?;
        if !same_account(&catalog.account, &current.account) {
            return Err(ManagedError::AccountChanged);
        }
        self.connection(HeldLease {
            grant: current,
            model,
            expires_at: now() + LEASE_MS,
        })
    }

    fn held(&self, id: &str) -> Result<(Grant, ManagedModel, u64)> {
        let mut leases = self.leases.lock().unwrap();
        match leases.get(id) {
            Some(lease) if lease.expires_at > now() => {
                Ok((lease.grant.clone(), lease.model.clone(), lease.expires_at))
            }
            _ => {
                leases.remove(id);
                Err(ManagedError::LeaseNotFound)
            }
        }
    }
}

struct DevicePoll {
    id: String,
    config: Config,
    revision: Revision,
    device_code: String,
    verifier: String,
    interval: u64,
    expires_at: u64,
    cancel: CancellationToken,
    timer: CancellationToken,
}

impl ManagedProvider for DimOAuth {
    fn providers(&self) -> Vec<ProviderDescriptor> {
        vec![ProviderDescriptor {
            provider_id: PROVIDER_ID.into(),
            display_name: "Dim OAuth".into(),
            login_methods: vec!["browser".into(), "device".into()],
        }]
    }
    fn status(&self, provider: &str) -> ManagedFuture<AccountSnapshot> {
        let provider = provider.to_owned();
        self.owned(|inner| async move {
            input(&provider)?;
            inner.store.status().await
        })
    }
    fn recover(&self, provider: &str) -> ManagedFuture<AccountSnapshot> {
        let provider = provider.to_owned();
        self.owned(|inner| async move {
            input(&provider)?;
            let account = inner.store.recover().await?;
            inner.forget_catalog();
            inner.signed_in.notify_one();
            Ok(account)
        })
    }
    fn begin_browser(&self, login: BrowserLoginBegin) -> ManagedFuture<BrowserLogin> {
        self.owned(|inner| inner.begin_browser(login))
    }
    fn complete_browser(
        &self,
        login: ManagedBrowserLoginComplete,
    ) -> ManagedFuture<AccountSnapshot> {
        self.owned(|inner| inner.complete_browser(login))
    }
    fn begin_device(&self, login: LoginBegin) -> ManagedFuture<DeviceLogin> {
        self.owned(|inner| inner.begin_device(login))
    }
    fn attempt_status(&self, attempt: &ManagedLoginAttempt) -> Result<LoginState> {
        let id = Inner::attempt_id(attempt)?;
        self.0
            .attempts
            .lock()
            .unwrap()
            .get(&id)
            .map(|attempt| attempt.status.clone())
            .ok_or(ManagedError::AttemptNotFound)
    }
    fn cancel_attempt(&self, attempt: ManagedLoginAttempt) -> ManagedFuture<()> {
        self.owned(|inner| async move {
            let id = Inner::attempt_id(&attempt)?;
            if !inner.attempts.lock().unwrap().contains_key(&id) {
                return Err(ManagedError::AttemptNotFound);
            }
            inner.cancel_attempts(&[id]).await;
            Ok(())
        })
    }
    fn logout(&self, provider: &str) -> ManagedFuture<AccountSnapshot> {
        let provider = provider.to_owned();
        self.owned(|inner| inner.logout(provider))
    }
    fn usage(&self, provider: &str) -> ManagedFuture<Usage> {
        let provider = provider.to_owned();
        self.owned(|inner| inner.usage(provider))
    }
    fn web_search(
        &self,
        request: WebSearchRequest,
        cancel: CancellationToken,
    ) -> ManagedFuture<WebSearchResponse> {
        self.owned(|inner| inner.web_search(request, cancel))
    }
    fn catalog(&self, provider: &str) -> ManagedFuture<ManagedCatalog> {
        let provider = provider.to_owned();
        self.owned(|inner| inner.catalog(provider))
    }
    fn credential(&self, provider: &str, model: &str) -> ManagedFuture<CredentialLease> {
        let (provider, model) = (provider.to_owned(), model.to_owned());
        self.owned(|inner| inner.credential(provider, model))
    }
    fn recover_rejected(&self, lease: &CredentialLease) -> ManagedFuture<CredentialLease> {
        let id = lease.id().to_owned();
        self.owned(|inner| async move {
            let (grant, model, expires_at) = inner.held(&id)?;
            let current = inner.store.resolve(Some(&grant)).await?;
            if !same_account(&grant.account, &current.account) {
                return Err(ManagedError::AccountChanged);
            }
            inner.connection(HeldLease {
                grant: current,
                model,
                expires_at,
            })
        })
    }
    fn finish_rejected(&self, lease: &CredentialLease) -> ManagedFuture<()> {
        let id = lease.id().to_owned();
        self.owned(|inner| async move {
            let (grant, _, _) = inner.held(&id)?;
            inner.store.finish(&grant).await?;
            inner.leases.lock().unwrap().remove(&id);
            Ok(())
        })
    }
    fn quiesce(&self) -> BoxFuture<'static, ()> {
        *self.0.closed.lock().unwrap() = true;
        for attempt in self.0.attempts.lock().unwrap().values() {
            attempt.cancel.cancel();
            attempt.timer.cancel();
        }
        self.0.store.quiesce();
        self.0.stop.cancel();
        Box::pin(std::future::ready(()))
    }
}

/// Provides the Dim account service under the product home's file root.
pub struct DimOAuthPlugin {
    root: usize,
    ids: ClientIds,
    files: Key<FileStorage>,
}
impl DimOAuthPlugin {
    pub fn new(names: &Names<'_>, root: usize, ids: ClientIds) -> rutis::Result<Self> {
        Ok(Self {
            root,
            ids,
            files: names.key()?,
        })
    }
}

/// The App Server row that installs DimOAuth under the product root.
pub fn row(ids: ClientIds) -> ManagedProviderRow {
    ManagedProviderRow(Arc::new(move |builtins, root| {
        let ids = ids.clone();
        register_part(builtins, &dim_app_server::catalog(), move |names| {
            Ok(Box::new(DimOAuthPlugin::new(names, root, ids.clone())?))
        })
        .expect("DimOAuth dependencies")
    }))
}
impl DynPlugin for DimOAuthPlugin {
    fn name(&self) -> &str {
        "DimOAuth"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.files.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            let client = Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| rutis::Error::from(ErrorKind::PluginFailed(e.to_string().into())))?;
            let service = Arc::new(DimOAuth(Arc::new(Inner {
                store: Store::new(files, self.root, client.clone()),
                client,
                ids: self.ids.clone(),
                bundled: OnceLock::new(),
                generation: Uuid::new_v4(),
                closed: Mutex::new(false),
                tasks: TaskTracker::new(),
                attempts: Mutex::new(HashMap::new()),
                leases: Mutex::new(HashMap::new()),
                catalog: Mutex::new(None),
                catalog_gate: tokio::sync::Mutex::new(()),
                signed_in: tokio::sync::Notify::new(),
                stop: CancellationToken::new(),
            })));
            let closing = service.clone();
            ctx.effect(move || async move {
                closing.close().await;
                Ok(())
            })?;
            service.0.tasks.spawn(service.0.clone().keep_fresh());
            ctx.provide_as::<dyn ManagedProvider>(service)?;
            Ok(Effect::Done)
        })
    }
}
