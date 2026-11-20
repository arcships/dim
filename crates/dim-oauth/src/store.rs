//! `auth.json` account schema 1: the only persisted source of Dim credentials.
//! Other root fields belong to their owners and are preserved as JSON values.
use crate::{
    Identity, Tokens,
    http::{self, MAX_SAFE_INTEGER, http_url, safe_integer, text},
    now,
};
use dim_agent_capabilities::file_storage::{FileStorageError, FsHost, FsPermissions};
use dim_app_server::managed_provider::{AccountSnapshot, AccountView, ManagedError};
use reqwest::Client;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{io, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const AUTH_FILE: &str = "auth.json";
const DEVICE_FILE: &str = "device-id";
const TOKENS: &str = "nextApiOauth";
const STATE: &str = "dimAccountState";
const FRESH_MS: u64 = 30_000;

type Result<T> = std::result::Result<T, ManagedError>;
type Doc = Map<String, Value>;

/// The persisted account state, as validated at the file boundary.
#[derive(Clone)]
struct State {
    epoch: String,
    generation: u64,
    revision: u64,
    identity: Option<Identity>,
}
struct Parsed {
    tokens: Option<Tokens>,
    state: Option<State>,
}

/// What a login compares before committing: the durable account revision, or
/// the token digest of a home without account state.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Revision {
    Managed { epoch: String, revision: u64 },
    Digest(String),
}

/// A persisted grant and the account it belongs to.
#[derive(Clone)]
pub(super) struct Grant {
    pub tokens: Tokens,
    pub account: AccountSnapshot,
    pub revision: u64,
}
impl Grant {
    /// Time until the grant enters the freshness window, when [`Store::resolve`]
    /// would rotate it.
    pub fn renews_in(&self) -> Duration {
        let expires = safe_integer(self.tokens.0.get("expires")).unwrap_or(0);
        Duration::from_millis(expires.saturating_sub(now() + FRESH_MS))
    }
}

fn file_error(error: FileStorageError) -> ManagedError {
    match error {
        FileStorageError::Closed | FileStorageError::Cancelled => ManagedError::ServiceReloading,
        FileStorageError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            ManagedError::AccountResetBlocked
        }
        FileStorageError::Io(error) if error.kind() == io::ErrorKind::InvalidData => {
            ManagedError::InvalidStore
        }
        _ => ManagedError::StorageError,
    }
}
/// Canonical JSON (RFC 8785) digest, byte-equal to the TypeScript account
/// files: UTF-16 key order and ECMAScript number encoding.
pub(super) fn digest(value: &Value) -> Result<String> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| ManagedError::InvalidStore)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn next(value: u64) -> Result<u64> {
    if value >= MAX_SAFE_INTEGER {
        return Err(ManagedError::InvalidStore);
    }
    Ok(value + 1)
}

pub(super) fn validate_tokens(value: &Value) -> Result<Tokens> {
    let store = ManagedError::InvalidStore;
    let token = value.as_object().ok_or(store)?;
    if token.get("type") != Some(&json!("oauth")) {
        return Err(store);
    }
    for key in ["access", "refresh", "clientId"] {
        text(token.get(key), store)?;
    }
    for key in ["issuer", "tokenEndpoint", "relayBaseUrl"] {
        http_url(token.get(key), store)?;
    }
    for key in [
        "authorizationEndpoint",
        "deviceAuthorizationEndpoint",
        "deviceTokenEndpoint",
    ] {
        if !matches!(token.get(key), None | Some(Value::Null)) {
            http_url(token.get(key), store)?;
        }
    }
    if !matches!(
        token.get("scope"),
        None | Some(Value::Null | Value::String(_))
    ) {
        return Err(store);
    }
    if !safe_integer(token.get("expires")).is_some_and(|n| n > 0) {
        return Err(store);
    }
    Ok(Tokens(token.clone()))
}

