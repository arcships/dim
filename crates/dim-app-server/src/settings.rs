//! Source-targeted settings. No MCP connections or Skill discovery.
mod mcp_project;
mod native;

use crate::storage::SqliteStorage;
use dim_agent_capabilities::file_storage::{FilePath, FileStorage, FileStorageError};
use dim_agent_capabilities::skill_settings::{
    SettingsVersion, SkillSettings as SkillSettingsPort, SkillSettingsError, SkillSettingsHost,
    SkillSettingsSnapshot,
};
use dim_agent_runtime::{Capability, KeysOf, StateCell, When};
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypeKey, TypedPlugin};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Storage(FileStorageError),
    #[error("invalid settings: {0}")]
    Invalid(&'static str),
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

/// Roots are authorized FileStorage indices supplied by the product host, not
/// arbitrary paths received from RPC. Project files are always `.mcp.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingsSource {
    UserMcp,
    ProjectMcp(usize),
    Skills,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpSettings {
    pub mcp_servers: BTreeMap<String, McpServerSettings>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerSettings {
    Stdio {
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        kind: Option<Stdio>,
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Remote {
        #[serde(rename = "type")]
        kind: RemoteTransport,
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        auth: Option<BearerAuth>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Stdio {
    Stdio,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteTransport {
    Http,
    Sse,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BearerAuth {
    Bearer { token: String },
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillSettings {
    pub disabled_skill_ids: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Contains server-side secrets (MCP headers/env); never serialize this snapshot
/// directly into client notifications or diagnostic logs.
#[derive(Clone, PartialEq)]
pub enum SettingsDocument {
    Mcp(McpSettings),
    Skills(SkillSettings),
}
#[derive(Clone, PartialEq)]
pub struct SettingsSnapshot {
    pub revision: String,
    pub document: SettingsDocument,
}

enum SettingsChange {
    Replace(SettingsDocument),
    SkillEnabled { id: String, enabled: bool },
}

pub struct SettingsService {
    files: Arc<FileStorage>,
    database: Option<Arc<SqliteStorage>>,
    home_root: usize,
    projects: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    updates: TaskTracker,
    changed: StateCell<u64>,
    skills_changed: StateCell<SettingsVersion>,
}
impl SettingsService {
    /// The product host authorizes canonical workspaces before Session loading.
    /// This port is never exposed as an arbitrary path/root-index RPC.
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
                if owner.projects.lock().unwrap().insert(cwd, root) != Some(root) {
                    owner.changed.update(|version| *version += 1);
                }
                Ok(root)
            })
        };
        task.await?
    }

    pub fn project_root(&self, cwd: &Path) -> Option<usize> {
        self.projects.lock().unwrap().get(cwd).copied()
    }

    pub(crate) fn mcp_sources(&self) -> Vec<(SettingsSource, Option<PathBuf>)> {
        std::iter::once((SettingsSource::UserMcp, None))
            .chain(
                self.projects
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(path, index)| (SettingsSource::ProjectMcp(*index), Some(path.clone()))),
            )
            .collect()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn path(&self, source: SettingsSource) -> Result<FilePath> {
        let (root, file) = match source {
            SettingsSource::UserMcp => (self.home_root, "mcp.json"),
            SettingsSource::Skills => (self.home_root, "skills.json"),
            SettingsSource::ProjectMcp(root)
                if self
                    .projects
                    .lock()
                    .unwrap()
                    .values()
                    .any(|index| *index == root) =>
            {
                (root, ".mcp.json")
            }
            SettingsSource::ProjectMcp(_) => {
                return Err(SettingsError::Invalid("unauthorized project"));
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

    pub async fn read(
        &self,
        source: SettingsSource,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot> {
        self.check_open(cancel)?;
        if self.database.is_some()
            && matches!(source, SettingsSource::UserMcp | SettingsSource::Skills)
        {
            return self.native_read(source, cancel).await;
        }
        decode(source, self.files.read(&self.path(source)?, cancel).await)
    }

    /// CAS is checked in the source's write transaction or file lease, not against
    /// an in-memory counter. Workspace edits made outside this service are detected.
    pub async fn update(
        self: &Arc<Self>,
        source: SettingsSource,
        expected_revision: &str,
        document: SettingsDocument,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot> {
        validate(source, &document)?;
        self.write(
            source,
            expected_revision,
            SettingsChange::Replace(document),
            cancel,
        )
        .await
    }

    async fn write(
        self: &Arc<Self>,
        source: SettingsSource,
        expected_revision: &str,
        change: SettingsChange,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot> {
        let path = self.path(source)?;
        let task = {
            let _guard = self.gate.lock().unwrap();
            self.check_open(cancel)?;
            let owner = self.clone();
            let expected_revision = expected_revision.to_owned();
            let cancel = cancel.clone();
            self.updates.spawn(async move {
                owner
                    .replace(source, path, &expected_revision, change, &cancel)
                    .await
            })
        };
        task.await?
    }

    async fn replace(
        &self,
        source: SettingsSource,
        path: FilePath,
        expected_revision: &str,
        change: SettingsChange,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot> {
        if self.database.is_some()
            && matches!(source, SettingsSource::UserMcp | SettingsSource::Skills)
        {
            return self
                .native_replace(source, expected_revision, change, cancel)
                .await;
        }
        let mut lease = loop {
            self.check_open(cancel)?;
            match self.files.lock(&path, cancel).await {
                Ok(lease) => break lease,
                Err(FileStorageError::Locked) => tokio::select! {
                    _ = self.stopping.cancelled() => return Err(SettingsError::Closed),
                    _ = cancel.cancelled() => return Err(SettingsError::Cancelled),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {},
                },
                Err(error) => return Err(error.into()),
            }
        };
        let current = decode(source, lease.read(cancel).await)?;
        if current.revision != expected_revision {
            return Err(SettingsError::Conflict);
        }
        let Some(document) = change.apply(&current.document) else {
            return Ok(current);
        };
        let mut bytes = match &document {
            SettingsDocument::Mcp(value) => serde_json::to_vec_pretty(value),
            SettingsDocument::Skills(value) => serde_json::to_vec_pretty(value),
        }
        .map_err(|_| SettingsError::Invalid("cannot encode settings"))?;
        bytes.push(b'\n');
        let revision = revision(Some(&bytes));
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(std::fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let permissions = None;
        lease.replace(bytes, permissions, cancel).await?;
        // Publish under the lease so concurrent commits cannot reorder versions.
        if source == SettingsSource::Skills {
            self.skills_changed.set(revision.clone());
        }
        self.changed.update(|version| *version += 1);
        drop(lease);
        Ok(SettingsSnapshot { revision, document })
    }
}

impl SettingsChange {
    fn apply(self, current: &SettingsDocument) -> Option<SettingsDocument> {
        match self {
            Self::Replace(document) => Some(document),
            Self::SkillEnabled { id, enabled } => {
                let SettingsDocument::Skills(mut settings) = current.clone() else {
                    unreachable!("Skill updates use the Skills source")
                };
                if settings.disabled_skill_ids.contains(&id) != enabled {
                    return None;
                }
                if enabled {
                    settings.disabled_skill_ids.retain(|value| value != &id);
                } else {
                    settings.disabled_skill_ids.push(id);
                }
                Some(SettingsDocument::Skills(settings))
            }
        }
    }
}

impl SkillSettingsPort for SettingsService {
    fn skills(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, std::result::Result<SkillSettingsSnapshot, SkillSettingsError>> {
        Box::pin(async move {
            let snapshot = self.read(SettingsSource::Skills, &cancel).await?;
            let SettingsDocument::Skills(settings) = snapshot.document else {
                unreachable!("Skills source decodes to a Skills document")
            };
            Ok(SkillSettingsSnapshot {
                version: snapshot.revision,
                disabled: settings.disabled_skill_ids.into_iter().collect(),
            })
        })
    }

    fn update_skills(
        self: Arc<Self>,
        expected: SettingsVersion,
        id: String,
        enabled: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, std::result::Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move {
            self.check_open(&cancel)?;
            if id.trim().is_empty() {
                return Err(SkillSettingsError::Invalid("empty Skill id".into()));
            }
            let saved = self
                .write(
                    SettingsSource::Skills,
                    &expected,
                    SettingsChange::SkillEnabled { id, enabled },
                    &cancel,
                )
                .await?;
            Ok(saved.revision)
        })
    }

    fn changes(&self) -> tokio::sync::watch::Receiver<SettingsVersion> {
        self.skills_changed.subscribe()
    }
}

impl From<SettingsError> for SkillSettingsError {
    fn from(error: SettingsError) -> Self {
        match error {
            SettingsError::Conflict => Self::Conflict,
            SettingsError::Invalid(reason) => Self::Invalid(reason.into()),
            SettingsError::Closed | SettingsError::Storage(FileStorageError::Closed) => {
                Self::Closed
            }
            SettingsError::Cancelled | SettingsError::Storage(FileStorageError::Cancelled) => {
                Self::Cancelled
            }
            error => Self::Failed(error.to_string()),
        }
    }
}

fn revision(bytes: Option<&[u8]>) -> String {
    match bytes {
        Some(bytes) => format!("{:x}", Sha256::digest(bytes)),
        None => "missing".into(),
    }
}

fn decode(
    source: SettingsSource,
    read: std::result::Result<Vec<u8>, FileStorageError>,
) -> Result<SettingsSnapshot> {
    let bytes = match read {
        Ok(bytes) => Some(bytes),
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let document = match (source, bytes.as_deref()) {
        (SettingsSource::Skills, Some(bytes)) => SettingsDocument::Skills(
            serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Invalid("invalid skills.json"))?,
        ),
        (SettingsSource::Skills, None) => SettingsDocument::Skills(SkillSettings {
            disabled_skill_ids: vec![],
            extra: BTreeMap::new(),
        }),
        (_, Some(bytes)) => SettingsDocument::Mcp(
            serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Invalid("invalid MCP configuration"))?,
        ),
        (_, None) => SettingsDocument::Mcp(McpSettings {
            mcp_servers: BTreeMap::new(),
            extra: BTreeMap::new(),
        }),
    };
    validate(source, &document)?;
    Ok(SettingsSnapshot {
        revision: revision(bytes.as_deref()),
        document,
    })
}

fn validate(source: SettingsSource, document: &SettingsDocument) -> Result<()> {
    match (source, document) {
        (SettingsSource::Skills, SettingsDocument::Skills(settings)) => {
            if settings.extra.contains_key("disabledSkillIds") {
                return Err(SettingsError::Invalid("reserved Skill configuration field"));
            }
            if settings
                .disabled_skill_ids
                .iter()
                .any(|id| id.trim().is_empty())
            {
                return Err(SettingsError::Invalid("empty Skill id"));
            }
        }
        (
            SettingsSource::UserMcp | SettingsSource::ProjectMcp(_),
            SettingsDocument::Mcp(settings),
        ) => {
            if settings.extra.contains_key("mcpServers") {
                return Err(SettingsError::Invalid("reserved MCP configuration field"));
            }
            for (id, server) in &settings.mcp_servers {
                if id.trim().is_empty() {
                    return Err(SettingsError::Invalid("empty MCP server id"));
                }
                let (target, extra) = match server {
                    McpServerSettings::Stdio { command, extra, .. } => (command, extra),
                    McpServerSettings::Remote {
                        url, auth, extra, ..
                    } => {
                        if let Some(BearerAuth::Bearer { token }) = auth
                            && token.trim().is_empty()
                        {
                            return Err(SettingsError::Invalid("empty MCP bearer token"));
                        }
                        (url, extra)
                    }
                };
                if target.trim().is_empty()
                    || extra.keys().any(|key| {
                        matches!(
                            key.as_str(),
                            "type"
                                | "command"
                                | "args"
                                | "env"
                                | "cwd"
                                | "enabled"
                                | "url"
                                | "headers"
                                | "auth"
                                | "transport"
                                | "oauth"
                        )
                    })
                {
                    return Err(SettingsError::Invalid(
                        "invalid MCP transport configuration",
                    ));
                }
                if extra
                    .get("metadata")
                    .is_some_and(|value| !value.is_object())
                {
                    return Err(SettingsError::Invalid("MCP metadata must be an object"));
                }
            }
        }
        _ => {
            return Err(SettingsError::Invalid(
                "settings source and document disagree",
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct SettingsPlugin {
    home_root: usize,
    project_roots: Vec<usize>,
    projects: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    native: bool,
}
impl SettingsPlugin {
    pub fn new(home_root: usize, project_roots: Vec<usize>) -> Self {
        Self {
            home_root,
            project_roots,
            projects: Arc::new(Mutex::new(BTreeMap::new())),
            native: false,
        }
    }

    /// SQLite owns user documents after their first successful import. Workspace
    /// files remain authoritative; legacy user files are retained but never dual-written.
    pub fn native(home_root: usize, project_roots: Vec<usize>) -> Self {
        Self {
            native: true,
            ..Self::new(home_root, project_roots)
        }
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
            for root in &self.project_roots {
                let path = files
                    .root_path(*root)
                    .map_err(|error| CordisError::PluginFailed(error.into()))?;
                if path.to_str().is_none() {
                    return Err(CordisError::PluginFailed(Box::new(SettingsError::Invalid(
                        "project cwd must be UTF-8",
                    ))));
                }
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
                changed: StateCell::new(0),
                skills_changed: StateCell::new("unread".into()),
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
            // Optional files are read by their consumers; a corrupt source must
            // not prevent the settings authority from registering.
            ctx.provide(SkillSettingsHost(service.clone()))?;
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
