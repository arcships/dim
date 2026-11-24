mod openid;
mod wire;

use super::{
    AccountSnapshot, AccountView, CredentialLease, LeaseOwner, LoginState, ManagedCatalog,
    ManagedError, ManagedModel, ProviderDescriptor, management::BrowserLogin,
};
use crate::model_catalog::{ModelEntry, ModelStatus};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::{
    file_storage::{FileLease, FilePath, FileStorage, FileStorageError},
    host::ModelProtocol,
};
use openid::{Discovery, TokenReply, has_plan, receive};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::OnceCell;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;
use uuid::Uuid;
pub(super) use wire::provide;

pub(crate) const ID: &str = "openai-chatgpt";
pub(crate) const NAMESPACE: &str = "account.chatgpt";
const REGISTER: &str = "dynamic_agent_client";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);
const PROFILE_LIMIT: usize = 32;

type Result<T> = std::result::Result<T, ManagedError>;

pub(super) fn descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: ID.into(),
        display_name: "OpenAI ChatGPT".into(),
        login_methods: vec!["browser".into()],
        default_directory: None,
    }
}

pub(super) struct ChatGpt {
    files: Arc<FileStorage>,
    path: FilePath,
    issuer: Url,
    api: Url,
    client: OnceCell<Client>,
    discovery: OnceCell<Discovery>,
    attempts: Mutex<HashMap<Uuid, Attempt>>,
    leases: Arc<Leases>,
    stop: CancellationToken,
    admission: Mutex<()>,
    tasks: TaskTracker,
}

#[derive(Default)]
struct Leases(Mutex<HashSet<String>>);

impl LeaseOwner for Leases {
    fn release(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}

struct Attempt {
    expires_at: u64,
    state: LoginState,
    pending: Option<Pending>,
    cancel: CancellationToken,
}

struct Pending {
    revision: (Uuid, u64),
    profile_id: Option<Uuid>,
    migrate_legacy: bool,
    client_id: Option<String>,
    redirect: Url,
    state: String,
    nonce: String,
    verifier: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u8,
    host_id: Uuid,
    generation: u64,
    active: Option<Uuid>,
    pending_client_id: Option<String>,
    #[serde(default)]
    legacy_codex_hidden: bool,
    profiles: Vec<Profile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: Uuid,
    client_id: String,
    subject: String,
    email: Option<String>,
    name: Option<String>,
    tokens: Option<Grant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    access: String,
    refresh: String,
    expires_at: u64,
    earliest_refresh_at: Option<u64>,
    scopes: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProfileInfo {
    id: Uuid,
    label: String,
    active: bool,
    signed_in: bool,
}

#[derive(Deserialize)]
struct ModelList {
    models: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct RemoteModel {
    slug: Option<String>,
    display_name: Option<String>,
    visibility: String,
}

impl Store {
    fn new() -> Self {
        Self {
            version: 1,
            host_id: Uuid::new_v4(),
            generation: 0,
            active: None,
            pending_client_id: None,
            legacy_codex_hidden: false,
            profiles: Vec::new(),
        }
    }

    fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| Some(p.id) == self.active)
    }

    fn revision(&self) -> (Uuid, u64) {
        (self.host_id, self.generation)
    }

    fn change_account(&mut self) -> Result<()> {
        self.generation = self
            .generation
            .checked_add(1)
            .filter(|generation| *generation <= crate::model_catalog::MAX_MODEL_TOKENS)
            .ok_or(ManagedError::InvalidStore)?;
        Ok(())
    }

    fn snapshot(&self, issuer: &Url) -> AccountSnapshot {
        AccountSnapshot {
            credential_epoch: self.host_id.to_string(),
            generation: self.generation,
            account: self.active().and_then(|p| {
                let token = p.tokens.as_ref()?;
                Some(AccountView {
                    issuer: issuer.origin().ascii_serialization(),
                    subject: p.subject.clone(),
                    email: p.email.clone(),
                    name: p.name.clone(),
                    granted_scopes: token.scopes.clone(),
                })
            }),
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum BeginReply {
    Browser(BrowserLogin),
    Migrated { migrated: bool },
}

impl ChatGpt {
    pub(super) fn new(files: Arc<FileStorage>, root: usize) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            path: files.path(root, "chatgpt-account.json").map_err(storage)?,
            files,
            issuer: Url::parse("https://auth.openai.com").unwrap(),
            api: Url::parse("https://api.openai.com/v1/").unwrap(),
            client: OnceCell::new(),
            discovery: OnceCell::new(),
            attempts: Mutex::new(HashMap::new()),
            leases: Arc::new(Leases::default()),
            stop: CancellationToken::new(),
            admission: Mutex::new(()),
            tasks: TaskTracker::new(),
        }))
    }

