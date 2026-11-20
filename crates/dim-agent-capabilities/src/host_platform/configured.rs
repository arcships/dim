use super::{ExecHost, ProcessSpecError};
use std::{
    collections::BTreeMap, ffi::OsString, path::PathBuf, process::ExitStatus, time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub struct ConfiguredCommand {
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, Option<OsString>>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

pub enum ConfiguredEnd {
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub enum ConfiguredError {
    Prepare(ProcessSpecError),
    Unloaded,
    Spawn(std::io::Error),
    Wait(std::io::Error, Vec<u8>),
}

impl ConfiguredError {
    pub fn stderr(&self) -> &[u8] {
        match self {
            Self::Wait(_, stderr) => stderr,
            _ => &[],
        }
    }
}

impl std::fmt::Display for ConfiguredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prepare(error) => write!(f, "{}: {}", error.code, error.reason),
            Self::Unloaded => f.write_str("unloaded"),
            Self::Spawn(error) => write!(f, "spawn failed: {error}"),
            Self::Wait(error, _) => write!(f, "wait failed: {error}"),
        }
    }
}

impl ExecHost {
    pub async fn configured(
        &self,
        request: ConfiguredCommand,
        cancel: &CancellationToken,
        gate: Option<&CancellationToken>,
    ) -> Result<ConfiguredEnd, ConfiguredError> {
        let (mut spec, _) = self
            .build_process_spec(&request.command, &request.cwd)
            .await
            .map_err(ConfiguredError::Prepare)?;
        spec.env.retain(|(name, _)| !request.env.contains_key(name));
        spec.env.extend(
            request
                .env
                .into_iter()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        );
        if gate.is_some_and(CancellationToken::is_cancelled) {
            return Err(ConfiguredError::Unloaded);
        }
        let mut tree = self
            .spawn_with_stdin(spec, request.stdin)
            .map_err(ConfiguredError::Spawn)?;
        let mut stdout = tree.stdout();
        let mut stderr = tree.stderr();
        let end = tokio::select! {
            biased;
            _ = cancel.cancelled() => ConfiguredEnd::Cancelled,
            _ = tokio::time::sleep(request.timeout) => ConfiguredEnd::TimedOut,
            (out, err, status) = async {
                tokio::join!(
                    read_capped(&mut stdout, request.stdout_limit),
                    read_capped(&mut stderr, request.stderr_limit),
                    tree.wait()
                )
            } => {
                return match status {
                    Ok(status) => Ok(ConfiguredEnd::Exited {
                        status,
                        stdout: out.0,
                        stderr: err.0,
                        stdout_truncated: out.1,
                        stderr_truncated: err.1,
                    }),
                    Err(error) => Err(ConfiguredError::Wait(error, err.0))
                }
            }
        };
        let _ = tree.terminate().await;
        Ok(end)
    }
}

/// Reads at most `limit` bytes and drains the rest, so the writer never
/// blocks; the bool says more was written.
async fn read_capped<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return (kept, overflow),
            Ok(read) => {
                let room = limit.saturating_sub(kept.len());
                if read <= room {
                    kept.extend_from_slice(&buffer[..read]);
                } else {
                    kept.extend_from_slice(&buffer[..room]);
                    overflow = true;
                }
            }
        }
    }
}
