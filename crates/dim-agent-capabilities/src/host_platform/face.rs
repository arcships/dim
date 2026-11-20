use super::{
    ConfinedError, ConfinedSpec, Confinement, HostPlatform, Os, SandboxBackend, ShellStatus,
    SystemServices,
};
use crate::port_faces::{self, Empty};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

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

async fn confined(
    owner: Arc<HostPlatform>,
    request: ConfinedRequest,
    cancel: CancellationToken,
) -> Reply {
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
    let result = owner
        .spawn_confined(spec, confinement, cancel)
        .await
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRequest {
    #[serde(rename = "taskId")]
    task_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveOutputDirectory {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfiguredRequest {
    command: String,
    cwd: PathBuf,
    #[serde(default)]
    env: BTreeMap<String, Option<String>>,
    stdin: Option<String>,
    timeout_ms: u64,
    limits: OutputLimits,
}

async fn configured(
    owner: Arc<HostPlatform>,
    request: ConfiguredRequest,
    cancel: CancellationToken,
) -> Reply {
    let stdin = STANDARD
        .decode(request.stdin.unwrap_or_default())
        .map_err(|error| port_faces::invalid("execHost", error))?;
    let request = super::ConfiguredCommand {
        command: request.command,
        cwd: request.cwd,
        env: request
            .env
            .into_iter()
            .map(|(k, v)| (k.into(), v.map(Into::into)))
            .collect(),
        stdin,
        timeout: Duration::from_millis(request.timeout_ms),
        stdout_limit: request.limits.stdout,
        stderr_limit: request.limits.stderr,
    };
    let started = std::time::Instant::now();
    let end = owner
        .configured(request, &cancel, Some(&cancel))
        .await
        .map_err(|error| {
            use super::ConfiguredError;
            let code = match &error {
                ConfiguredError::Prepare(error) => error.code,
                ConfiguredError::Spawn(_) => "spawn",
                ConfiguredError::Wait(_, _) => "wait",
                ConfiguredError::Unloaded => "cancelled",
            };
            rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
        })?;
    use super::ConfiguredEnd;
    let (status, exit, stdout, stderr, stdout_truncated, stderr_truncated) = match end {
        ConfiguredEnd::Exited {
            status,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => (
            if status.code().is_some() {
                "exited"
            } else {
                "signaled"
            },
            Some(status),
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        ),
        ConfiguredEnd::Cancelled => ("cancelled", None, Vec::new(), Vec::new(), false, false),
        ConfiguredEnd::TimedOut => ("timed_out", None, Vec::new(), Vec::new(), false, false),
    };
    let mut reply = serde_json::json!({
        "status":status,"durationMs":started.elapsed().as_millis() as u64,
        "stdout":STANDARD.encode(stdout),"stderr":STANDARD.encode(stderr),
        "stdoutTruncated":stdout_truncated,"stderrTruncated":stderr_truncated,
    });
    if let Some(exit) = exit {
        if let Some(code) = exit.code() {
            reply["exitCode"] = code.into();
        }
        if let Some(signal) = super::signal_name(exit) {
            reply["signal"] = signal.into();
        }
    }
    Ok(reply.into())
}

pub(super) struct Dispatch {
    pub(super) owner: Arc<HostPlatform>,
    pub(super) stopping: CancellationToken,
    pub(super) calls: TaskTracker,
}

impl Dispatch {
    fn process<F>(
        &self,
        run: impl FnOnce(Arc<HostPlatform>, CancellationToken) -> F + Send + 'static,
    ) -> Reply
    where
        F: Future<Output = Reply> + Send + 'static,
    {
        let owner = self.owner.clone();
        let cancel = self.stopping.child_token();
        let calls = self.calls.clone();
        Ok(Value::cancellable_future(async move {
            let hold = calls.token();
            if cancel.is_cancelled() {
                return Err(rutis_bridge::exports::failure(
                    "ExecHostError",
                    "cancelled",
                    "execution host closed".into(),
                    None,
                ));
            }
            let _dropped = cancel.clone().drop_guard();
            tokio::spawn(async move {
                let _hold = hold;
                run(owner, cancel).await
            })
            .await
            .map_err(|error| {
                rutis_bridge::exports::failure("ExecHostError", "failed", error.to_string(), None)
            })?
        }))
    }
}

impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "info":"async", "confined":"async", "configured":"async",
            "removeOutputDirectory":"async", "retainOutput":"async", "tail":"async"
        }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if matches!(method, "retainOutput" | "tail") {
            let TaskRequest { task_id } = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            let retain = method == "retainOutput";
            return Ok(Value::future(async move {
                let result = if retain {
                    owner
                        .retain_output(&task_id)
                        .await
                        .map(|path| serde_json::json!({"outputPath": path}))
                } else {
                    owner
                        .tail(&task_id)
                        .await
                        .map(|tail| serde_json::json!(tail))
                };
                result.map(Into::into).map_err(|error| {
                    let code = match error {
                        crate::workspace_tools::exec::CaptureError::Unknown => "task_unknown",
                        crate::workspace_tools::exec::CaptureError::Unavailable(_) => {
                            "output_unavailable"
                        }
                    };
                    rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
                })
            }));
        }
        if method == "configured" {
            let request = port_faces::request("execHost", args)?;
            return self.process(move |owner, cancel| configured(owner, request, cancel));
        }
        if method == "removeOutputDirectory" {
            let RemoveOutputDirectory { path } = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            return Ok(Value::future(async move {
                owner.remove_output_directory(path).await.map_err(|error| {
                    let code = if error.kind() == std::io::ErrorKind::InvalidInput {
                        "invalid"
                    } else {
                        "io"
                    };
                    rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
                })?;
                rutis_bridge::session::arg(&Empty {})
            }));
        }
        if method == "confined" {
            let request = port_faces::request("execHost", args)?;
            return self.process(move |owner, cancel| confined(owner, request, cancel));
        }
        if method != "info" {
            return Err(port_faces::unknown_method("execHost", method));
        }
        let Empty {} = port_faces::request("execHost", args).map_err(|error| {
            rutis_bridge::exports::failure("ExecHostError", "invalid", error.to_string(), None)
        })?;
        let owner = self.owner.clone();
        Ok(Value::cancellable_future(async move {
            rutis_bridge::session::arg(&owner.info().await)
        }))
    }
}