    pub(super) async fn close(&self) {
        {
            let _admission = self.admission.lock().unwrap();
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }

    async fn lock(&self, cancel: &CancellationToken) -> Result<FileLease> {
        loop {
            if self.stop.is_cancelled() || cancel.is_cancelled() {
                return Err(ManagedError::Cancelled);
            }
            match self.files.lock(&self.path, cancel).await {
                Ok(lease) => return Ok(lease),
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = self.stop.cancelled() => return Err(ManagedError::Cancelled),
                    _ = cancel.cancelled() => return Err(ManagedError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                },
                Err(error) => return Err(storage(error)),
            }
        }
    }

    async fn read(&self, cancel: &CancellationToken) -> Result<Option<Store>> {
        decode_store(self.files.read(&self.path, cancel).await)
    }

    pub(super) async fn status(&self, cancel: &CancellationToken) -> Result<AccountSnapshot> {
        Ok(self.read(cancel).await?.map_or(
            AccountSnapshot {
                credential_epoch: "unregistered".into(),
                generation: 0,
                account: None,
            },
            |state| state.snapshot(&self.issuer),
        ))
    }

    pub(super) async fn profiles(&self, cancel: &CancellationToken) -> Result<Vec<ProfileInfo>> {
        let Some(state) = self.read(cancel).await? else {
            return Ok(Vec::new());
        };
        Ok(state
            .profiles
            .into_iter()
            .enumerate()
            .map(|(index, p)| ProfileInfo {
                id: p.id,
                label: match p.email.or(p.name) {
                    Some(label) => format!("{label} ({})", index + 1),
                    None => format!("Connection {}", index + 1),
                },
                active: Some(p.id) == state.active,
                signed_in: p.tokens.is_some(),
            })
            .collect())
    }

    pub(super) async fn select(
        &self,
        profile_id: Uuid,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot> {
        let mut lease = self.lock(cancel).await?;
        let mut state =
            decode_store(lease.read(cancel).await)?.ok_or(ManagedError::InvalidInput)?;
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or(ManagedError::InvalidInput)?;
        if profile.tokens.is_none() {
            return Err(ManagedError::TokenMissing);
        }
        if state.active != Some(profile_id) {
            state.active = Some(profile_id);
            state.change_account()?;
            write(&mut lease, &state).await?;
        }
        Ok(state.snapshot(&self.issuer))
    }

    pub(super) async fn legacy_codex_hidden(&self, cancel: &CancellationToken) -> Result<bool> {
        Ok(self
            .read(cancel)
            .await?
            .is_some_and(|store| store.legacy_codex_hidden))
    }

    pub(super) async fn begin(
        &self,
        redirect_uri: &str,
        profile_id: Option<Uuid>,
        migrate_legacy: bool,
        cancel: &CancellationToken,
    ) -> Result<BeginReply> {
        let redirect = Url::parse(redirect_uri).map_err(|_| ManagedError::InvalidInput)?;
        if redirect.scheme() != "http"
            || redirect.host_str() != Some("127.0.0.1")
            || redirect.port().is_none()
            || redirect.path() != "/auth/callback"
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err(ManagedError::InvalidInput);
        }
        let mut lease = self.lock(cancel).await?;
        let mut store = match decode_store(lease.read(cancel).await)? {
            Some(store) => store,
            None => {
                let store = Store::new();
                write(&mut lease, &store).await?;
                store
            }
        };
        if migrate_legacy
            && let Some(id) = profile_id.or(store.active)
            && store
                .profiles
                .iter()
                .any(|p| p.id == id && p.tokens.is_some())
        {
            if store.active != Some(id) || !store.legacy_codex_hidden {
                if store.active != Some(id) {
                    store.active = Some(id);
                    store.change_account()?;
                }
                store.legacy_codex_hidden = true;
                write(&mut lease, &store).await?;
            }
            return Ok(BeginReply::Migrated { migrated: true });
        }
        let client_id = if let Some(id) = profile_id {
            Some(
                store
                    .profiles
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or(ManagedError::InvalidInput)?
                    .client_id
                    .clone(),
            )
        } else {
            if store.profiles.len() == PROFILE_LIMIT {
                return Err(ManagedError::InvalidInput);
            }
            store.pending_client_id.clone()
        };
        let attempt_id = Uuid::new_v4();
        let expires_at = now().saturating_add(SIGN_IN_TIMEOUT.as_secs());
        let pending = Pending {
            revision: store.revision(),
            profile_id,
            migrate_legacy,
            client_id,
            redirect,
            state: random()?,
            nonce: random()?,
            verifier: random()?,
        };
        let mut url = self.issuer.join("/api/accounts/authorize").unwrap();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()));
        let resource = self.resource();
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs([
                (
                    "client_id",
                    pending.client_id.as_deref().unwrap_or(REGISTER),
                ),
                ("ext_agent_host_id", &format!("urn:uuid:{}", store.host_id)),
                ("response_type", "code"),
                ("redirect_uri", pending.redirect.as_str()),
                ("scope", SCOPES),
                ("resource", resource.as_str()),
                ("state", &pending.state),
                ("nonce", &pending.nonce),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
            ]);
            if pending.client_id.is_none() {
                query.append_pair("agent_name_hint", "Dim");
            }
        }
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|_, a| a.expires_at > now());
        if attempts.len() >= 32 {
            return Err(ManagedError::InvalidInput);
        }
        attempts.insert(
            attempt_id,
            Attempt {
                expires_at,
                state: LoginState::Pending,
                pending: Some(pending),
                cancel: self.stop.child_token(),
            },
        );
        Ok(BeginReply::Browser(BrowserLogin {
            attempt_id: attempt_id.to_string(),
            authorization_url: url.into(),
            expires_at: expires_at * 1000,
        }))
    }

    pub(super) fn attempt_status(&self, id: Uuid) -> Result<LoginState> {
        let attempts = self.attempts.lock().unwrap();
        let attempt = attempts.get(&id).ok_or(ManagedError::AttemptNotFound)?;
        if attempt.expires_at <= now() && matches!(attempt.state, LoginState::Pending) {
            return Ok(LoginState::Expired);
        }
        Ok(attempt.state.clone())
    }

    pub(super) fn cancel(&self, id: Uuid) -> Result<()> {
        let mut attempts = self.attempts.lock().unwrap();
        let attempt = attempts.get_mut(&id).ok_or(ManagedError::AttemptNotFound)?;
        if matches!(attempt.state, LoginState::Pending) {
            attempt.cancel.cancel();
            attempt.pending = None;
            attempt.state = LoginState::Cancelled;
        }
        Ok(())
    }

    pub(super) async fn complete(&self, id: Uuid, callback: &str) -> Result<AccountSnapshot> {
        let callback = Url::parse(callback).map_err(|_| ManagedError::InvalidCallback)?;
        let (pending, cancel, code, issued) = {
            let mut attempts = self.attempts.lock().unwrap();
            let attempt = attempts.get_mut(&id).ok_or(ManagedError::AttemptNotFound)?;
            let pending = attempt
                .pending
                .as_ref()
                .filter(|_| attempt.expires_at > now())
                .ok_or(ManagedError::AttemptClosed)?;
            let (code, issued) = match callback_values(pending, callback) {
                Ok(values) => values,
                Err(ManagedError::PlanNotGranted) => {
                    attempt.pending = None;
                    attempt.state = LoginState::Failed {
                        error: super::ManagedProviderError::from(ManagedError::PlanNotGranted)
                            .message
                            .into(),
                    };
                    return Err(ManagedError::PlanNotGranted);
                }
                Err(error) => return Err(error),
            };
            (
                attempt.pending.take().unwrap(),
                attempt.cancel.clone(),
                code,
                issued,
            )
        };
        let result = self.exchange(pending, &code, issued, &cancel).await;
        if let Some(attempt) = self.attempts.lock().unwrap().get_mut(&id)
            && !matches!(attempt.state, LoginState::Cancelled)
        {
            attempt.state = match &result {
                Ok(account) => LoginState::Completed {
                    account: account.clone(),
                },
                Err(error) => LoginState::Failed {
                    error: super::ManagedProviderError::from(*error).message.into(),
                },
            };
        }
        result
    }

    async fn exchange(
        &self,
        pending: Pending,
        code: &str,
        client_id: String,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot> {
        let mut lease = self.lock(cancel).await?;
        let mut state =
            decode_store(lease.read(cancel).await)?.ok_or(ManagedError::InvalidStore)?;
        if state.revision() != pending.revision {
            return Err(ManagedError::AccountChanged);
        }
        if pending.profile_id.is_none() {
            match state.pending_client_id.as_ref() {
                Some(other) if other != &client_id => return Err(ManagedError::AccountChanged),
                Some(_) => {}
                None => {
                    state.pending_client_id = Some(client_id.clone());
                    write(&mut lease, &state).await?;
                }
            }
        }
        let resource = self.resource();
        let client = self.client().await?;
        let response: TokenReply = receive(
            client.post(self.token_url()).form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", code),
                ("code_verifier", &pending.verifier),
                ("redirect_uri", pending.redirect.as_str()),
                ("resource", &resource),
            ]),
            64 * 1024,
        )
        .await?;
        let id_token = response
            .id_token
            .as_deref()
            .ok_or(ManagedError::IdentityRejected)?;
        let identity = self.identity(id_token, &client_id, &pending.nonce).await?;
        let grant = response.grant()?;
        if cancel.is_cancelled() {
            return Err(ManagedError::Cancelled);
        }
        let id = match pending.profile_id {
            Some(id) => {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == id)
                    .ok_or(ManagedError::AccountChanged)?;
                if profile.subject != identity.sub || profile.client_id != client_id {
                    return Err(ManagedError::IdentityRejected);
                }
                profile.email = identity.email;
                profile.name = identity.name;
                profile.tokens = Some(grant);
                id
            }
            None => {
                if state.profiles.iter().any(|p| p.client_id == client_id) {
                    return Err(ManagedError::AccountChanged);
                }
                let id = Uuid::new_v4();
                state.profiles.push(Profile {
                    id,
                    client_id,
                    subject: identity.sub,
                    email: identity.email,
                    name: identity.name,
                    tokens: Some(grant),
                });
                state.pending_client_id = None;
                id
            }
        };
        state.active = Some(id);
        state.legacy_codex_hidden |= pending.migrate_legacy;
        state.change_account()?;
        write(&mut lease, &state).await?;
        Ok(state.snapshot(&self.issuer))
    }

    pub(super) async fn logout(&self, cancel: &CancellationToken) -> Result<bool> {
        let mut lease = self.lock(cancel).await?;
        let Some(mut state) = decode_store(lease.read(cancel).await)? else {
            return Ok(true);
        };
        let active = state.active;
        let Some(profile) = state.profiles.iter_mut().find(|p| Some(p.id) == active) else {
            return Ok(true);
        };
        let Some(grant) = profile.tokens.take() else {
            return Ok(true);
        };
        let confirmed = self
            .revoke(&profile.client_id, &grant.refresh)
            .await
            .is_ok();
        state.change_account()?;
        write(&mut lease, &state).await?;
        Ok(confirmed)
    }

    async fn revoke(&self, client_id: &str, refresh: &str) -> Result<()> {
        let client = self.client().await?;
        let endpoint = &self.discovery().await?.revocation_endpoint;
        for delay in [None, Some(Duration::from_millis(250))] {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            match client
                .post(endpoint.clone())
                .form(&[
                    ("token", refresh),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", client_id),
                ])
                .send()
                .await
            {
                Ok(response) if response.status() == StatusCode::OK => return Ok(()),
                Ok(response) if !response.status().is_server_error() => break,
                _ => {}
            }
        }
        Err(ManagedError::Transient)
    }

    pub(super) async fn catalog(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<ManagedCatalog> {
        let (account, access) = self.token(None, cancel).await?;
        let list: ModelList = receive(
            self.client()
                .await?
                .get(self.api.join("models").unwrap())
                .bearer_auth(access),
            8 * 1024 * 1024,
        )
        .await?;
        let mut ids = HashSet::new();
        let mut models = Vec::new();
        for model in list
            .models
            .into_iter()
            .filter(|model| model.visibility == "list")
        {
            let model_id = model
                .slug
                .filter(|id| !id.trim().is_empty())
                .ok_or(ManagedError::InvalidResponse)?;
            let display_name = model
                .display_name
                .filter(|name| !name.trim().is_empty())
                .ok_or(ManagedError::InvalidResponse)?;
            if !ids.insert(model_id.clone()) {
                return Err(ManagedError::InvalidResponse);
            }
            models.push(ManagedModel {
                entry: ModelEntry {
                    model_id,
                    display_name,
                    status: ModelStatus::Active,
                    capabilities: Default::default(),
                    model_guide_prompt: None,
                    metadata: None,
                },
                protocol: ModelProtocol::OpenaiResponses,
                unavailable_reason: Some("limits_undeclared".into()),
                zdr: false,
            });
        }
        Ok(ManagedCatalog { account, models })
    }

    pub(super) async fn credential(
        self: &Arc<Self>,
        rejected: Option<(&AccountSnapshot, &str)>,
        cancel: &CancellationToken,
    ) -> Result<CredentialLease> {
        let (account, bearer) = self.token(rejected, cancel).await?;
        let id = Uuid::new_v4().to_string();
        {
            let mut leases = self.leases.0.lock().unwrap();
            if leases.len() >= 1024 {
                return Err(ManagedError::LeaseCapacity);
            }
            leases.insert(id.clone());
        }
        let owner: Arc<dyn LeaseOwner> = self.leases.clone();
        Ok(CredentialLease::new(
            account,
            ModelProtocol::OpenaiResponses,
            self.resource(),
            HashMap::new(),
            bearer,
            id,
            Arc::downgrade(&owner),
        ))
    }

    async fn token(
        self: &Arc<Self>,
        rejected: Option<(&AccountSnapshot, &str)>,
        cancel: &CancellationToken,
    ) -> Result<(AccountSnapshot, String)> {
        let state = self.read(cancel).await?.ok_or(ManagedError::TokenMissing)?;
        let account = state.snapshot(&self.issuer);
        let grant = state
            .active()
            .and_then(|p| p.tokens.as_ref())
            .ok_or(ManagedError::TokenMissing)?;
        if let Some((previous, _)) = rejected
            && previous != &account
        {
            return Err(ManagedError::AccountChanged);
        }
        if rejected.is_none()
            && (grant.expires_at > now().saturating_add(60)
                || (grant.expires_at > now()
                    && grant.earliest_refresh_at.is_some_and(|time| time > now())))
        {
            return Ok((account, grant.access.clone()));
        }
        let revision = state.revision();
        let previous = rejected.map(|(_, bearer)| bearer.to_owned());
        let owner = self.clone();
        let cancel = cancel.clone();
        let task = {
            let _admission = self.admission.lock().unwrap();
            if self.stop.is_cancelled() || cancel.is_cancelled() {
                return Err(ManagedError::Cancelled);
            }
            self.tasks
                .spawn(async move { owner.refresh(revision, previous.as_deref(), &cancel).await })
        };
        task.await.map_err(|_| ManagedError::Transient)?
    }

    async fn refresh(
        &self,
        revision: (Uuid, u64),
        rejected: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<(AccountSnapshot, String)> {
        let mut lease = self.lock(cancel).await?;
        let mut state =
            decode_store(lease.read(cancel).await)?.ok_or(ManagedError::TokenMissing)?;
        if state.revision() != revision {
            return Err(ManagedError::AccountChanged);
        }
        let active = state.active;
        let profile = state
            .profiles
            .iter_mut()
            .find(|p| Some(p.id) == active)
            .ok_or(ManagedError::TokenMissing)?;
        let grant = profile.tokens.as_ref().ok_or(ManagedError::TokenMissing)?;
        let now = now();
        let refresh_due = match rejected {
            Some(bearer) => grant.access == bearer || grant.expires_at <= now,
            None => grant.expires_at <= now.saturating_add(60),
        };
        if !refresh_due {
            let access = grant.access.clone();
            return Ok((state.snapshot(&self.issuer), access));
        }
        if grant.earliest_refresh_at.is_some_and(|time| time > now) {
            return if rejected.is_none() && grant.expires_at > now {
                let access = grant.access.clone();
                Ok((state.snapshot(&self.issuer), access))
            } else {
                Err(ManagedError::Transient)
            };
        }
        let client = self.client().await?;
        let resource = self.resource();
        let result: Result<TokenReply> = receive(
            client.post(self.token_url()).form(&[
                ("grant_type", "refresh_token"),
                ("client_id", profile.client_id.as_str()),
                ("refresh_token", grant.refresh.as_str()),
                ("resource", &resource),
            ]),
            64 * 1024,
        )
        .await;
        match result.and_then(TokenReply::grant) {
            Ok(grant) => {
                let access = grant.access.clone();
                profile.tokens = Some(grant);
                write(&mut lease, &state).await?;
                Ok((state.snapshot(&self.issuer), access))
            }
            Err(ManagedError::RefreshRejected | ManagedError::PlanNotGranted) => {
                profile.tokens = None;
                state.change_account()?;
                write(&mut lease, &state).await?;
                Err(ManagedError::RefreshRejected)
            }
            Err(error) => Err(error),
        }
    }

    fn resource(&self) -> String {
        self.api.as_str().trim_end_matches('/').to_owned()
    }

    fn token_url(&self) -> Url {
        self.issuer.join("/api/accounts/oauth/token").unwrap()
    }
}

fn callback_values(pending: &Pending, mut callback: Url) -> Result<(String, String)> {
    let mut fields = HashMap::new();
    for (key, value) in callback.query_pairs() {
        if matches!(key.as_ref(), "state" | "code" | "error" | "client_id")
            && fields
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(ManagedError::InvalidCallback);
        }
    }
    callback.set_query(None);
    if callback != pending.redirect || fields.get("state") != Some(&pending.state) {
        return Err(ManagedError::InvalidCallback);
    }
    if let Some(error) = fields.get("error") {
        return Err(if error == "access_denied" {
            ManagedError::PlanNotGranted
        } else {
            ManagedError::InvalidCallback
        });
    }
    let returned = fields.remove("client_id");
    let client_id = match (&pending.client_id, returned) {
        (Some(saved), Some(returned)) if *saved == returned => returned,
        (Some(saved), None) => saved.clone(),
        (None, Some(returned)) if valid_client_id(&returned) => returned,
        _ => return Err(ManagedError::InvalidCallback),
    };
    let code = fields
        .remove("code")
        .filter(|code| !code.is_empty())
        .ok_or(ManagedError::InvalidCallback)?;
    Ok((code, client_id))
}

