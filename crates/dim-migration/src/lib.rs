mod accounts;
mod archives;
mod engine;
mod inventory;
mod ledger;
mod native;
mod providers;
mod scheduler;
mod settings;
mod source;
pub mod terminal;

use dim_agent_capabilities::file_storage::{
    FileStorage, FileStorageError, FileStoragePlugin, FsHost,
};
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use serde::{Deserialize, Serialize};
pub use source::{Inventory, validate_source_home};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Object(String),
    #[error("{0}")]
    Deferred(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Source(String),
    #[error("{0}")]
    Target(String),
    #[error("{0}")]
    Storage(FileStorageError),
    #[error("Migration terminal output failed: {0}")]
    Terminal(std::io::Error),
    #[error("Migration was cancelled after accepted work settled")]
    Cancelled,
}
impl From<FileStorageError> for Error {
    fn from(error: FileStorageError) -> Self {
        match error {
            FileStorageError::Cancelled => Self::Cancelled,
            error => Self::Storage(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Running,
    Completed,
    Partial,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Imported,
    AlreadyImported,
    Conflict,
    Deferred,
    Unsupported,
    Failed,
    Cancelled,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectResult {
    pub kind: String,
    pub id: String,
    pub outcome: Outcome,
    pub target_id: Option<String>,
    pub reason: Option<String>,
    pub omitted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskState {
    pub format: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    pub source_home: PathBuf,
    pub status: TaskStatus,
    pub stage: String,
    pub processed: u64,
    pub total: Option<u64>,
    pub current: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReport {
    pub task: TaskState,
    pub objects: Vec<ObjectResult>,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub task: TaskState,
    pub recent: VecDeque<ObjectResult>,
}

pub struct MigrationPlugin {
    home: PathBuf,
    themes: Vec<String>,
    native_directory: PathBuf,
}

impl MigrationPlugin {
    pub fn new(home: PathBuf, native_directory: PathBuf, themes: Vec<String>) -> Self {
        Self {
            home,
            native_directory,
            themes,
        }
    }
}

impl Plugin for MigrationPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "Migration"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let home = std::fs::canonicalize(&self.home).map_err(|error| {
            rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
        })?;
        let storage = ctx.plugin(FileStoragePlugin::new(vec![home.clone()]));
        (&storage).await.map_err(|error| {
            rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
        })?;
        let files = ctx.get::<FileStorage>().ok_or_else(|| {
            rutis::Error::from(ErrorKind::PluginFailed(
                "Migration FileStorage is unavailable".into(),
            ))
        })?;
        let (progress, _) = watch::channel(None);
        let host = ctx.get::<FsHost>().ok_or_else(|| {
            rutis::Error::from(ErrorKind::PluginFailed(
                "Migration fsHost is unavailable".into(),
            ))
        })?;
        let ledger = ledger::Ledger::open(host, home, progress.clone())
            .await
            .map_err(|error| {
                rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
            })?;
        let service = Arc::new(Migration {
            ledger,
            files,
            stopping: CancellationToken::new(),
            tasks: TaskTracker::new(),
            progress,
            themes: self.themes.clone(),
            native_directory: self.native_directory.clone(),
            admission: Mutex::new(()),
        });
        let owner = service.clone();
        ctx.effect(move || async move {
            {
                let _guard = owner.admission.lock().unwrap();
                owner.stopping.cancel();
                owner.tasks.close();
            }
            owner.tasks.wait().await;
            Ok(())
        })?;
        ctx.provide_as(service)?;
        Ok(())
    }
}

pub struct Migration {
    ledger: ledger::Ledger,
    files: Arc<FileStorage>,
    stopping: CancellationToken,
    tasks: TaskTracker,
    progress: watch::Sender<Option<Progress>>,
    themes: Vec<String>,
    native_directory: PathBuf,
    admission: Mutex<()>,
}

impl Migration {
    pub fn progress(&self) -> watch::Receiver<Option<Progress>> {
        self.progress.subscribe()
    }
    pub fn cancel_current(&self) {
        self.stopping.cancel();
    }

    pub async fn run(
        self: &Arc<Self>,
        source_home: PathBuf,
        source_stopped: bool,
        retry: Option<String>,
    ) -> Result<TaskReport> {
        if !source_stopped {
            return Err(Error::Source(
                "Exit legacy CLI and Desktop using this home, then pass --source-stopped".into(),
            ));
        }
        let source_home = validate_source_home(&source_home, &self.ledger.home)?;
        let task = {
            let _guard = self.admission.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let owner = self.clone();
            self.tasks
                .spawn(async move { engine::run(owner, source_home, retry).await })
        };
        task.await.map_err(|_| {
            Error::Source("Migration worker stopped before its result was recorded".into())
        })?
    }

    pub async fn report(&self, id: &str) -> Result<TaskReport> {
        self.ledger.report(id).await
    }
    pub async fn list(&self) -> Result<Vec<TaskState>> {
        self.ledger.list().await
    }
    pub async fn cancel(&self, id: &str) -> Result<TaskReport> {
        let report = self.report(id).await?;
        if report.task.status == TaskStatus::Running {
            self.ledger.request_cancel(id).await?;
        }
        self.report(id).await
    }

    pub async fn inspect(
        self: &Arc<Self>,
        source_home: PathBuf,
        source_stopped: bool,
    ) -> Result<Inventory> {
        if !source_stopped {
            return Err(Error::Source(
                "Exit legacy CLI and Desktop using this home, then pass --source-stopped".into(),
            ));
        }
        let task = {
            let _guard = self.admission.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let owner = self.clone();
            self.tasks.spawn(async move {
                source::Source::with_snapshot(&owner, source_home, async |source| {
                    Ok(source.inventory.clone())
                })
                .await
            })
        };
        task.await.map_err(|_| {
            Error::Source("Migration inspection stopped before cleanup completed".into())
        })?
    }
}
