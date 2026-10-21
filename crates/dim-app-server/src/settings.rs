//! Business-owned documents with source-local persistence and CAS: Rust
//! `BusinessDocument` sources and the JSON Schema sources non-Rust owners
//! declare (settings-service design).
mod declared;
mod native;
pub mod workflow_files;
pub use workflow_files::WorkflowFiles;
pub(crate) use workflow_files::WorkflowPackages;

use crate::storage::SqliteStorage;
use dim_agent_capabilities::file_storage::{FilePath, FileStorage, FileStorageError};
use dim_agent_runtime::{Capability, KeysOf, When};
use rutis::{BoxFuture, CordisError, Ctx, Effect, Event, EventKey, TypeKey, TypedPlugin};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    any::TypeId,
    collections::BTreeMap,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use declared::Declared;
pub(crate) use declared::{Read, Update};
pub use declared::{SourceDeclaration, Writer};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Storage(FileStorageError),
    #[error("invalid settings: {0}")]
    Invalid(&'static str),
    /// Not a declared source; Rust sources are served by their owners.
    #[error("unknown settings source")]
    UnknownSource,
    #[error("invalid settings scope: {0}")]
    InvalidScope(&'static str),
    /// A written document breaks its declared schema. The message names the
    /// instance path, never the value.
    #[error("invalid settings document: {0}")]
    Document(String),
    /// An owner write changes the user-only field at this JSON Pointer.
    #[error("settings field {0} is changed only by the user")]
    UserOnlyField(String),
    #[error("settings changed; read the current version before updating")]
    Conflict,
    #[error("settings service is closed")]
    Closed,
    #[error("settings operation cancelled")]
    Cancelled,
    #[error("settings persistence failed; read the current revision before retrying")]
    Persistence,
    #[error("settings worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}
impl From<FileStorageError> for SettingsError {
    fn from(error: FileStorageError) -> Self {
        match error {
            FileStorageError::Cancelled => Self::Cancelled,
            FileStorageError::Closed => Self::Closed,
            error => Self::Storage(error),
        }
    }
}
pub type Result<T> = std::result::Result<T, SettingsError>;

/// Declared in the capabilities crate, so listeners in any crate share the type.
pub use dim_agent_capabilities::settings::{SettingsChanged, SettingsScope, settings_changes};

/// A project directory newly authorized for project sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAuthorized {
    pub root: usize,
    pub cwd: PathBuf,
}
impl Event for ProjectAuthorized {
    const NAME: &'static str = "settings.project_authorized";
    type Value = ();
}
/// The rutis channel of project authorizations.
pub fn project_authorizations() -> EventKey<ProjectAuthorized> {
    EventKey::dynamic("settings/projects")
}

/// Wakes a receiver on every event it listens to, for an owner whose read
/// loop waits on a watch receiver: it rereads the source after each wake.
#[derive(Clone)]
pub(crate) struct Wake(Arc<tokio::sync::watch::Sender<()>>);
impl Wake {
    pub(crate) fn new() -> Self {
        Self(Arc::new(tokio::sync::watch::Sender::new(())))
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.0.subscribe()
    }
}
impl<E: Event<Value = ()>> rutis::Listener<E> for Wake {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        _: &'a E,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        self.0.send_replace(());
        Box::pin(async { Ok(None) })
    }
}

pub trait BusinessDocument: Default + Serialize + DeserializeOwned + Send + Sync + 'static {
    const NAMESPACE: &'static str;
    const USER_FILE: &'static str;
    const PROJECT_FILE: Option<&'static str> = None;
    fn validate(&self) -> Result<()>;
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SettingsSnapshot<D> {
    pub revision: String,
    pub document: D,
}

/// How one source's documents cross the persistence boundary, so Rust and
/// declared sources share the file and native paths, keyed by source name.
trait Format: Send + Sync + 'static {
    type Document: Serialize + Send + 'static;
    fn source(&self) -> &str;
    /// Decodes and checks a persisted document; `None` is a missing file.
    fn decode(&self, bytes: Option<&[u8]>) -> Result<Self::Document>;
}
struct Typed<D>(PhantomData<fn() -> D>);
impl<D: BusinessDocument> Format for Typed<D> {
    type Document = D;
    fn source(&self) -> &str {
        D::NAMESPACE
    }
    fn decode(&self, bytes: Option<&[u8]>) -> Result<D> {
        let document: D = match bytes {
            Some(bytes) => serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Invalid("invalid settings document"))?,
            None => D::default(),
        };
        document.validate()?;
        Ok(document)
    }
}
fn typed<D: BusinessDocument>() -> Arc<Typed<D>> {
    Arc::new(Typed(PhantomData))
}

