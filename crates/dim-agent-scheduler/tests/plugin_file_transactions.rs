//! A native process plugin runs typed transactions on its authorized files:
//! the host envelope is written and read by the host, and an invalid envelope
//! is refused without changing its bytes.
use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig, Names};
use dim_app_server::{ServerConfig, serve};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

struct Probe {
    files: Key<dim_app_server::plugin_files::PluginFiles>,
    observed: Arc<OnceLock<Value>>,
}

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "plugin-file-transaction-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.files.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?.open(ctx)?;
            let observed = self.observed.clone();
            tokio::spawn(async move {
                let created = files
                    .with_lock("scheduler/runs.json", |current| {
                        (Some("first".to_owned()), current.map(str::to_owned))
                    })
                    .await;
                let read = files
                    .with_lock("scheduler/runs.json", |current| {
                        (None, current.map(str::to_owned))
                    })
                    .await;
                let refused = files
                    .with_lock("scheduler/invalid.json", |_| (Some("never".to_owned()), ()))
                    .await;
                let _ = observed.set(json!({
                    "created": format!("{created:?}"),
                    "read": format!("{read:?}"),
                    "refused": format!("{refused:?}"),
                }));
            });
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_transactions_write_the_host_envelope_and_refuse_invalid_bytes() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        home.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    let invalid = home.path().join("scheduler/invalid.json");
    let invalid_bytes = br#"{"version":2,"text":"x"}"#;
    std::fs::write(&invalid, invalid_bytes).unwrap();

    let observed = Arc::new(OnceLock::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let probe = observed.clone();
    builder.plugin(move |names: &Names<'_>| {
        Ok(Box::new(Probe {
            files: names.key()?,
            observed: probe.clone(),
        }) as Box<dyn DynPlugin>)
    });
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
    let report = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(report) = observed.get() {
                break report.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the probe reports");
    assert_eq!(report["created"], "Ok(None)", "{report}");
    assert_eq!(report["read"], "Ok(Some(\"first\"))", "{report}");
    assert_eq!(report["refused"], "Err(InvalidData)", "{report}");
    assert_eq!(
        std::fs::read(home.path().join("scheduler/runs.json")).unwrap(),
        br#"{"version":1,"text":"first"}"#
    );
    assert_eq!(std::fs::read(&invalid).unwrap(), invalid_bytes);
    server.shutdown();
    server.join().await.unwrap();
}