fn parse(doc: &Doc, verify_digest: bool) -> Result<Parsed> {
    let store = ManagedError::InvalidStore;
    let raw = doc.get(STATE);
    if let Some(raw) = raw {
        let schema = raw.as_object().ok_or(store)?.get("schemaVersion");
        if schema.is_some_and(|v| v != &json!(1)) {
            return Err(ManagedError::UnsupportedSchema);
        }
    }
    let tokens = doc.get(TOKENS).map(validate_tokens).transpose()?;
    let Some(raw) = raw else {
        return Ok(Parsed {
            tokens,
            state: None,
        });
    };
    let state = raw.as_object().ok_or(store)?;
    let epoch = state
        .get("credentialEpoch")
        .and_then(Value::as_str)
        .filter(|epoch| epoch.len() == 36 && Uuid::parse_str(epoch).is_ok())
        .ok_or(store)?;
    let generation = safe_integer(state.get("generation"))
        .filter(|n| *n >= 1)
        .ok_or(store)?;
    let revision = safe_integer(state.get("credentialRevision"))
        .filter(|n| *n >= 1)
        .ok_or(store)?;
    if state.get("schemaVersion") != Some(&json!(1)) {
        return Err(store);
    }
    let identity = match state.get("state").and_then(Value::as_str) {
        Some("signed_out") => {
            if tokens.is_some()
                || state.contains_key("identity")
                || state.contains_key("tokenDigest")
            {
                return Err(store);
            }
            None
        }
        Some("authenticated") => {
            let identity = state
                .get("identity")
                .and_then(Value::as_object)
                .ok_or(store)?;
            let tokens = tokens.as_ref().ok_or(store)?;
            let token_digest = state
                .get("tokenDigest")
                .and_then(Value::as_str)
                .filter(|d| {
                    d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                })
                .ok_or(store)?;
            if identity.get("issuer").and_then(Value::as_str) != Some(tokens.str("issuer")) {
                return Err(store);
            }
            let subject = text(identity.get("subject"), store)?;
            let issuer =
                http_url(identity.get("issuer"), store).map(|_| tokens.str("issuer").to_owned())?;
            let label = |key: &str| match identity.get(key) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(store),
            };
            let identity = Identity {
                issuer,
                subject,
                email: label("email")?,
                name: label("name")?,
            };
            if verify_digest && token_digest != digest(&Value::Object(tokens.0.clone()))? {
                return Err(ManagedError::AccountRecoveryRequired);
            }
            Some(identity)
        }
        _ => return Err(store),
    };
    Ok(Parsed {
        tokens,
        state: Some(State {
            epoch: epoch.to_owned(),
            generation,
            revision,
            identity,
        }),
    })
}

fn signed_out(old: Option<&State>, changed: bool) -> Result<Value> {
    let advance = |value: u64| if changed { next(value) } else { Ok(value) };
    Ok(json!({
        "schemaVersion": 1,
        "credentialEpoch": old.map_or_else(|| Uuid::new_v4().to_string(), |s| s.epoch.clone()),
        "generation": old.map_or(Ok(1), |s| advance(s.generation))?,
        "credentialRevision": old.map_or(Ok(1), |s| advance(s.revision))?,
        "state": "signed_out",
    }))
}
fn authenticated(
    epoch: String,
    generation: u64,
    revision: u64,
    identity: &Identity,
    tokens: &Tokens,
) -> Result<Value> {
    Ok(json!({
        "schemaVersion": 1,
        "credentialEpoch": epoch,
        "generation": generation,
        "credentialRevision": revision,
        "state": "authenticated",
        "identity": identity.value(),
        "tokenDigest": digest(&Value::Object(tokens.0.clone()))?,
    }))
}

