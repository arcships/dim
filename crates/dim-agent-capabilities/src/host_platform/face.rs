use super::{
    ConfinedError, ConfinedSpec, Confinement, HostPlatform, Os, SandboxBackend, ShellStatus,
    SystemServices,
};
use crate::port_faces::{self, Empty};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: Os,
    pub arch: String,
    pub home: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub exec_root: PathBuf,
    pub platform_roots: Vec<PathBuf>,
    pub backend: SandboxBackend,
    pub exec_shell: ShellStatus,
}

impl HostPlatform {
    pub async fn info(&self) -> HostInfo {
        HostInfo {
            os: self.os,
            arch: self.arch.clone(),
            home: self.home.clone(),
            timezone: self.timezone.clone(),
            exec_root: self.exec_root(),
            platform_roots: self.platform_roots.clone(),
            backend: self.sandbox_backend(),
            exec_shell: self.exec_shell().await,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfinedRequest {
    program: PathBuf,
    args: Vec<String>,
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
    output_limit: OutputLimits,
    services: SystemServices,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputLimits {
    stdout: usize,
    stderr: usize,
}

async fn confined(owner: Arc<HostPlatform>, request: ConfinedRequest) -> Reply {
    let spec = ConfinedSpec {
        executable: request.program,
        args: request.args.into_iter().map(Into::into).collect(),
        env: request
            .env
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    };
    let confinement = Confinement {
        read: request.read,
        write: request.write,
        services: request.services,
        timeout: Duration::from_millis(request.timeout_ms),
        stdout_limit: request.output_limit.stdout,
        stderr_limit: request.output_limit.stderr,
    };
    let cancel = CancellationToken::new();
    let _dropped = cancel.clone().drop_guard();
    let result = tokio::spawn(async move { owner.spawn_confined(spec, confinement, cancel).await })
        .await
        .map_err(|error| {
            rutis_bridge::exports::failure("ExecHostError", "failed", error.to_string(), None)
        })?
        .map_err(|error| {
            let code = match &error {
                ConfinedError::InvalidPath { .. } => "InvalidPath",
                ConfinedError::TimedOut => "TimedOut",
                ConfinedError::OutputLimit { .. } => "OutputLimit",
                ConfinedError::Cancelled => "Cancelled",
                ConfinedError::Spawn(_) => "Spawn",
                ConfinedError::Cleanup(_) => "Cleanup",
            };
            rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
        })?;
    Ok(serde_json::json!({
        "status": result.status.code(),
        "stdout": STANDARD.encode(result.stdout),
        "stderr": STANDARD.encode(result.stderr),
        "confined": result.confined,
        "stdoutTruncated": false,
        "stderrTruncated": false,
    })
    .into())
}

pub(super) struct Dispatch(pub(super) Arc<HostPlatform>);

impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"info":"async","confined":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method == "confined" {
            let request = port_faces::request("execHost", args)?;
            return Ok(Value::cancellable_future(confined(self.0.clone(), request)));
        }
        if method != "info" {
            return Err(port_faces::unknown_method("execHost", method));
        }
        let Empty {} = port_faces::request("execHost", args).map_err(|error| {
            rutis_bridge::exports::failure("ExecHostError", "invalid", error.to_string(), None)
        })?;
        let owner = self.0.clone();
        Ok(Value::cancellable_future(async move {
            rutis_bridge::session::arg(&owner.info().await)
        }))
    }
}
