//! `mcp-oauth.json` under the home root: Mcp's own credential store. One
//! writer (Mcp), atomic 0600 replace through fsHost, versioned.
use super::{CredentialKey, McpAuthError};
use dim_agent_capabilities::file_storage::{FileStorageError, FsHost, FsPermissions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

const FILE: &str = "mcp-oauth.json";
const VERSION: u64 = 1;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Document {
    pub credentials: Vec<Record>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
impl Document {
    pub fn get(&self, key: &CredentialKey) -> Option<&Record> {
        self.credentials.iter().find(|record| record.is(key))
    }
    pub fn get_mut(&mut self, key: &CredentialKey) -> Option<&mut Record> {
        self.credentials.iter_mut().find(|record| record.is(key))
    }
    pub fn put(&mut self, record: Record) {
        self.remove(&record.key());
        self.credentials.push(record);
    }
    pub fn remove(&mut self, key: &CredentialKey) -> Option<Record> {
        let index = self.credentials.iter().position(|record| record.is(key))?;
        Some(self.credentials.remove(index))
    }
}

/// One credential key's record. A record without `tokens` is expired: the
/// user must sign in again. Secrets never leave this module except as
/// request material; nothing here implements `Debug`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Record {
    pub server_id: String,
    pub url: String,
    /// The `resource` sent on the authorization request, repeated on every
    /// token request.
    pub resource: String,
    pub issuer: String,
    pub token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_endpoint: Option<String>,
    /// Cleared when the authorization server answers `invalid_client`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
    pub requested_scopes: Vec<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
impl Record {
    pub fn new(key: &CredentialKey, binding: Binding, client: Client, tokens: Tokens) -> Self {
        Self {
            server_id: key.server_id.clone(),
            url: key.url.clone(),
            resource: binding.resource,
            issuer: binding.issuer,
            token_endpoint: binding.token_endpoint,
            revocation_endpoint: binding.revocation_endpoint,
            client: Some(client),
            tokens: Some(tokens),
            requested_scopes: binding.requested_scopes,
            extra: Map::new(),
        }
    }
    fn is(&self, key: &CredentialKey) -> bool {
        self.server_id == key.server_id && self.url == key.url
    }
    fn key(&self) -> CredentialKey {
        CredentialKey {
            server_id: self.server_id.clone(),
            url: self.url.clone(),
        }
    }
}

/// What an authorization binds a record to, besides its client and tokens.
#[derive(Clone)]
pub(super) struct Binding {
    pub resource: String,
    pub issuer: String,
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub requested_scopes: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Client {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub registration: Registration,
    pub redirect_uri: String,
    pub auth_method: AuthMethod,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Registration {
    Preregistered,
    Dynamic,
}

/// Client authentication at the token and revocation endpoints.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AuthMethod {
    None,
    ClientSecretBasic,
    ClientSecretPost,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix ms, computed from `expires_in` on receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
}

pub(super) struct Store {
    files: Arc<FsHost>,
    path: std::path::PathBuf,
    /// In front of the FileStorage lock, which admits or fails at once.
    local: Mutex<()>,
    #[cfg(feature = "testing")]
    home: std::path::PathBuf,
}

/// The store held for one read-modify-write, including any token request
/// made in between.
pub(super) struct Locked<'a> {
    _local: MutexGuard<'a, ()>,
    lease: uuid::Uuid,
    store: &'a Store,
    pub document: Document,
}
impl Locked<'_> {
    pub async fn unlock(self) -> Result<(), McpAuthError> {
        self.store
            .files
            .unlock(self.lease)
            .await
            .map_err(|_| McpAuthError::Unavailable)
    }

    /// Commits once started: the replace ignores cancellation. A replace in
    /// place whose sync failed counts as written: readers already see it,
    /// and a crash that loses it ends like a kill (design §7).
    pub async fn write(&mut self) -> Result<(), McpAuthError> {
        let mut value =
            serde_json::to_value(&self.document).map_err(|_| McpAuthError::Unavailable)?;
        value
            .as_object_mut()
            .ok_or(McpAuthError::Unavailable)?
            .insert("version".into(), VERSION.into());
        let mut bytes = serde_json::to_vec_pretty(&value).map_err(|_| McpAuthError::Unavailable)?;
        bytes.push(b'\n');
        #[cfg(feature = "testing")]
        testing::pause(&self.store.home).await;
        match self
            .store
            .files
            .replace(
                &self.store.path,
                bytes,
                FsPermissions::Private,
                Some(self.lease),
                &CancellationToken::new(),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(FileStorageError::Unsynced(error)) => {
                tracing::warn!(target: "mcp.auth", %error, "credential store written, durability unconfirmed");
                Ok(())
            }
            Err(_) => Err(McpAuthError::Unavailable),
        }
    }
}

impl Store {
    pub fn new(files: Arc<FsHost>, home: std::path::PathBuf) -> Self {
        Self {
            path: home.join(FILE),
            #[cfg(feature = "testing")]
            home,
            files,
            local: Mutex::new(()),
        }
    }

    /// A lock-free read: writes are atomic replaces.
    pub async fn read(&self, cancel: &CancellationToken) -> Result<Document, McpAuthError> {
        decode(
            self.files
                .read_bytes(&self.path, usize::MAX, None, cancel)
                .await,
        )
    }

    /// Another daemon sharing the home may hold the file lock; retry until
    /// it is released or the caller stops.
    pub async fn lock(&self, cancel: &CancellationToken) -> Result<Locked<'_>, McpAuthError> {
        let local = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(McpAuthError::Cancelled),
            guard = self.local.lock() => guard,
        };
        let lease = loop {
            match self.files.lock(None, &self.path, cancel).await {
                Ok(lease) => break lease,
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = cancel.cancelled() => return Err(McpAuthError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                },
                Err(FileStorageError::Cancelled) => return Err(McpAuthError::Cancelled),
                Err(_) => return Err(McpAuthError::Unavailable),
            }
        };
        let document = match decode(
            self.files
                .read_bytes(&self.path, usize::MAX, Some(lease), cancel)
                .await,
        ) {
            Ok(document) => document,
            Err(error) => {
                let _ = self.files.unlock(lease).await;
                return Err(error);
            }
        };
        Ok(Locked {
            _local: local,
            lease,
            document,
            store: self,
        })
    }
}

