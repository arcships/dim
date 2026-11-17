use dim_agent_runtime::Modules;
use dim_agent_runtime::wait_active;
use dim_app_server::mcp::{McpConnection, McpOwner, McpPublication, McpService, Result};
use rutis::Ctx;
use rutis_loader::{Layer, Loader, LoaderPlugin, Patch};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
#[allow(dead_code)]
pub struct Directory {
    service: std::sync::Arc<McpService>,
    owner: McpOwner,
}
#[allow(dead_code)]
impl Directory {
    pub fn new(service: std::sync::Arc<McpService>, root: &Ctx, loader: Loader) -> Self {
        Self {
            service,
            owner: McpOwner::new(root.clone(), loader),
        }
    }
    pub async fn server(
        &self,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<std::sync::Arc<McpConnection>> {
        self.service
            .server(self.owner.clone(), project, id, cancel)
            .await
    }
}
impl std::ops::Deref for Directory {
    type Target = std::sync::Arc<McpService>;
    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

/// Provides the process value MCP needs for reference expansion. A fixture
/// host reads no provider credential from the environment.
pub fn provide_credentials(root: &Ctx) {
    root.provide(dim_agent_capabilities::host::CredentialVariablesHost::new(
        std::sync::Arc::new(std::collections::BTreeSet::<String>::new),
    ))
    .unwrap();
}

pub struct Host {
    view: rutis::host::FiberView,
}
impl Host {
    pub async fn start(root: &Ctx) -> (Self, Loader) {
        let catalog = dim_app_server::catalog();
        let mut builtins = Modules::new();
        let row = McpPublication::register(&mut builtins, &catalog).unwrap();
        let template = McpPublication::standalone_template(&mut builtins);
        let plugin = LoaderPlugin::new(builtins, Default::default());
        let loader = plugin.handle();
        let view = root.plugin(plugin);
        let stop = CancellationToken::new();
        wait_active(
            root,
            std::slice::from_ref(&view),
            Duration::from_secs(5),
            &stop,
        )
        .await
        .unwrap();
        let assembly = Layer::new(
            "assembly",
            vec![Patch {
                insert: Some(vec![json!({"id":row,"name":row}), template]),
                ..Default::default()
            }],
        );
        let report = loader
            .reconcile(vec![assembly.clone()], None)
            .await
            .unwrap();
        assert!(report.failures.is_empty() && report.issues.is_empty());
        (Self { view }, loader)
    }
    pub async fn close(&self) {
        self.view.shutdown().await.unwrap();
    }
}
