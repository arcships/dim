//! `auth.json` account schema 1: the only persisted source of Dim credentials.
//! Other root fields belong to their owners and are preserved as JSON values.
use crate::{
    Identity, Tokens,
    http::{self, MAX_SAFE_INTEGER, http_url, safe_integer, text},
    now,
};
use dim_agent_capabilities::file_storage::{FileLease, FileStorage, FileStorageError};
use dim_app_server::managed_provider::{AccountSnapshot, AccountView, ManagedError};
use reqwest::Client;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::watch;
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
fn private_permissions() -> Option<std::fs::Permissions> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        None
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

/// Whether `doc` stores a signed-in account, from a validated read: a
/// `tokenDigest` that does not match its grant is not signed in. `None` for a
/// document this build cannot read, which leaves the published value alone.
fn signed_in(doc: &Doc) -> Option<bool> {
    parse(doc, true)
        .ok()
        .map(|parsed| parsed.state.is_some_and(|state| state.identity.is_some()))
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
    lease: FileLease,
    /// The owner's read of whether the file stores a signed-in account, moved
    /// by every observation made under this lease: a write, or a read of the
    /// file. `None` until an observation initializes it.
    account: watch::Sender<Option<bool>>,
}
impl AuthFile {
    async fn read(&mut self) -> Result<(Doc, Option<Vec<u8>>)> {
        let bytes = match self.lease.read(&CancellationToken::new()).await {
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
        self.lease
            .replace(bytes, private_permissions(), &CancellationToken::new())
            .await
            .map_err(file_error)?;
        self.publish(doc);
        Ok(())
    }
    /// Publishes the signed-in state of a document this lease read or wrote.
    /// The publication and the read share the lease, so a write that waits
    /// for it publishes after this observation and one that held it earlier
    /// is what the read saw.
    fn publish(&self, doc: &Doc) {
        if let Some(signed_in) = signed_in(doc) {
            self.account.send_replace(Some(signed_in));
        }
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
        self.lease
            .backup(
                "dim-reset-backup",
                private_permissions(),
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
    files: Arc<FileStorage>,
    root: usize,
    client: Client,
    stopping: CancellationToken,
    /// Whether the account file stores a signed-in account, moved after every
    /// write of it: the provider publishes the App Server's availability from
    /// the account's own transitions. `None` until the store reads the file.
    account: watch::Sender<Option<bool>>,
}
impl Store {
    pub fn new(files: Arc<FileStorage>, root: usize, client: Client) -> Self {
        Self {
            files,
            root,
            client,
            stopping: CancellationToken::new(),
            account: watch::Sender::new(None),
        }
    }
    /// The account file's signed-in state: the initialized observation, then
    /// every write. A subscriber that sees `None` waits for the provider's
    /// first read rather than treating it as signed out.
    pub fn account(&self) -> watch::Receiver<Option<bool>> {
        self.account.subscribe()
    }
    /// Publishes the signed-in state the account file stores. An observation
    /// whose validated state already equals the watch answers from the atomic
    /// read without the lease; the first observation, or one whose state
    /// differs, takes the lease, rereads the file and publishes what that read
    /// holds. The observation is read-only: it never creates, normalizes or
    /// rewrites `auth.json`, so a shared file another writer populated stays as
    /// it is. A missing file is a home that never signed in and publishes
    /// signed out; a record this build cannot validate leaves the watch as it
    /// was. `cancel` ends a wait for the lease, so a caller that gives up does
    /// not leave a retry behind.
    pub async fn observe_account(&self, cancel: Option<&CancellationToken>) -> Result<()> {
        if let Some(current) = self.observed_current().await
            && *self.account.borrow() == Some(current)
        {
            return Ok(());
        }
        let mut file = self.open(cancel).await?;
        let (doc, _) = file.read().await?;
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(ManagedError::ServiceReloading);
        }
        file.publish(&doc);
        Ok(())
    }
    /// Stops lock acquisition. A transaction that holds the lock still
    /// persists its result.
    pub fn quiesce(&self) {
        self.stopping.cancel();
    }
    async fn lease(&self, name: &str, cancel: Option<&CancellationToken>) -> Result<FileLease> {
        let path = self.files.path(self.root, name).map_err(file_error)?;
        let cancel = cancel.cloned().unwrap_or_default();
        loop {
            if self.stopping.is_cancelled() || cancel.is_cancelled() {
                return Err(ManagedError::ServiceReloading);
            }
            let attempt = tokio::select! {
                _ = self.stopping.cancelled() => return Err(ManagedError::ServiceReloading),
                _ = cancel.cancelled() => return Err(ManagedError::ServiceReloading),
                attempt = self.files.lock(&path, &self.stopping) => attempt,
            };
            match attempt {
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
    async fn open(&self, cancel: Option<&CancellationToken>) -> Result<AuthFile> {
        Ok(AuthFile {
            lease: self.lease(AUTH_FILE, cancel).await?,
            account: self.account.clone(),
        })
    }

    /// The signed-in state the account file stores, read without its lease.
    /// A missing file is a home that never signed in; an unreadable or invalid
    /// record returns `None`, which sends the observation down the leased path;
    /// that path publishes only a validated read.
    async fn observed_current(&self) -> Option<bool> {
        let path = self.files.path(self.root, AUTH_FILE).ok()?;
        let bytes = match self.files.read(&path, &CancellationToken::new()).await {
            Ok(bytes) => bytes,
            Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Some(false);
            }
            Err(_) => return None,
        };
        let doc = document(&bytes).ok()?;
        signed_in(&doc)
    }

    /// Reads the account file without its lease. Writes are atomic replaces,
    /// so a fresh grant needs neither the cross-process marker nor its fsync;
    /// any error sends the caller down the leased path.
    async fn read_current(&self) -> Option<Doc> {
        let path = self.files.path(self.root, AUTH_FILE).ok()?;
        let bytes = self
            .files
            .read(&path, &CancellationToken::new())
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

    /// The `account.status` read. A read that changes the published
    /// observation takes the account lease, so a newer write or logout
    /// publishes after it and an uninitialized watch is recovered from the
    /// store it reads.
    pub async fn status(&self) -> Result<AccountSnapshot> {
        if let Some(status) = self.current_status().await {
            let status = status?;
            if *self.account.borrow() == Some(status.account.is_some()) {
                return Ok(status);
            }
        }
        let mut file = self.open(None).await?;
        self.status_in(&mut file).await
    }

    /// The stored account for internal readers. A record that needs no rewrite
    /// answers from the lease-free read, so a fresh credential lookup is not
    /// held up by another process's account lease; a rewrite still takes the
    /// lease and publishes that observation.
    pub(super) async fn read_status(&self) -> Result<AccountSnapshot> {
        if let Some(status) = self.current_status().await {
            return status;
        }
        let mut file = self.open(None).await?;
        self.status_in(&mut file).await
    }

    /// Reads the account under `file`'s lease, normalizing or writing a
    /// signed-out record, and publishes the observation before the lease is
    /// released.
    async fn status_in(&self, file: &mut AuthFile) -> Result<AccountSnapshot> {
        let (mut doc, _) = file.read().await?;
        file.normalize(&mut doc).await?;
        // No Dim record at all, even beside other owners' records, is signed
        // out; Dim tokens without their state still need recovery.
        if !doc.contains_key(STATE) && !doc.contains_key(TOKENS) {
            doc.insert(STATE.into(), signed_out(None, false)?);
            file.write(&doc).await?;
        }
        let status = snapshot(&doc)?;
        file.publish(&doc);
        Ok(status)
    }

    pub async fn login_revision(&self) -> Result<Revision> {
        let mut file = self.open(None).await?;
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
        let mut file = self.open(Some(cancel)).await?;
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
    }

    pub async fn device_id(&self) -> Result<String> {
        let mut lease = self.lease(DEVICE_FILE, None).await?;
        let current = match lease.read(&CancellationToken::new()).await {
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
        lease
            .replace(
                format!("{id}\n").into_bytes(),
                private_permissions(),
                &CancellationToken::new(),
            )
            .await
            .map_err(file_error)?;
        Ok(id)
    }

    /// The current grant, refreshed within the freshness window, or after a
    /// rejection of `rejected` when no newer grant replaced it. A record that
    /// needs normalization is rewritten first. A rotated grant is persisted
    /// before the account lock is released.
    pub async fn resolve(&self, rejected: Option<&Grant>) -> Result<Grant> {
        self.resolve_in(rejected, true).await
    }

    /// Renewal for the background task: it persists a rotated grant and signs
    /// out a rejected one, but never normalizes the record it reads.
    pub(super) async fn renew(&self) -> Result<Grant> {
        self.resolve_in(None, false).await
    }

    async fn resolve_in(&self, rejected: Option<&Grant>, normalize: bool) -> Result<Grant> {
        if let Some(grant) = self.current(rejected).await? {
            return Ok(grant);
        }
        let mut file = self.open(None).await?;
        let (mut doc, _) = file.read().await?;
        if normalize {
            file.normalize(&mut doc).await?;
        }
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
    }

    /// Signs out only when `rejected` still owns the account.
    pub async fn finish(&self, rejected: &Grant) -> Result<()> {
        let mut file = self.open(None).await?;
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
    }

    pub async fn logout(&self) -> Result<(AccountSnapshot, Option<Tokens>)> {
        let mut file = self.open(None).await?;
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
    }

    /// Explicit identity recovery for a legacy grant or a digest mismatch.
    /// Never applies to unknown or corrupt schema.
    pub async fn recover(&self) -> Result<AccountSnapshot> {
        let (tokens, raw) = {
            let mut file = self.open(None).await?;
            let (mut doc, _) = file.read().await?;
            let mut legacy = false;
            match parse(&doc, true) {
                Ok(_) if doc.contains_key(STATE) => return snapshot(&doc),
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
                return snapshot(&doc);
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
            (tokens, file.read().await?.1)
        };
        let identity = http::userinfo(&self.client, &tokens, None).await?;
        let mut file = self.open(None).await?;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_capabilities::file_storage::FileStoragePlugin;
    use rutis::Ctx;
    use rutis::host::CtxHost;
    use std::path::Path;

    async fn storage(home: &Path) -> (Ctx, Arc<FileStorage>) {
        let root = Ctx::root().unwrap();
        let view = root.plugin(FileStoragePlugin::new(vec![home.to_path_buf()]));
        view.running(Duration::from_secs(10)).await.unwrap();
        let files = root.get::<FileStorage>().unwrap();
        (root, files)
    }

    fn signed_in_document() -> Value {
        let identity = Identity {
            issuer: "https://accounts.example".into(),
            subject: "subject".into(),
            email: None,
            name: None,
        };
        let tokens = validate_tokens(&json!({
            "type": "oauth",
            "access": "access",
            "refresh": "refresh",
            "clientId": "client",
            "issuer": identity.issuer,
            "tokenEndpoint": "https://accounts.example/token",
            "relayBaseUrl": "https://relay.example",
            "expires": now() + 3_600_000,
        }))
        .unwrap();
        let state = authenticated(Uuid::new_v4().to_string(), 1, 1, &identity, &tokens).unwrap();
        json!({"nextApiOauth": Value::Object(tokens.0), "dimAccountState": state})
    }

    /// A store whose initializer never read the file still publishes its first
    /// successful account read, so a failed assembly read does not hide a
    /// signed-in account until the next write.
    #[tokio::test]
    async fn a_status_read_publishes_an_uninitialized_account_watch() {
        let home = std::env::temp_dir().join(format!("do-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&signed_in_document()).unwrap(),
        )
        .unwrap();
        let (root, files) = storage(&home).await;
        let store = Store::new(files, 0, Client::new());
        assert_eq!(*store.account.borrow(), None);
        assert!(store.status().await.unwrap().account.is_some());
        assert_eq!(
            *store.account.borrow(),
            Some(true),
            "the status read must publish the account it read"
        );
        root.shutdown().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    /// The publication decision validates the record: a `tokenDigest` that
    /// does not match its grant is not a signed-in account.
    #[test]
    fn a_digest_mismatch_is_not_published_as_signed_in() {
        let mut doc = signed_in_document();
        assert_eq!(signed_in(doc.as_object().unwrap()), Some(true));
        doc["dimAccountState"]["tokenDigest"] = json!("0".repeat(64));
        assert_eq!(
            signed_in(doc.as_object().unwrap()),
            None,
            "a record whose digest does not match its grant was published signed in"
        );
    }
}