/// An unknown version or an unreadable document is never overwritten.
fn decode(read: Result<Vec<u8>, FileStorageError>) -> Result<Document, McpAuthError> {
    let bytes = match read {
        Ok(bytes) => bytes,
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Document::default());
        }
        Err(FileStorageError::Cancelled) => return Err(McpAuthError::Cancelled),
        Err(_) => return Err(McpAuthError::Unavailable),
    };
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| McpAuthError::Unsupported)?;
    let object = value.as_object_mut().ok_or(McpAuthError::Unsupported)?;
    if object.remove("version").and_then(|v| v.as_u64()) != Some(VERSION) {
        return Err(McpAuthError::Unsupported);
    }
    serde_json::from_value(value).map_err(|_| McpAuthError::Unsupported)
}

/// Holds a credential store write open for race flow tests. Feature
/// `testing` only; production builds have no hook.
#[cfg(feature = "testing")]
pub mod testing {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };
    use tokio::sync::oneshot;

    type Hold = (PathBuf, oneshot::Sender<()>, oneshot::Receiver<()>);
    static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

    /// Holds the next credential store write under `home`, before it
    /// replaces the file, until this is dropped.
    pub struct HeldWrite {
        arrived: oneshot::Receiver<()>,
        _release: oneshot::Sender<()>,
    }
    impl HeldWrite {
        /// Resolves once the held write has arrived.
        pub async fn arrived(&mut self) {
            let _ = (&mut self.arrived).await;
        }
    }

    pub fn hold_credential_write(home: &Path) -> HeldWrite {
        let (arrive, arrived) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let home = home.canonicalize().expect("an existing home");
        HOLDS.lock().unwrap().push((home, arrive, released));
        HeldWrite {
            arrived,
            _release: release,
        }
    }

    pub(super) async fn pause(home: &Path) {
        let hold = {
            let mut holds = HOLDS.lock().unwrap();
            let index = holds.iter().position(|(held, ..)| held == home);
            index.map(|index| holds.swap_remove(index))
        };
        if let Some((_, arrive, released)) = hold {
            let _ = arrive.send(());
            let _ = released.await;
        }
    }
}