fn snapshot(doc: &Doc) -> Result<AccountSnapshot> {
    let Parsed { tokens, state } = parse(doc, true)?;
    let state = state.ok_or(ManagedError::AccountRecoveryRequired)?;
    Ok(AccountSnapshot {
        credential_epoch: state.epoch,
        generation: state.generation,
        account: state.identity.map(|identity| AccountView {
            issuer: identity.issuer,
            subject: identity.subject,
            email: identity.email,
            name: identity.name,
            granted_scopes: tokens
                .as_ref()
                .and_then(|t| t.0.get("scope"))
                .and_then(Value::as_str)
                .map(|scope| scope.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
        }),
    })
}
fn revision(doc: &Doc) -> Result<Revision> {
    Ok(match parse(doc, true)?.state {
        None => Revision::Digest(digest(doc.get(TOKENS).unwrap_or(&Value::Null))?),
        Some(state) => Revision::Managed {
            epoch: state.epoch,
            revision: state.revision,
        },
    })
}
fn same_grant(state: &State, grant: &Grant) -> bool {
    state.epoch == grant.account.credential_epoch && state.revision == grant.revision
}
fn recoverable(error: ManagedError) -> bool {
    matches!(
        error,
        ManagedError::InvalidStore | ManagedError::AccountRecoveryRequired
    )
}
/// Reads a file that the TypeScript plugin may have written as a PluginFiles
/// record (`{"version":1,"text":...}`); the next write stores plain JSON.
fn unwrap_record(bytes: &[u8]) -> Option<Vec<u8>> {
    let Ok(Value::Object(mut record)) = serde_json::from_slice::<Value>(bytes) else {
        return None;
    };
    if record.get("version") != Some(&json!(1)) {
        return None;
    }
    let Some(Value::String(text)) = record.remove("text") else {
        return None;
    };
    record.remove("version");
    if record.is_empty() {
        return Some(text.into_bytes());
    }
    // Other owners wrote their fields beside the record; keep both.
    let Ok(Value::Object(mut inner)) = serde_json::from_str::<Value>(&text) else {
        return None;
    };
    for (key, value) in record {
        inner.entry(key).or_insert(value);
    }
    serde_json::to_vec(&inner).ok()
}

/// Decodes a stored account file, accepting the PluginFiles record the
/// TypeScript plugin wrote.
fn document(bytes: &[u8]) -> Result<Doc> {
    let text = unwrap_record(bytes).unwrap_or_else(|| bytes.to_vec());
    match serde_json::from_slice::<Value>(&text) {
        Ok(Value::Object(doc)) => Ok(doc),
        _ => Err(ManagedError::InvalidStore),
    }
}

/// One locked `auth.json`.
struct AuthFile {
    files: Arc<FsHost>,
    path: std::path::PathBuf,
    lease: Uuid,
}
impl AuthFile {
    async fn read(&mut self) -> Result<(Doc, Option<Vec<u8>>)> {
        let bytes = match self
            .files
            .read_bytes(
                &self.path,
                usize::MAX,
                Some(self.lease),
                &CancellationToken::new(),
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Map::new(), None));
            }
            Err(error) => return Err(file_error(error)),
        };
        Ok((document(&bytes)?, Some(bytes)))
    }
    async fn write(&mut self, doc: &Doc) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(doc).map_err(|_| ManagedError::InvalidStore)?;
        bytes.push(b'\n');
        self.files
            .replace(
                &self.path,
                bytes,
                FsPermissions::Private,
                Some(self.lease),
                &CancellationToken::new(),
            )
            .await
            .map_err(file_error)
    }
    /// Drops a stored client secret and moves a legacy `dimcode.cn` grant to
    /// `dimagent.cn`, advancing the revision when the grant changes.
    async fn normalize(&mut self, doc: &mut Doc) -> Result<()> {
        let Parsed { tokens, state } = parse(doc, true)?;
        let Some(Tokens(mut tokens)) = tokens else {
            return Ok(());
        };
        let before = digest(&Value::Object(tokens.clone()))?;
        tokens.remove("clientSecret");
        let legacy =
            |value: &str| value == "https://dimcode.cn" || value.starts_with("https://dimcode.cn/");
        if state.is_none()
            && tokens.get("issuer").and_then(Value::as_str) == Some("https://dimcode.cn")
            && tokens
                .get("relayBaseUrl")
                .and_then(Value::as_str)
                .is_some_and(legacy)
        {
            for key in [
                "issuer",
                "authorizationEndpoint",
                "tokenEndpoint",
                "deviceAuthorizationEndpoint",
                "deviceTokenEndpoint",
                "relayBaseUrl",
            ] {
                if let Some(Value::String(value)) = tokens.get_mut(key)
                    && legacy(value)
                {
                    *value = format!(
                        "https://dimagent.cn{}",
                        &value["https://dimcode.cn".len()..]
                    );
                }
            }
        }
        let tokens = Value::Object(tokens);
        let after = digest(&tokens)?;
        if after == before {
            return Ok(());
        }
        if let Some(state) = state.filter(|s| s.identity.is_some()) {
            let fields = doc[STATE].as_object_mut().expect("validated account state");
            fields.insert("credentialRevision".into(), next(state.revision)?.into());
            fields.insert("tokenDigest".into(), after.into());
        }
        doc.insert(TOKENS.into(), tokens);
        self.write(doc).await
    }
    /// Backs up the complete original file, then replaces only Dim fields with
    /// a new signed-out state. An existing backup blocks another reset.
    async fn reset(&mut self, doc: &mut Doc) -> Result<()> {
        if let Some(state) = doc.get(STATE)
            && state
                .as_object()
                .ok_or(ManagedError::UnsupportedSchema)?
                .get("schemaVersion")
                != Some(&json!(1))
        {
            return Err(ManagedError::UnsupportedSchema);
        }
        self.files
            .backup(
                self.lease,
                "dim-reset-backup",
                FsPermissions::Private,
                &CancellationToken::new(),
            )
            .await
            .map_err(file_error)?;
        doc.remove(TOKENS);
        doc.insert(STATE.into(), signed_out(None, false)?);
        self.write(doc).await
    }
}

