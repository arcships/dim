use super::{HostPlatform, ShellStatus, native, parse_preference, resolve_shell};
use crate::file_storage::{FileStorageError, FsPermissions};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum ExecShellSettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("Execution shell preference changed; reload before saving")]
    Conflict,
    #[error("Execution shell settings are unavailable on this host")]
    Unavailable,
    #[error(transparent)]
    Storage(#[from] FileStorageError),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecShellSettings {
    pub revision: String,
    pub executable_path: Option<String>,
    pub shells: Vec<ShellChoice>,
    pub config_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellChoice {
    pub executable_path: String,
    pub name: String,
}

fn revision(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(
        || "absent".into(),
        |bytes| format!("sha256:{:x}", Sha256::digest(bytes)),
    )
}

fn optional_bytes(
    result: Result<Vec<u8>, FileStorageError>,
) -> Result<Option<Vec<u8>>, ExecShellSettingsError> {
    match result {
        Ok(bytes) => Ok(Some(bytes)),
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

impl HostPlatform {
    async fn shell_settings_snapshot(
        &self,
        bytes: Option<&[u8]>,
    ) -> Result<ExecShellSettings, ExecShellSettingsError> {
        let parsed = bytes.map_or(Ok(None), parse_preference);
        let config_error = parsed.as_ref().err().cloned();
        #[cfg(unix)]
        let detected = {
            let env = self.env.clone();
            tokio::task::spawn_blocking(move || native::detect(&env))
                .await
                .map_err(|_| ExecShellSettingsError::Unavailable)?
        };
        #[cfg(windows)]
        let detected = native::detect(&self.env).await;
        Ok(ExecShellSettings {
            revision: revision(bytes),
            executable_path: parsed.unwrap_or(None),
            config_error,
            shells: detected
                .shells
                .into_iter()
                .map(|shell| ShellChoice {
                    executable_path: shell.path.to_string_lossy().into_owned(),
                    name: shell.edition.name().into(),
                })
                .collect(),
        })
    }

    /// Reads the persisted preference and detected choices without changing running shells.
    pub async fn read_exec_shell_settings(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ExecShellSettings, ExecShellSettingsError> {
        let (storage, path) = self
            .shell_file
            .as_ref()
            .ok_or(ExecShellSettingsError::Unavailable)?;
        let bytes = optional_bytes(storage.read_bytes(path, usize::MAX, None, cancel).await)?;
        self.shell_settings_snapshot(bytes.as_deref()).await
    }

    /// Atomically selects an available shell, or automatic detection when the path is absent.
    /// New invocations read this preference; existing processes keep their shell.
    pub async fn update_exec_shell_settings(
        &self,
        expected_revision: &str,
        executable_path: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<ExecShellSettings, ExecShellSettingsError> {
        let (storage, path) = self
            .shell_file
            .as_ref()
            .ok_or(ExecShellSettingsError::Unavailable)?;
        let document = match executable_path {
            Some(path) => serde_json::json!({"version": 1, "executablePath": path}),
            None => serde_json::json!({"version": 1}),
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|_| {
            ExecShellSettingsError::Invalid("Invalid execution shell preference".into())
        })?;
        match resolve_shell(Ok(Some(bytes.clone())), &self.env).await {
            ShellStatus::Available(_) => {}
            ShellStatus::Unavailable { reason, .. } => {
                return Err(ExecShellSettingsError::Invalid(reason));
            }
        }
        let lease = storage.lock(None, path, cancel).await?;
        let updated = async {
            let current = optional_bytes(
                storage
                    .read_bytes(path, usize::MAX, Some(lease), cancel)
                    .await,
            )?;
            if revision(current.as_deref()) != expected_revision {
                return Err(ExecShellSettingsError::Conflict);
            }
            let snapshot = self.shell_settings_snapshot(Some(&bytes)).await?;
            storage
                .replace(path, bytes, FsPermissions::Preserve, Some(lease), cancel)
                .await?;
            Ok(snapshot)
        }
        .await;
        storage.unlock(lease).await?;
        updated
    }
}
