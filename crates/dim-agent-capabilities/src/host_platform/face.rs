use super::{HostPlatform, Os, SandboxBackend, ShellStatus};
use crate::port_faces::{self, Empty};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: Os,
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
            home: self.home.clone(),
            timezone: self.timezone.clone(),
            exec_root: self.exec_root(),
            platform_roots: self.platform_roots.clone(),
            backend: self.sandbox_backend(),
            exec_shell: self.exec_shell().await,
        }
    }
}

pub(super) struct Dispatch(pub(super) Arc<HostPlatform>);

impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"info":"async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
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
