use super::HostPlatform;
use crate::file_storage::FileStorageError;
#[cfg(unix)]
use dim_agent_types::{
    IsolationEscalation, IsolationPolicyEffective, IsolationPolicySource, Sandbox,
};
use dim_agent_types::{IsolationPolicy, IsolationPolicySnapshot};
#[cfg(unix)]
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum IsolationPolicyError {
    #[error("Invalid isolation policy: {0}")]
    Invalid(String),
    #[error("Isolation policy changed; reload settings before saving")]
    Conflict,
    #[error("Command isolation management is unavailable on this execution node")]
    Unavailable,
    #[error(transparent)]
    Storage(#[from] FileStorageError),
}

#[cfg(unix)]
pub(super) fn effective(policy: &IsolationPolicy) -> Result<IsolationPolicyEffective, String> {
    match policy.version {
        1 if policy.sandbox.is_some() || policy.escalation.is_some() => Err(
            "host-isolation.json version 1 does not accept sandbox or escalation; use version 2"
                .into(),
        ),
        1 => Ok(IsolationPolicyEffective {
            sandbox: if policy.enabled == Some(false) {
                Sandbox::Host
            } else if policy.network == Some(true) {
                Sandbox::Network
            } else {
                Sandbox::Workspace
            },
            escalation: IsolationEscalation::Deny,
        }),
        2 if policy.enabled.is_some() || policy.network.is_some() => Err(
            "host-isolation.json version 2 names the tier with sandbox, not enabled or network"
                .into(),
        ),
        2 => Ok(IsolationPolicyEffective {
            sandbox: policy.sandbox.unwrap_or(Sandbox::Workspace),
            escalation: policy.escalation.unwrap_or(IsolationEscalation::Ask),
        }),
        _ => Err("host-isolation.json version must be 1 or 2".into()),
    }
}

#[cfg(unix)]
async fn validate(policy: &IsolationPolicy) -> Result<IsolationPolicyEffective, String> {
    let effective = effective(policy)?;
    if policy.environment.as_ref().is_some_and(|names| {
        names
            .iter()
            .any(|name| name.is_empty() || name.contains(['\0', '=']))
    }) {
        return Err("Environment names must be nonempty and contain neither NUL nor '='".into());
    }
    let (read, files) =
        super::isolation::roots(policy.read_only_paths.clone().unwrap_or_default(), true).await?;
    let (write, _) =
        super::isolation::roots(policy.writable_paths.clone().unwrap_or_default(), false).await?;
    super::isolation::validate_overlap(&read, &files, &write)?;
    Ok(effective)
}

#[cfg(unix)]
fn revision(bytes: Option<&[u8]>) -> String {
    use sha2::{Digest, Sha256};
    match bytes {
        Some(bytes) => format!("sha256:{:x}", Sha256::digest(bytes)),
        None => "absent".into(),
    }
}

#[cfg(unix)]
fn optional_bytes(
    result: Result<Vec<u8>, FileStorageError>,
) -> Result<Option<Vec<u8>>, IsolationPolicyError> {
    match result {
        Ok(bytes) => Ok(Some(bytes)),
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
async fn snapshot(
    path: PathBuf,
    bytes: Option<&[u8]>,
) -> Result<IsolationPolicySnapshot, IsolationPolicyError> {
    let policy = match bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|error| {
            IsolationPolicyError::Invalid(format!(
                "host-isolation.json at line {}, column {}",
                error.line(),
                error.column()
            ))
        })?,
        None => IsolationPolicy::default(),
    };
    let effective = validate(&policy)
        .await
        .map_err(IsolationPolicyError::Invalid)?;
    Ok(IsolationPolicySnapshot {
        revision: revision(bytes),
        path,
        source: if bytes.is_some() {
            IsolationPolicySource::File
        } else {
            IsolationPolicySource::Defaults
        },
        policy,
        effective,
    })
}

impl HostPlatform {
    /// Reads and validates the execution node's policy, without exposing environment values.
    pub async fn read_isolation_policy(
        &self,
        cancel: &CancellationToken,
    ) -> Result<IsolationPolicySnapshot, IsolationPolicyError> {
        #[cfg(unix)]
        {
            let (storage, root, relative) = self
                .isolation_file
                .as_ref()
                .ok_or(IsolationPolicyError::Unavailable)?;
            let file = storage.path(*root, relative)?;
            let bytes = optional_bytes(storage.read(&file, cancel).await)?;
            snapshot(storage.root_path(*root)?.join(relative), bytes.as_deref()).await
        }
        #[cfg(not(unix))]
        {
            let _ = cancel;
            Err(IsolationPolicyError::Unavailable)
        }
    }

    /// Atomically saves a validated policy if its source still matches the inspected revision.
    /// Running processes retain their original boundary.
    pub async fn update_isolation_policy(
        &self,
        expected_revision: &str,
        policy: IsolationPolicy,
        cancel: &CancellationToken,
    ) -> Result<IsolationPolicySnapshot, IsolationPolicyError> {
        #[cfg(unix)]
        {
            let (storage, root, relative) = self
                .isolation_file
                .as_ref()
                .ok_or(IsolationPolicyError::Unavailable)?;
            let file = storage.path(*root, relative)?;
            let mut lease = storage.lock(&file, cancel).await?;
            let current = optional_bytes(lease.read(cancel).await)?;
            if revision(current.as_deref()) != expected_revision {
                return Err(IsolationPolicyError::Conflict);
            }
            let effective = validate(&policy)
                .await
                .map_err(IsolationPolicyError::Invalid)?;
            let mut bytes = serde_json::to_vec_pretty(&policy)
                .map_err(|error| IsolationPolicyError::Invalid(error.to_string()))?;
            bytes.push(b'\n');
            let updated = IsolationPolicySnapshot {
                revision: revision(Some(&bytes)),
                path: storage.root_path(*root)?.join(relative),
                source: IsolationPolicySource::File,
                policy,
                effective,
            };
            lease.replace(bytes, None, cancel).await?;
            Ok(updated)
        }
        #[cfg(not(unix))]
        {
            let _ = (expected_revision, policy, cancel);
            Err(IsolationPolicyError::Unavailable)
        }
    }

    /// Saves explicit product defaults, replacing custom lists, with the same revision check as update.
    pub async fn reset_isolation_policy(
        &self,
        expected_revision: &str,
        cancel: &CancellationToken,
    ) -> Result<IsolationPolicySnapshot, IsolationPolicyError> {
        self.update_isolation_policy(expected_revision, IsolationPolicy::default(), cancel)
            .await
    }
}
