//! One disposable native MCP binding per Gateway/Session, never shared settings.
use crate::rpc::{self, Rpc};
use agent_client_protocol::{Error, schema::v1 as acp};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) type Servers = BTreeMap<String, Value>;
pub(crate) fn servers(input: Vec<acp::McpServer>) -> Result<Servers, Error> {
    let mut result = BTreeMap::new();
    for server in input {
        let (name, configuration) = match server {
            acp::McpServer::Stdio(server) => (
                server.name,
                json!({"command":server.command,"args":server.args,"env":pairs(server.env.into_iter().map(|v|(v.name,v.value)))?}),
            ),
            acp::McpServer::Http(server) => (
                server.name,
                json!({"type":"http","url":server.url,"headers":pairs(server.headers.into_iter().map(|v|(v.name.to_ascii_lowercase(),v.value)))?}),
            ),
            acp::McpServer::Sse(server) => (
                server.name,
                json!({"type":"sse","url":server.url,"headers":pairs(server.headers.into_iter().map(|v|(v.name.to_ascii_lowercase(),v.value)))?}),
            ),
            _ => return Err(Error::invalid_params()),
        };
        if name.trim().is_empty() || result.insert(name, configuration).is_some() {
            return Err(Error::invalid_params());
        }
    }
    Ok(result)
}
fn pairs(
    values: impl Iterator<Item = (String, String)>,
) -> Result<BTreeMap<String, String>, Error> {
    let mut result = BTreeMap::new();
    for (name, value) in values {
        if result.insert(name, value).is_some() {
            return Err(Error::invalid_params());
        }
    }
    Ok(result)
}
struct Binding {
    id: String,
    servers: Servers,
    generation: tokio::sync::Mutex<Option<u64>>,
}
#[derive(Clone)]
pub(crate) struct Bindings {
    rpc: Rpc,
    entries: Arc<Mutex<HashMap<String, Arc<Binding>>>>,
    stop: CancellationToken,
}
impl Bindings {
    pub(crate) fn start(rpc: Rpc, tasks: &TaskTracker, stop: CancellationToken) -> Self {
        let owner = Self {
            rpc,
            entries: Arc::default(),
            stop,
        };
        let worker = owner.clone();
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    () = worker.stop.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
                let entries: Vec<_> = worker
                    .entries
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, binding)| (id.clone(), binding.clone()))
                    .collect();
                for (id, binding) in entries {
                    let renew = async {
                        let mut generation = binding.generation.lock().await;
                        if generation.is_some() && worker.rpc.ready() {
                            let result = tokio::time::timeout(
                                Duration::from_secs(5),
                                worker.rpc.call(
                                    "session.mcp_temporary.renew",
                                    json!({"sessionId":id,"bindingId":binding.id}),
                                ),
                            )
                            .await;
                            if !matches!(result, Ok(Ok(_))) {
                                *generation = None;
                            }
                        }
                    };
                    tokio::select! { ()=worker.stop.cancelled()=>return, ()=renew=>{} }
                }
            }
        });
        owner
    }
    pub(crate) fn supported(&self) -> bool {
        self.rpc.supports("mcp.temporary.capabilities")
    }
    pub(crate) fn can_coalesce(&self, id: &str, servers: &Servers) -> bool {
        servers.is_empty()
            || !self.supported()
            || self
                .entries
                .lock()
                .unwrap()
                .get(id)
                .is_some_and(|entry| &entry.servers == servers)
    }
    pub(crate) fn plan(&self, id: &str, servers: Servers) -> Result<bool, Error> {
        if servers.is_empty() || !self.supported() {
            return Ok(false);
        }
        let mut entries = self.entries.lock().unwrap();
        if let Some(binding) = entries.get(id) {
            if binding.servers != servers {
                return Err(Error::new(
                    -32602,
                    "This Session already has a different client MCP binding",
                ));
            }
        } else {
            entries.insert(
                id.into(),
                Arc::new(Binding {
                    id: uuid::Uuid::new_v4().to_string(),
                    servers,
                    generation: tokio::sync::Mutex::new(None),
                }),
            );
            return Ok(true);
        }
        Ok(false)
    }
    pub(crate) async fn ensure(&self, id: &str) -> Result<(), Error> {
        let binding = self.entries.lock().unwrap().get(id).cloned();
        let Some(binding) = binding else {
            return Ok(());
        };
        let operation = async {
            let mut generation = binding.generation.lock().await;
            let current = self.rpc.generation();
            if *generation == Some(current) {
                match self
                    .rpc
                    .call(
                        "session.mcp_temporary.renew",
                        json!({"sessionId":id,"bindingId":binding.id}),
                    )
                    .await
                {
                    Ok(_) => return Ok(()),
                    Err(error)
                        if error
                            .data
                            .as_ref()
                            .is_some_and(|v| v["nativeAppError"] == "mcp_binding_missing") => {}
                    Err(error) => return Err(error),
                }
            }
            *generation = None;
            self.rpc
                .call(
                    "session.mcp_temporary.attach",
                    json!({"sessionId":id,"bindingId":binding.id,"servers":binding.servers}),
                )
                .await?;
            *generation = Some(current);
            Ok(())
        };
        tokio::select! {
            () = self.stop.cancelled() => Err(rpc::unavailable()),
            result = operation => result,
        }
    }
    pub(crate) fn forget(&self, id: &str) {
        self.entries.lock().unwrap().remove(id);
    }
    pub(crate) fn release(
        &self,
        id: &str,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let binding = self.entries.lock().unwrap().remove(id);
        let rpc = self.rpc.clone();
        let id = id.to_owned();
        async move {
            if let Some(binding) = binding {
                let _ = rpc
                    .call(
                        "session.mcp_temporary.detach",
                        json!({"sessionId":id,"bindingId":binding.id}),
                    )
                    .await;
            }
        }
    }
    pub(crate) async fn close(&self) {
        let entries = std::mem::take(&mut *self.entries.lock().unwrap());
        for (id, binding) in entries {
            let _ = self
                .rpc
                .call(
                    "session.mcp_temporary.detach",
                    json!({"sessionId":id,"bindingId":binding.id}),
                )
                .await;
        }
    }
}
