//! A native process plugin outside App Server reads `sessions` by
//! its service name, the way the TypeScript Scheduler does.
use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{ServerConfig, serve};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, TypeKey};
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

struct Probe(Arc<OnceLock<Value>>);

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "scheduler-permission-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let (ctx, observed) = (ctx.clone(), self.0.clone());
            tokio::spawn(async move {
                let mut last = String::new();
                for _ in 0..200 {
                    match rutis_bridge::session::call_service::<Value>(
                        &ctx,
                        "sessions",
                        "permissionDefaults",
                        (json!({}),),
                    )
                    .await
                    {
                        Ok(value) => {
                            let _ = observed.set(value);
                            return;
                        }
                        Err(error) => {
                            last = error.to_string();
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                let _ = observed.set(json!({ "error": last }));
            });
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_process_plugin_reads_session_permissions_by_name() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        home.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let observed = Arc::new(OnceLock::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let probe = observed.clone();
    builder.plugin(move |_names| Ok(Box::new(Probe(probe.clone())) as Box<dyn DynPlugin>));
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let value = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(value) = observed.get() {
                break value.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the probe reports a reply");
    assert!(value.get("error").is_none(), "{value}");
    assert_eq!(
        value,
        json!({"preset":"read-only","allowedTools":[],"disallowedTools":[]}),
        "the reply is the host's default policy"
    );
    server.shutdown();
    server.join().await.unwrap();
}