pub(super) struct Store {
    files: Arc<FsHost>,
    home: std::path::PathBuf,
    client: Client,
    stopping: CancellationToken,
}
impl Store {
    pub fn new(files: Arc<FsHost>, home: std::path::PathBuf, client: Client) -> Self {
        Self {
            files,
            home,
            client,
            stopping: CancellationToken::new(),
        }
    }
    /// Stops lock acquisition. A transaction that holds the lock still
    /// persists its result.
    pub fn quiesce(&self) {
        self.stopping.cancel();
    }
    async fn lease(
        &self,
        path: &std::path::Path,
        cancel: Option<&CancellationToken>,
    ) -> Result<Uuid> {
        let cancel = cancel.cloned().unwrap_or_default();
        loop {
            if self.stopping.is_cancelled() || cancel.is_cancelled() {
                return Err(ManagedError::ServiceReloading);
            }
            match self.files.lock(None, path, &self.stopping).await {
                Ok(lease) => return Ok(lease),
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = self.stopping.cancelled() => {}
                    _ = cancel.cancelled() => {}
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                },
                Err(error) => return Err(file_error(error)),
            }
        }
    }
    async fn with_auth<T, F>(
        &self,
        cancel: Option<&CancellationToken>,
        work: impl FnOnce(AuthFile) -> F,
    ) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        let path = self.home.join(AUTH_FILE);
        let lease = self.lease(&path, cancel).await?;
        let result = work(AuthFile {
            files: self.files.clone(),
            path,
            lease,
        })
        .await;
        let released = self.files.unlock(lease).await.map_err(file_error);
        let value = result?;
        released?;
        Ok(value)
    }

    /// Reads the account file without its lease. Writes are atomic replaces,
    /// so a fresh grant needs neither the cross-process marker nor its fsync;
    /// any error sends the caller down the leased path.
    async fn read_current(&self) -> Option<Doc> {
        let path = self.home.join(AUTH_FILE);
        let bytes = self
            .files
            .read_bytes(&path, usize::MAX, None, &CancellationToken::new())
            .await
            .ok()?;
        document(&bytes).ok()
    }

    /// The account snapshot when the record needs no rewrite, read without
    /// the lease. An unknown account, a legacy grant or a dropped client
    /// secret still takes the leased path that normalizes it.
    async fn current_status(&self) -> Option<Result<AccountSnapshot>> {
        let doc = self.read_current().await?;
        let Ok(Parsed { tokens, state }) = parse(&doc, true) else {
            return None;
        };
        state.as_ref()?;
        if tokens.is_some_and(|tokens| tokens.0.contains_key("clientSecret")) {
            return None;
        }
        Some(snapshot(&doc))
    }

    /// The grant already stored when it is usable without a token request.
    /// `None` takes the lease: the file is absent or unreadable, the grant is
    /// inside the freshness window, the rejected grant is still current, or
    /// normalization would rewrite the record first.
    async fn current(&self, rejected: Option<&Grant>) -> Result<Option<Grant>> {
        let Some(doc) = self.read_current().await else {
            return Ok(None);
        };
        let Ok(Parsed {
            tokens: Some(tokens),
            state: Some(state),
        }) = parse(&doc, true)
        else {
            return Ok(None);
        };
        if tokens.0.contains_key("clientSecret") {
            return Ok(None);
        }
        let fresh = safe_integer(tokens.0.get("expires")).unwrap_or(0) > now() + FRESH_MS;
        let reuse = match rejected {
            Some(rejected) => !same_grant(&state, rejected),
            None => fresh,
        };
        if !reuse {
            return Ok(None);
        }
        Ok(Some(Grant {
            tokens,
            account: snapshot(&doc)?,
            revision: state.revision,
        }))
    }

    pub async fn status(&self) -> Result<AccountSnapshot> {
        if let Some(status) = self.current_status().await {
            return status;
        }
        self.with_auth(None, |mut file| async move {
            let (mut doc, _) = file.read().await?;
            file.normalize(&mut doc).await?;
            // No Dim record at all, even beside other owners' records, is signed
            // out; Dim tokens without their state still need recovery.
            if !doc.contains_key(STATE) && !doc.contains_key(TOKENS) {
                doc.insert(STATE.into(), signed_out(None, false)?);
                file.write(&doc).await?;
            }
            snapshot(&doc)
        })
        .await
    }

    pub async fn login_revision(&self) -> Result<Revision> {
        self.with_auth(None, |mut file| async move {
            let (mut doc, raw) = file.read().await?;
            if raw.is_none() {
                doc.insert(STATE.into(), signed_out(None, false)?);
                file.write(&doc).await?;
            }
            match revision(&doc) {
                Err(error) if recoverable(error) && raw.is_some() => {
                    file.reset(&mut doc).await?;
                    revision(&doc)
                }
                result => result,
            }
        })
        .await
    }

    pub async fn commit(
        &self,
        expected: &Revision,
        tokens: &Tokens,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<AccountSnapshot> {
        let mut tokens = validate_tokens(&Value::Object(tokens.0.clone()))?;
        if tokens.str("issuer") != identity.issuer || identity.subject.trim().is_empty() {
            return Err(ManagedError::IdentityRejected);
        }
        tokens.0.remove("clientSecret");
        self.with_auth(Some(cancel), |mut file| async move {
            let (mut doc, _) = file.read().await?;
            let current = revision(&doc)?;
            if cancel.is_cancelled() {
                return Err(ManagedError::ServiceReloading);
            }
            if *expected != current {
                return Err(ManagedError::AccountChanged);
            }
            let old = doc.get(STATE).and_then(Value::as_object);
            let field = |key: &str| safe_integer(old.and_then(|o| o.get(key))).unwrap_or(0);
            let epoch = old
                .and_then(|o| o.get("credentialEpoch"))
                .and_then(Value::as_str)
                .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
            let state = authenticated(
                epoch,
                next(field("generation"))?,
                next(field("credentialRevision"))?,
                identity,
                &tokens,
            )?;
            doc.insert(TOKENS.into(), Value::Object(tokens.0));
            doc.insert(STATE.into(), state);
            // The admitted compare-and-swap owns persistence even when its caller cancels.
            file.write(&doc).await?;
            snapshot(&doc)
        })
        .await
    }

    pub async fn device_id(&self) -> Result<String> {
        let path = self.home.join(DEVICE_FILE);
        let lease = self.lease(&path, None).await?;
        let result: Result<String> = async {
            let current = match self
                .files
                .read_bytes(&path, usize::MAX, Some(lease), &CancellationToken::new())
                .await
            {
                Ok(bytes) => String::from_utf8(unwrap_record(&bytes).unwrap_or(bytes))
                    .map_err(|_| ManagedError::InvalidStore)?,
                Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    String::new()
                }
                Err(error) => return Err(file_error(error)),
            };
            if !current.trim().is_empty() {
                return Ok(current.trim().to_owned());
            }
            let id = format!("dim_{}", Uuid::new_v4());
            self.files
                .replace(
                    &path,
                    format!("{id}\n").into_bytes(),
                    FsPermissions::Private,
                    Some(lease),
                    &CancellationToken::new(),
                )
                .await
                .map_err(file_error)?;
            Ok(id)
        }
        .await;
        let released = self.files.unlock(lease).await.map_err(file_error);
        let value = result?;
        released?;
        Ok(value)
    }

    /// The current grant, refreshed within the freshness window, or after a
    /// rejection of `rejected` when no newer grant replaced it. A rotated grant
    /// is persisted before the account lock is released.
    pub async fn resolve(&self, rejected: Option<&Grant>) -> Result<Grant> {
        if let Some(grant) = self.current(rejected).await? {
            return Ok(grant);
        }
        self.with_auth(None, |mut file| async move {
            let (mut doc, _) = file.read().await?;
            file.normalize(&mut doc).await?;
            let Parsed { tokens, state } = parse(&doc, true)?;
            // A home that never signed in needs login, not recovery.
            if tokens.is_none() && state.is_none() {
                return Err(ManagedError::TokenMissing);
            }
            let current = snapshot(&doc)?;
            let (Some(tokens), Some(state)) = (tokens, state) else {
                return Err(ManagedError::TokenMissing);
            };
            let fresh = safe_integer(tokens.0.get("expires")).unwrap_or(0) > now() + FRESH_MS;
            let reuse = match rejected {
                Some(rejected) => !same_grant(&state, rejected),
                None => fresh,
            };
            if reuse {
                return Ok(Grant {
                    tokens,
                    account: current,
                    revision: state.revision,
                });
            }
            let rotated = match http::refresh(&self.client, &tokens).await {
                Ok(rotated) => rotated,
                Err(ManagedError::RefreshRejected) => {
                    doc.remove(TOKENS);
                    doc.insert(STATE.into(), signed_out(Some(&state), true)?);
                    file.write(&doc).await?;
                    return Err(ManagedError::RefreshRejected);
                }
                Err(error) => return Err(error),
            };
            let mut rotated = validate_tokens(&Value::Object(rotated.0))?;
            rotated.0.remove("clientSecret");
            if state
                .identity
                .as_ref()
                .is_none_or(|identity| identity.issuer != rotated.str("issuer"))
            {
                return Err(ManagedError::InvalidResponse);
            }
            let revision = next(state.revision)?;
            let fields = doc[STATE].as_object_mut().expect("validated account state");
            fields.insert("credentialRevision".into(), revision.into());
            fields.insert(
                "tokenDigest".into(),
                digest(&Value::Object(rotated.0.clone()))?.into(),
            );
            doc.insert(TOKENS.into(), Value::Object(rotated.0.clone()));
            file.write(&doc).await?;
            Ok(Grant {
                tokens: rotated,
                account: snapshot(&doc)?,
                revision,
            })
        })
        .await
    }

    /// Signs out only when `rejected` still owns the account.
    pub async fn finish(&self, rejected: &Grant) -> Result<()> {
        self.with_auth(None, |mut file| async move {
            let (mut doc, _) = file.read().await?;
            let Parsed { tokens, state } = parse(&doc, true)?;
            if let (Some(_), Some(state)) = (tokens, state)
                && same_grant(&state, rejected)
            {
                doc.remove(TOKENS);
                doc.insert(STATE.into(), signed_out(Some(&state), true)?);
                file.write(&doc).await?;
            }
            Ok(())
        })
        .await
    }

    pub async fn logout(&self) -> Result<(AccountSnapshot, Option<Tokens>)> {
        self.with_auth(None, |mut file| async move {
            let (mut doc, raw) = file.read().await?;
            let Parsed { tokens, state } = match parse(&doc, true) {
                Ok(parsed) => parsed,
                Err(error) if recoverable(error) && raw.is_some() => {
                    file.reset(&mut doc).await?;
                    return Ok((snapshot(&doc)?, None));
                }
                Err(error) => return Err(error),
            };
            doc.remove(TOKENS);
            doc.insert(STATE.into(), signed_out(state.as_ref(), true)?);
            file.write(&doc).await?;
            Ok((snapshot(&doc)?, tokens))
        })
        .await
    }

    /// Explicit identity recovery for a legacy grant or a digest mismatch.
    /// Never applies to unknown or corrupt schema.
    pub async fn recover(&self) -> Result<AccountSnapshot> {
        use std::ops::ControlFlow;
        let recovery = self
            .with_auth(None, |mut file| async move {
                let (mut doc, _) = file.read().await?;
                let mut legacy = false;
                match parse(&doc, true) {
                    Ok(_) if doc.contains_key(STATE) => {
                        return snapshot(&doc).map(ControlFlow::Break);
                    }
                    Ok(_) => legacy = true,
                    Err(ManagedError::AccountRecoveryRequired) => {}
                    Err(error) => return Err(error),
                }
                if legacy {
                    file.normalize(&mut doc).await?;
                }
                let Parsed { tokens, state } = parse(&doc, false)?;
                let Some(mut tokens) = tokens else {
                    doc.insert(STATE.into(), signed_out(None, false)?);
                    file.write(&doc).await?;
                    return snapshot(&doc).map(ControlFlow::Break);
                };
                if safe_integer(tokens.0.get("expires")).unwrap_or(0) <= now() + FRESH_MS {
                    tokens = match http::refresh(&self.client, &tokens).await {
                        Ok(rotated) => rotated,
                        Err(ManagedError::RefreshRejected) => {
                            doc.remove(TOKENS);
                            doc.insert(STATE.into(), signed_out(state.as_ref(), true)?);
                            file.write(&doc).await?;
                            return Err(ManagedError::RefreshRejected);
                        }
                        Err(error) => return Err(error),
                    };
                    // Persist the rotated grant and drop unverified identity
                    // before going online for userinfo.
                    doc.insert(TOKENS.into(), Value::Object(tokens.0.clone()));
                    doc.remove(STATE);
                    file.write(&doc).await?;
                }
                Ok(ControlFlow::Continue((tokens, file.read().await?.1)))
            })
            .await?;
        let (tokens, raw) = match recovery {
            ControlFlow::Break(snapshot) => return Ok(snapshot),
            ControlFlow::Continue(pending) => pending,
        };
        let identity = http::userinfo(&self.client, &tokens, None).await?;
        self.with_auth(None, |mut file| async move {
            let (mut doc, current) = file.read().await?;
            if current != raw {
                return Err(ManagedError::AccountChanged);
            }
            let mut tokens = tokens;
            tokens.0.remove("clientSecret");
            let state = authenticated(Uuid::new_v4().to_string(), 1, 1, &identity, &tokens)?;
            doc.insert(TOKENS.into(), Value::Object(tokens.0));
            doc.insert(STATE.into(), state);
            file.write(&doc).await?;
            snapshot(&doc)
        })
        .await
    }
}