fn valid_client_id(id: &str) -> bool {
    id != REGISTER
        && !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn decode_store(value: std::result::Result<Vec<u8>, FileStorageError>) -> Result<Option<Store>> {
    let bytes = match value {
        Ok(bytes) => bytes,
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(storage(error)),
    };
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| ManagedError::InvalidStore)?;
    if store.version != 1 {
        return Err(ManagedError::UnsupportedSchema);
    }
    let mut ids = HashSet::new();
    let mut clients = HashSet::new();
    if store.profiles.len() > PROFILE_LIMIT
        || store.generation > crate::model_catalog::MAX_MODEL_TOKENS
        || store.active.is_some() && store.active().is_none()
        || store
            .pending_client_id
            .as_deref()
            .is_some_and(|id| !valid_client_id(id))
        || !store.profiles.iter().all(|p| {
            ids.insert(p.id)
                && clients.insert(&p.client_id)
                && valid_client_id(&p.client_id)
                && !p.subject.trim().is_empty()
                && p.tokens.as_ref().is_none_or(|t| {
                    !t.access.is_empty() && !t.refresh.is_empty() && has_plan(&t.scopes)
                })
        })
    {
        return Err(ManagedError::InvalidStore);
    }
    Ok(Some(store))
}

async fn write(lease: &mut FileLease, store: &Store) -> Result<()> {
    let bytes = serde_json::to_vec(store).map_err(|_| ManagedError::InvalidStore)?;
    #[cfg(unix)]
    let permissions = Some(std::os::unix::fs::PermissionsExt::from_mode(0o600));
    #[cfg(not(unix))]
    let permissions = None;
    lease
        .replace(bytes, permissions, &CancellationToken::new())
        .await
        .map_err(storage)
}

fn storage(error: FileStorageError) -> ManagedError {
    match error {
        FileStorageError::Cancelled => ManagedError::Cancelled,
        _ => ManagedError::StorageError,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ManagedError::Transient)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests;