/// Who holds a source name or file: a Rust document type or one declaration.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claimant {
    Rust(TypeId),
    Declared(usize),
}
fn claim_names(source: &str, user_file: &str, project_file: Option<&str>) -> Vec<String> {
    [format!("namespace {source}"), format!("user {user_file}")]
        .into_iter()
        .chain(project_file.map(|file| format!("project {file}")))
        .collect()
}

pub struct SettingsService {
    files: Arc<FileStorage>,
    database: Option<Arc<SqliteStorage>>,
    home_root: usize,
    projects: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    updates: TaskTracker,
    /// Settings' own plugin context, which emits change events.
    events: Ctx,
    /// The claimant of each namespace and file; another claimant naming one
    /// of them would share its persisted source.
    claims: Mutex<BTreeMap<String, Claimant>>,
    /// The declared sources, compiled once at apply.
    declared: BTreeMap<String, Arc<Declared>>,
}
impl SettingsService {
    pub async fn authorize_project(
        self: &Arc<Self>,
        cwd: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        if cwd.to_str().is_none() {
            return Err(SettingsError::Invalid("project cwd must be UTF-8"));
        }
        let task = {
            let _guard = self.gate.lock().unwrap();
            self.check_open(cancel)?;
            let owner = self.clone();
            let cancel = cancel.clone();
            self.updates.spawn(async move {
                let root = owner.files.register_root(cwd.clone(), &cancel).await?;
                let _guard = owner.gate.lock().unwrap();
                owner.check_open(&cancel)?;
                if owner.projects.lock().unwrap().insert(cwd.clone(), root) != Some(root) {
                    owner.emit(&project_authorizations(), ProjectAuthorized { root, cwd });
                }
                Ok(root)
            })
        };
        task.await?
    }
    pub fn project_root(&self, cwd: &Path) -> Option<usize> {
        self.projects.lock().unwrap().get(cwd).copied()
    }
    pub fn project_path(&self, root: usize) -> Result<PathBuf> {
        self.projects
            .lock()
            .unwrap()
            .iter()
            .find(|(_, index)| **index == root)
            .map(|(path, _)| path.clone())
            .ok_or(SettingsError::Invalid("unauthorized project"))
    }
    pub fn project_sources(&self) -> Vec<(usize, PathBuf)> {
        self.projects
            .lock()
            .unwrap()
            .iter()
            .map(|(path, index)| (*index, path.clone()))
            .collect()
    }
    /// Announces a committed write on the source's channel. Callers hold the
    /// commit order (the gate or the native connection), so events follow it.
    fn publish(&self, source: &str, scope: SettingsScope, revision: &str) {
        self.emit(
            &settings_changes(source),
            SettingsChanged {
                source: source.to_owned(),
                scope,
                revision: revision.to_owned(),
            },
        );
    }
    /// The change is already committed; a refused announcement cannot undo it,
    /// so it is recorded rather than returned to the writer.
    fn emit<E: Event>(&self, key: &EventKey<E>, event: E) {
        if let Err(error) = self
            .events
            .events()
            .emit(&self.events, key, Arc::new(event))
        {
            tracing::warn!(event = E::NAME, %error, "Settings change was not announced");
        }
    }
    fn path<D: BusinessDocument>(&self, scope: SettingsScope) -> Result<FilePath> {
        validate_namespace(D::NAMESPACE)?;
        validate_file(D::USER_FILE)?;
        if let Some(file) = D::PROJECT_FILE {
            validate_file(file)?;
        }
        self.claim(
            Claimant::Rust(TypeId::of::<D>()),
            claim_names(D::NAMESPACE, D::USER_FILE, D::PROJECT_FILE),
        )?;
        self.locate(D::USER_FILE, D::PROJECT_FILE, scope)
    }
    fn locate(
        &self,
        user_file: &str,
        project_file: Option<&str>,
        scope: SettingsScope,
    ) -> Result<FilePath> {
        let (root, file) = match scope {
            SettingsScope::User => (self.home_root, user_file),
            SettingsScope::Project(root) => {
                self.project_path(root)?;
                (
                    root,
                    project_file.ok_or(SettingsError::Invalid("document has no project source"))?,
                )
            }
        };
        Ok(self.files.path(root, file)?)
    }
    fn claim(&self, claimant: Claimant, names: Vec<String>) -> Result<()> {
        let mut claims = self.claims.lock().unwrap();
        claim(&mut claims, claimant, names)
    }
    fn check_open(&self, cancel: &CancellationToken) -> Result<()> {
        if self.stopping.is_cancelled() {
            Err(SettingsError::Closed)
        } else if cancel.is_cancelled() {
            Err(SettingsError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub async fn read<D: BusinessDocument>(
        &self,
        scope: SettingsScope,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<D>> {
        self.check_open(cancel)?;
        let path = self.path::<D>(scope)?;
        self.read_source(typed::<D>(), path, scope, cancel).await
    }
    pub async fn update<D: BusinessDocument>(
        self: &Arc<Self>,
        scope: SettingsScope,
        expected: &str,
        document: D,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<D>> {
        document.validate()?;
        let path = self.path::<D>(scope)?;
        self.replace(
            typed::<D>(),
            path,
            scope,
            expected,
            document,
            |_, _| Ok(()),
            cancel,
        )
        .await
    }
    async fn read_source<F: Format>(
        &self,
        format: Arc<F>,
        path: FilePath,
        scope: SettingsScope,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<F::Document>> {
        if scope == SettingsScope::User && self.database.is_some() {
            return self.native_read(format, path, cancel).await;
        }
        snapshot(&*format, self.files.read(&path, cancel).await)
    }
    /// CAS on the revision. `check` sees the current and the new document
    /// under the same lease (or native transaction) as the revision check.
    #[allow(clippy::too_many_arguments)]
    async fn replace<F: Format>(
        self: &Arc<Self>,
        format: Arc<F>,
        path: FilePath,
        scope: SettingsScope,
        expected: &str,
        document: F::Document,
        check: impl FnOnce(&F::Document, &F::Document) -> Result<()> + Send + 'static,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<F::Document>> {
        let task = {
            let _guard = self.gate.lock().unwrap();
            self.check_open(cancel)?;
            let owner = self.clone();
            let expected = expected.to_owned();
            let cancel = cancel.clone();
            self.updates.spawn(async move {
                if scope == SettingsScope::User && owner.database.is_some() {
                    return owner
                        .native_replace(format, path, expected, document, check, &cancel)
                        .await;
                }
                let mut lease = owner.lease(&path, &cancel).await?;
                let current = snapshot(&*format, lease.read(&cancel).await)?;
                if current.revision != expected {
                    return Err(SettingsError::Conflict);
                }
                check(&current.document, &document)?;
                let mut bytes = serde_json::to_vec_pretty(&document)
                    .map_err(|_| SettingsError::Invalid("cannot encode settings"))?;
                bytes.push(b'\n');
                let revision = revision(Some(&bytes));
                match lease.replace(bytes, private_permissions(), &cancel).await {
                    Ok(()) => {}
                    // The new content is what readers now see: consumers follow
                    // it, and the caller learns it may not survive a crash.
                    Err(FileStorageError::Unsynced(error)) => {
                        owner.publish(format.source(), scope, &revision);
                        return Err(FileStorageError::Unsynced(error).into());
                    }
                    Err(error) => return Err(error.into()),
                }
                owner.publish(format.source(), scope, &revision);
                Ok(SettingsSnapshot { revision, document })
            })
        };
        task.await?
    }
    async fn lease(
        &self,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<dim_agent_capabilities::file_storage::FileLease> {
        loop {
            self.check_open(cancel)?;
            match self.files.lock(path, cancel).await {
                Ok(lease) => return Ok(lease),
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = self.stopping.cancelled() => return Err(SettingsError::Closed),
                    _ = cancel.cancelled() => return Err(SettingsError::Cancelled),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {},
                },
                Err(error) => return Err(error.into()),
            }
        }
    }
}
fn claim(
    claims: &mut BTreeMap<String, Claimant>,
    claimant: Claimant,
    names: Vec<String>,
) -> Result<()> {
    if names
        .iter()
        .any(|name| claims.get(name).is_some_and(|claimed| *claimed != claimant))
    {
        return Err(SettingsError::Invalid(
            "settings source belongs to another document type",
        ));
    }
    for name in names {
        claims.insert(name, claimant);
    }
    Ok(())
}
fn validate_namespace(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    {
        return Err(SettingsError::Invalid("invalid document namespace"));
    }
    Ok(())
}
fn validate_file(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', '\0'])
        || Path::new(value).is_absolute()
    {
        return Err(SettingsError::Invalid("invalid document filename"));
    }
    Ok(())
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
fn revision(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(
        || "missing".into(),
        |bytes| format!("{:x}", Sha256::digest(bytes)),
    )
}
fn snapshot<F: Format>(
    format: &F,
    read: std::result::Result<Vec<u8>, FileStorageError>,
) -> Result<SettingsSnapshot<F::Document>> {
    let bytes = match read {
        Ok(bytes) => Some(bytes),
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(SettingsSnapshot {
        document: format.decode(bytes.as_deref())?,
        revision: revision(bytes.as_deref()),
    })
}

#[derive(Clone)]
pub struct SettingsPlugin {
    home_root: usize,
    project_roots: Vec<usize>,
    projects: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    native: bool,
    /// Rust sources the composition root registers, claimed before any
    /// declaration so a declaration cannot take one of their names.
    reserved: Vec<(TypeId, Vec<String>)>,
    declarations: Vec<SourceDeclaration>,
}
impl SettingsPlugin {
    pub fn new(home_root: usize, project_roots: Vec<usize>) -> Self {
        Self {
            home_root,
            project_roots,
            projects: Arc::new(Mutex::new(BTreeMap::new())),
            native: false,
            reserved: Vec::new(),
            declarations: Vec::new(),
        }
    }
    pub fn native(home_root: usize, project_roots: Vec<usize>) -> Self {
        Self {
            native: true,
            ..Self::new(home_root, project_roots)
        }
    }
    /// Reserves a Rust source's names: a declaration naming one fails assembly.
    pub fn reserve<D: BusinessDocument>(mut self) -> Self {
        self.reserved.push((
            TypeId::of::<D>(),
            claim_names(D::NAMESPACE, D::USER_FILE, D::PROJECT_FILE),
        ));
        self
    }
    /// The declared sources this Settings serves. Each is checked and compiled
    /// in `apply`; an invalid or duplicate declaration fails assembly.
    pub fn declare(mut self, declarations: Vec<SourceDeclaration>) -> Self {
        self.declarations.extend(declarations);
        self
    }
}
impl TypedPlugin for SettingsPlugin {
    type Deps = (Arc<FileStorage>, When<Arc<SqliteStorage>>);
    fn name(&self) -> &str {
        "Settings"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (files, When(database)): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let mut claims = BTreeMap::new();
            for (owner, names) in &self.reserved {
                claim(&mut claims, Claimant::Rust(*owner), names.clone())
                    .map_err(|error| CordisError::PluginFailed(error.to_string().into()))?;
            }
            let mut declared = BTreeMap::new();
            for (index, declaration) in self.declarations.iter().enumerate() {
                let failed = |reason: String| {
                    CordisError::PluginFailed(
                        format!(
                            "settings source {} of {}: {reason}",
                            declaration.source, declaration.owner
                        )
                        .into(),
                    )
                };
                let compiled = Declared::compile(declaration.clone()).map_err(failed)?;
                claim(
                    &mut claims,
                    Claimant::Declared(index),
                    claim_names(
                        &declaration.source,
                        &declaration.user_file,
                        declaration.project_file.as_deref(),
                    ),
                )
                .map_err(|error| failed(error.to_string()))?;
                declared.insert(declaration.source.clone(), Arc::new(compiled));
            }
            for root in &self.project_roots {
                let path = files
                    .root_path(*root)
                    .map_err(|error| CordisError::PluginFailed(error.into()))?;
                self.projects.lock().unwrap().insert(path, *root);
            }
            let service = Arc::new(SettingsService {
                files,
                database,
                home_root: self.home_root,
                projects: self.projects.clone(),
                stopping: CancellationToken::new(),
                gate: Mutex::new(()),
                updates: TaskTracker::new(),
                events: ctx.clone(),
                claims: Mutex::new(claims),
                declared,
            });
            let owner = service.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        {
                            let _guard = owner.gate.lock().unwrap();
                            owner.stopping.cancel();
                            owner.updates.close();
                        }
                        owner.updates.wait().await;
                        Ok(())
                    })
                }))
            })?;
            declared::provide(ctx, &service)?;
            ctx.provide_as(
                TypeKey::of::<WorkflowFiles>(),
                Arc::new(WorkflowFiles(service.clone())),
            )?;
            ctx.provide_as(TypeKey::of::<SettingsService>(), service)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for SettingsPlugin {
    fn keys(&self) -> KeysOf<Self> {
        ((), self.native.then_some(()))
    }
}
