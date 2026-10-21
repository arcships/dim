//! Row-scoped definition files; business parsing and enablement belong to Workflow.
use super::SettingsService;
use dim_agent_capabilities::file_storage::{EntryKind, FileStorageError};
use rutis::{CordisError, Ctx, TypeKey};
use rutis_bridge::session::Bytes;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc};

/// Package placement from one process row's structural configuration.
pub(crate) struct WorkflowPackages(BTreeMap<String, usize>);
impl WorkflowPackages {
    pub(crate) fn from_config(
        config: &serde_json::Value,
        roots: &[PathBuf],
    ) -> Result<Self, CordisError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Package {
            id: String,
            root: PathBuf,
        }
        let mut packages = BTreeMap::new();
        if let Some(declared) = config.get("packages") {
            let declared: Vec<Package> =
                serde_json::from_value(declared.clone()).map_err(|error| {
                    CordisError::PluginFailed(format!("invalid workflow packages: {error}").into())
                })?;
            for package in declared {
                let root = roots
                    .iter()
                    .position(|root| *root == package.root)
                    .ok_or_else(|| {
                        CordisError::PluginFailed(
                            "package root must be a canonical authorized host root".into(),
                        )
                    })?;
                if package.id.is_empty() || packages.insert(package.id, root).is_some() {
                    return Err(CordisError::PluginFailed(
                        "package ids must be nonempty and unique per row".into(),
                    ));
                }
            }
        }
        Ok(Self(packages))
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum WorkflowRoot {
    Project { project: PathBuf },
    Package { package: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDirectoryRequest {
    pub root: WorkflowRoot,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReadRequest {
    pub root: WorkflowRoot,
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReplaceRequest {
    pub root: WorkflowRoot,
    pub path: String,
    pub bytes: Bytes,
}

#[derive(Debug, Serialize)]
pub struct WorkflowFileEntry {
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WorkflowFilesError {
    #[error("{message}")]
    InvalidPath { message: String },
    #[error("{message}")]
    Closed { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    Locked { message: String },
    #[error("{message}")]
    Unsupported { message: String },
    #[error("{message}")]
    Unsynced { message: String },
    #[error("{message}")]
    Io { message: String },
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    InvalidData { message: String },
}
impl WorkflowFilesError {
    fn invalid(message: &str) -> Self {
        Self::InvalidPath {
            message: message.into(),
        }
    }
}
impl From<FileStorageError> for WorkflowFilesError {
    fn from(error: FileStorageError) -> Self {
        let message = error.to_string();
        match error {
            FileStorageError::InvalidPath(_) => Self::InvalidPath { message },
            FileStorageError::Closed => Self::Closed { message },
            FileStorageError::Cancelled => Self::Cancelled { message },
            FileStorageError::Locked => Self::Locked { message },
            FileStorageError::Unsupported => Self::Unsupported { message },
            FileStorageError::Unsynced(_) => Self::Unsynced { message },
            FileStorageError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::NotFound { message }
            }
            FileStorageError::Io(_)
            | FileStorageError::Worker(_)
            | FileStorageError::Cleanup { .. }
            | FileStorageError::Release(_) => Self::Io { message },
        }
    }
}

/// Generated access to authorized project definitions and the calling row's packages.
pub struct WorkflowFiles(pub(super) Arc<SettingsService>);
impl WorkflowFiles {
    fn root(
        &self,
        caller: &Ctx,
        selector: &WorkflowRoot,
        write: bool,
    ) -> Result<(usize, &'static str), WorkflowFilesError> {
        if self.0.stopping.is_cancelled() {
            return Err(WorkflowFilesError::Closed {
                message: "Settings is closed".into(),
            });
        }
        if caller.cancellation_token().is_cancelled() {
            return Err(WorkflowFilesError::Cancelled {
                message: "calling row is unloading".into(),
            });
        }
        let packages = caller
            .require_as::<WorkflowPackages>(TypeKey::instance::<WorkflowPackages>(
                caller.instance(),
            ))
            .map_err(|_| {
                WorkflowFilesError::invalid("workflow files require a configured process row")
            })?;
        match selector {
            WorkflowRoot::Project { project } => self
                .0
                .project_root(project)
                .map(|root| (root, ".dimcode/workflows"))
                .ok_or_else(|| WorkflowFilesError::invalid("unauthorized project")),
            WorkflowRoot::Package { package } if !write => packages
                .0
                .get(package)
                .map(|root| (*root, "workflows"))
                .ok_or_else(|| WorkflowFilesError::invalid("package is not declared by this row")),
            WorkflowRoot::Package { .. } => Err(WorkflowFilesError::invalid(
                "package definitions are read-only",
            )),
        }
    }

    /// Reads raw bytes from one .yaml basename; no S10 envelope or text conversion.
    pub async fn read(
        &self,
        caller: &Ctx,
        request: WorkflowReadRequest,
    ) -> std::result::Result<Bytes, WorkflowFilesError> {
        let (root, prefix) = self.root(caller, &request.root, false)?;
        let path = self
            .0
            .files
            .path(root, PathBuf::from(prefix).join(filename(&request.path)?))?;
        let cancel = caller.cancellation_token().child_token();
        let _cancel = cancel.clone().drop_guard();
        Ok(self.0.files.read(&path, &cancel).await?.into())
    }

    /// Lists the fixed definition directory; non-UTF-8 names fail without aliases.
    pub async fn list(
        &self,
        caller: &Ctx,
        request: WorkflowDirectoryRequest,
    ) -> std::result::Result<Vec<WorkflowFileEntry>, WorkflowFilesError> {
        let (root, prefix) = self.root(caller, &request.root, false)?;
        let path = self.0.files.path(root, prefix)?;
        let cancel = caller.cancellation_token().child_token();
        let _cancel = cancel.clone().drop_guard();
        self.0
            .files
            .list(&path, &cancel)
            .await?
            .into_iter()
            .map(|entry| {
                Ok(WorkflowFileEntry {
                    name: entry.name.into_string().map_err(|_| {
                        WorkflowFilesError::InvalidData {
                            message: "definition entry name is not UTF-8".into(),
                        }
                    })?,
                    kind: entry.kind,
                })
            })
            .collect()
    }

    /// Atomically replaces a project definition. Package selectors return invalid_path.
    pub async fn replace(
        &self,
        caller: &Ctx,
        request: WorkflowReplaceRequest,
    ) -> std::result::Result<(), WorkflowFilesError> {
        let (root, prefix) = self.root(caller, &request.root, true)?;
        let path = self
            .0
            .files
            .path(root, PathBuf::from(prefix).join(filename(&request.path)?))?;
        let cancel = caller.cancellation_token().child_token();
        let _cancel = cancel.clone().drop_guard();
        Ok(self
            .0
            .files
            .replace(&path, request.bytes.into(), None, &cancel)
            .await?)
    }

    /// Ensures only the project's .dimcode and .dimcode/workflows directories.
    pub async fn create_directory(
        &self,
        caller: &Ctx,
        request: WorkflowDirectoryRequest,
    ) -> std::result::Result<(), WorkflowFilesError> {
        let (root, prefix) = self.root(caller, &request.root, true)?;
        let cancel = caller.cancellation_token().child_token();
        let _cancel = cancel.clone().drop_guard();
        for relative in [".dimcode", prefix] {
            let path = self.0.files.path(root, relative)?;
            self.0.files.create_directory(&path, &cancel).await?;
        }
        Ok(())
    }
}

fn filename(path: &str) -> Result<&str, WorkflowFilesError> {
    if !path.ends_with(".yaml") || path.contains(['/', '\\', '\0']) {
        return Err(WorkflowFilesError::invalid("expected one .yaml basename"));
    }
    Ok(path)
}
