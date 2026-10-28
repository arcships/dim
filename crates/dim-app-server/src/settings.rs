//! Business-owned documents with source-local persistence and CAS: Rust
//! `BusinessDocument` sources and JSON Schema sources, each registered by
//! the plugin that owns it (settings-service design).
mod declared;
mod native;
pub mod workflow_files;
pub use workflow_files::WorkflowFiles;
pub(crate) use workflow_files::WorkflowPackages;

use crate::storage::SqliteStorage;
use dim_agent_capabilities::file_storage::{FilePath, FileStorage, FileStorageError};
use dim_agent_runtime::{Names, Registrations};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Event};
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
    /// No source of this name and kind is registered now.
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
    /// The channel it is sent under ([`project_authorizations`]).
    #[serde(skip)]
    pub name: String,
    pub root: usize,
    pub cwd: PathBuf,
}
impl Event for ProjectAuthorized {
    const NAME: &'static str = "";
    type Value = ();
    fn name(&self) -> &str {
        &self.name
    }
}
/// The rutis channel of project authorizations.
pub fn project_authorizations() -> String {
    "settings/projects".to_owned()
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

/// A registered source: its files and how its documents are checked.
struct Source {
    user_file: String,
    project_file: Option<String>,
    kind: Kind,
}
enum Kind {
    Rust(TypeId),
    Declared(Arc<Declared>),
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
    /// The registered sources by name, each withdrawn when the plugin that
    /// registered it unloads; the documents stay where they are.
    sources: Arc<Registrations<Source>>,
    /// Holds a registration's file check and insertion together.
    registering: Mutex<()>,
}
impl SettingsService {
    /// Registers the Rust source `D` for as long as the plugin that owns
    /// `ctx` is loaded. A source name or file registered already fails.
    pub fn register<D: BusinessDocument>(&self, ctx: &Ctx) -> rutis::Result<()> {
        let failed = |error: SettingsError| registration_failed(D::NAMESPACE, error.to_string());
        validate_namespace(D::NAMESPACE).map_err(failed)?;
        validate_file(D::USER_FILE).map_err(failed)?;
        if let Some(file) = D::PROJECT_FILE {
            validate_file(file).map_err(failed)?;
        }
        self.insert(
            ctx,
            D::NAMESPACE,
            Source {
                user_file: D::USER_FILE.into(),
                project_file: D::PROJECT_FILE.map(Into::into),
                kind: Kind::Rust(TypeId::of::<D>()),
            },
        )
    }
    /// Checks and registers a JSON Schema source for as long as the plugin
    /// that owns `ctx` is loaded. An invalid declaration, or a source name or
    /// file registered already, fails.
    pub fn register_declared(
        &self,
        ctx: &Ctx,
        declaration: SourceDeclaration,
    ) -> rutis::Result<()> {
        let source = declaration.source.clone();
        let declared = Declared::compile(declaration)
            .map_err(|reason| registration_failed(&source, reason))?;
        let (user_file, project_file) = declared.files();
        self.insert(
            ctx,
            &source,
            Source {
                user_file,
                project_file,
                kind: Kind::Declared(Arc::new(declared)),
            },
        )
    }
    /// Two sources sharing a name or a file would share a persisted document.
    fn insert(&self, ctx: &Ctx, name: &str, source: Source) -> rutis::Result<()> {
        let _guard = self.registering.lock().unwrap();
        let taken = self.sources.get(name).is_some()
            || self.sources.all().iter().any(|other| {
                other.value.user_file == source.user_file
                    || (other.value.project_file.is_some()
                        && other.value.project_file == source.project_file)
            });
        if taken {
            return Err(registration_failed(
                name,
                "its name or file is registered already".into(),
            ));
        }
        self.sources.register(ctx, name, source)?;
        Ok(())
    }
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
                    owner.emit(ProjectAuthorized {
                        name: project_authorizations(),
                        root,
                        cwd,
                    });
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
        self.emit(SettingsChanged {
            name: settings_changes(source),
            source: source.to_owned(),
            scope,
            revision: revision.to_owned(),
        });
    }
    /// The change is already committed; a refused announcement cannot undo it,
    /// so it is recorded rather than returned to the writer.
    fn emit<E: Event>(&self, event: E) {
        let name = event.name().to_owned();
        if let Err(error) = self.events.emit(event) {
            tracing::warn!(event = name, %error, "Settings change was not announced");
        }
    }
    fn path<D: BusinessDocument>(&self, scope: SettingsScope) -> Result<FilePath> {
        match self.sources.get(D::NAMESPACE) {
            Some(registered) if matches!(registered.value.kind, Kind::Rust(id) if id == TypeId::of::<D>()) =>
                {}
            _ => return Err(SettingsError::UnknownSource),
        }
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
        self.check_open(cancel)?;
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
fn registration_failed(source: &str, reason: String) -> rutis::Error {
    rutis::Error::from(ErrorKind::PluginFailed(
        format!("settings source {source}: {reason}").into(),
    ))
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
    files: Key<FileStorage>,
    /// SqliteStorage only for the native profile.
    database: Option<Key<SqliteStorage>>,
}
impl SettingsPlugin {
    pub fn new(
        names: &Names<'_>,
        home_root: usize,
        project_roots: Vec<usize>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            home_root,
            project_roots,
            projects: Arc::new(Mutex::new(BTreeMap::new())),
            files: names.key()?,
            database: None,
        })
    }
    pub fn native(
        names: &Names<'_>,
        home_root: usize,
        project_roots: Vec<usize>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            database: Some(names.key()?),
            ..Self::new(names, home_root, project_roots)?
        })
    }
}
impl DynPlugin for SettingsPlugin {
    fn name(&self) -> &str {
        "Settings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![self.files.erase()];
        keys.extend(self.database.as_ref().map(Key::erase));
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            let database = match &self.database {
                Some(key) => Some(ctx.require_at(key)?),
                None => None,
            };
            for root in &self.project_roots {
                let path = files
                    .root_path(*root)
                    .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
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
                sources: Arc::default(),
                registering: Mutex::new(()),
            });
            let owner = service.clone();
            ctx.effect(move || async move {
                {
                    let _guard = owner.gate.lock().unwrap();
                    owner.stopping.cancel();
                    owner.updates.close();
                }
                owner.updates.wait().await;
                Ok(())
            })?;
            declared::provide(ctx, &service)?;
            ctx.provide_as::<WorkflowFiles>(Arc::new(WorkflowFiles(service.clone())))?;
            ctx.provide_as::<SettingsService>(service)?;
            Ok(Effect::Done)
        })
    }
}
