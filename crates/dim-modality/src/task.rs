//! Task records and files in `pluginFiles` (Modality §2, A.4, format version
//! 1).
//!
//! Everything lives under the Modality root: `modality/tasks/<task_id>/task.json`
//! is the record, `input/` holds the inline inputs, `result/` the result files
//! and `work/` the directory the adapter writes results into. A record write
//! takes the file's lock and compares `revision`; the first terminal write
//! wins and a later outcome is discarded.
use crate::adapter::AdapterProgress;
use dim_agent_types::ModelId;
use dim_app_server::plugin_files::{PluginFileError, PluginFileTransactions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

/// The record format this build reads and writes (A.4).
pub const FORMAT_VERSION: u32 = 1;
/// Every task directory, relative to the plugin files root.
pub const TASKS_DIR: &str = "modality/tasks";

/// The record file of `task_id`, relative to the plugin files root.
pub fn record_name(task_id: &str) -> String {
    format!("{}/{task_id}/task.json", TASKS_DIR)
}
/// The task's directory, relative to the plugin files root.
pub fn task_dir(task_id: &str) -> String {
    format!("{}/{task_id}", TASKS_DIR)
}
/// The task's inline input file `name`.
pub fn input_name(task_id: &str, name: &str) -> String {
    format!("{}/input/{name}", task_dir(task_id))
}
/// The task's input directory.
pub fn input_dir_name(task_id: &str) -> String {
    format!("{}/input", task_dir(task_id))
}
/// The task's result file `name`.
pub fn result_name(task_id: &str, name: &str) -> String {
    format!("{}/result/{name}", task_dir(task_id))
}
/// The task's result directory.
pub fn result_dir_name(task_id: &str) -> String {
    format!("{}/result", task_dir(task_id))
}
/// The task's work directory.
pub fn work_name(task_id: &str) -> String {
    format!("{}/work", task_dir(task_id))
}

/// One task state (A.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}
impl TaskState {
    /// Whether no later write may change the task (A.4, §4).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// Who runs the task (A.4). `adapter` names the executor adapter; until the
/// per-task lookup (#861) lands there is one process service and it is
/// recorded under its service name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Executor {
    Provider { provider: ModelId, adapter: String },
    Host { node: Option<String> },
}

/// One input file of a record (A.4): an inline file under `input/`, or the
/// caller's `node_path`, which Modality reads once and never copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    pub name: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_path: Option<String>,
}

/// One result file (A.4): `bytes` and `sha256` are computed while Modality
/// streams it into `result/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultFile {
    pub name: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
}

/// The result of a succeeded task (A.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResult {
    pub files: Vec<ResultFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Why a task failed (A.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskError {
    pub code: String,
    pub message: String,
}

/// One task record (A.4), format version 1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub version: u32,
    pub task_id: String,
    pub client_task_id: Option<String>,
    /// The revision of this write; every write increments it (A.4).
    pub revision: u64,
    pub category: String,
    pub operation: String,
    pub executor: Executor,
    /// The validated parameters; the input files are replaced by their
    /// metadata under `files` (A.4).
    pub params: Value,
    pub state: TaskState,
    pub remote_handle: Option<String>,
    pub progress: Option<AdapterProgress>,
    pub result: Option<TaskResult>,
    pub error: Option<TaskError>,
    pub remote_cancel: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

/// Why a record could not be read or written.
#[derive(Debug)]
pub enum StoreError {
    /// The file store refused the operation.
    Files(PluginFileError),
    /// The stored text is not a version 1 task record of this shape.
    Invalid(String),
}
impl From<PluginFileError> for StoreError {
    fn from(error: PluginFileError) -> Self {
        Self::Files(error)
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(error) => write!(formatter, "{error}"),
            Self::Invalid(detail) => write!(formatter, "invalid task record: {detail}"),
        }
    }
}

/// What a write accepted as the current record.
#[derive(Clone, Copy, Debug)]
pub enum Expect {
    /// The record is at this revision; any state may still change it.
    Revision(u64),
    /// The record is at this revision and has not started yet.
    Queued(u64),
    /// The record is at this revision and is not terminal; the caller
    /// decided a terminal outcome at that revision, so the first terminal
    /// write wins (§4).
    NonTerminal(u64),
}

/// What a write did.
#[derive(Debug)]
pub enum Write {
    Written(TaskRecord),
    /// Another write got there first; the current record is returned.
    Discarded(Option<TaskRecord>),
}

fn parse(text: &str) -> Result<TaskRecord, serde_json::Error> {
    serde_json::from_str(text)
}

/// Reads a task record, or `None` when it does not exist. The file's lock is
/// taken and the bytes are left unchanged.
pub async fn load(
    files: &PluginFileTransactions,
    task_id: &str,
) -> Result<Option<TaskRecord>, StoreError> {
    let parsed = files
        .transact(&record_name(task_id), |text| {
            (None, text.map(parse).transpose())
        })
        .await?;
    parsed.map_err(|error| StoreError::Invalid(error.to_string()))
}

/// Writes the first revision of a new record. The file must not exist yet.
pub async fn create(
    files: &PluginFileTransactions,
    record: TaskRecord,
) -> Result<TaskRecord, StoreError> {
    match write(files, record, Expect::Revision(0)).await? {
        Write::Written(record) => Ok(record),
        Write::Discarded(_) => Err(StoreError::Invalid("task id already exists".into())),
    }
}

/// Writes `record` under the file's lock when `expect` accepts the current
/// record; `record.revision` becomes one more than the current revision.
pub async fn write(
    files: &PluginFileTransactions,
    mut record: TaskRecord,
    expect: Expect,
) -> Result<Write, StoreError> {
    let name = record_name(&record.task_id);
    let outcome = files
        .transact(&name, move |text| {
            let current = match text {
                Some(text) => match parse(text) {
                    Ok(current) => Some(current),
                    Err(error) => return (None, Err(error)),
                },
                None => None,
            };
            let (expected, accepted) = match expect {
                Expect::Revision(expected) => {
                    let accepted = match &current {
                        Some(current) => current.revision == expected,
                        None => expected == 0,
                    };
                    (expected, accepted)
                }
                Expect::Queued(expected) => {
                    let accepted = current.as_ref().is_some_and(|current| {
                        current.revision == expected && current.state == TaskState::Queued
                    });
                    (expected, accepted)
                }
                Expect::NonTerminal(expected) => {
                    let accepted = current.as_ref().is_some_and(|current| {
                        current.revision == expected && !current.state.is_terminal()
                    });
                    (expected, accepted)
                }
            };
            if !accepted {
                return (None, Ok(Write::Discarded(current)));
            }
            record.revision = expected + 1;
            match serde_json::to_string(&record) {
                Ok(text) => (Some(text), Ok(Write::Written(record))),
                Err(error) => (None, Err(error)),
            }
        })
        .await?;
    outcome.map_err(|error| StoreError::Invalid(error.to_string()))
}

/// Writes an inline input file of `task_id`.
pub async fn write_input(
    files: &PluginFileTransactions,
    task_id: &str,
    name: &str,
    bytes: Vec<u8>,
) -> Result<(), StoreError> {
    files
        .write_stream(&input_name(task_id, name), io::Cursor::new(bytes))
        .await?;
    Ok(())
}

/// The byte count and SHA-256 of what a streamed write read.
#[derive(Clone, Default)]
pub struct Digest {
    state: Arc<Mutex<DigestState>>,
}
#[derive(Default)]
struct DigestState {
    hasher: Sha256,
    bytes: u64,
}
impl Digest {
    /// The bytes read so far.
    pub fn bytes(&self) -> u64 {
        self.state.lock().unwrap().bytes
    }
    /// The lowercase hex SHA-256 of the bytes read so far.
    pub fn sha256(&self) -> String {
        hex(&self.state.lock().unwrap().hasher.clone().finalize())
    }
    /// A reader of `source` that counts and hashes what it yields.
    pub fn reader<R: Read>(&self, source: R) -> DigestReader<R> {
        DigestReader {
            source,
            digest: self.clone(),
        }
    }
}
pub struct DigestReader<R> {
    source: R,
    digest: Digest,
}
impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(buffer)?;
        let mut state = self.digest.state.lock().unwrap();
        state.hasher.update(&buffer[..count]);
        state.bytes += count as u64;
        Ok(count)
    }
}
/// The lowercase hex form of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}
/// The SHA-256 of `bytes`, as a record stores it.
pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Streams `source` into the result file `name` of `task_id`, returning the
/// bytes written and their SHA-256.
pub async fn write_result(
    files: &PluginFileTransactions,
    task_id: &str,
    name: &str,
    source: impl Read + Send + 'static,
) -> Result<(u64, String), StoreError> {
    let digest = Digest::default();
    files
        .write_stream(&result_name(task_id, name), digest.reader(source))
        .await?;
    Ok((digest.bytes(), digest.sha256()))
}

/// Removes one result file of `task_id`, if it exists.
pub async fn remove_result(
    files: &PluginFileTransactions,
    task_id: &str,
    name: &str,
) -> Result<(), StoreError> {
    files.remove(&result_name(task_id, name)).await?;
    Ok(())
}

/// Removes the task's work directory, if it exists.
pub async fn remove_work(files: &PluginFileTransactions, task_id: &str) -> Result<(), StoreError> {
    files.remove(&work_name(task_id)).await?;
    Ok(())
}

/// Removes the whole task directory with its inputs and results.
pub async fn remove_task(files: &PluginFileTransactions, task_id: &str) -> Result<(), StoreError> {
    files.remove(&task_dir(task_id)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_state_is_final() {
        for terminal in [
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::Interrupted,
        ] {
            assert!(terminal.is_terminal());
        }
        assert!(!TaskState::Queued.is_terminal());
        assert!(!TaskState::Running.is_terminal());
    }

    #[test]
    fn hex_and_sha256_agree_with_the_known_digest() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256(b"abc")[..8], *"ba7816bf");
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    #[test]
    fn a_digest_reader_counts_and_hashes_what_it_yields() {
        let digest = Digest::default();
        let mut reader = digest.reader(io::Cursor::new(b"abc".to_vec()));
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
        assert_eq!(digest.bytes(), 3);
        assert_eq!(digest.sha256(), sha256(b"abc"));
    }

    #[test]
    fn names_live_under_the_task_directory() {
        assert_eq!(record_name("mt_1"), "modality/tasks/mt_1/task.json");
        assert_eq!(
            input_name("mt_1", "a.png"),
            "modality/tasks/mt_1/input/a.png"
        );
        assert_eq!(
            result_name("mt_1", "a.png"),
            "modality/tasks/mt_1/result/a.png"
        );
        assert_eq!(work_name("mt_1"), "modality/tasks/mt_1/work");
    }
}
