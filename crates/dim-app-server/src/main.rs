use dim_agent_aimux::{AimuxModels, ConfiguredModelPlugin, config::ModelConfigFile};
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    context_methods::{self, ContextMethodsPlugin},
    context_model::ProductContextModel,
    defaults,
    history::HistoryRenderer,
    host::{CompactionStrategyHost, ContextModelHost, IdentityContext, PrepareContextHost},
    model_provider::ModelProviderPlugin,
    summary_strategy::ProductSummaryStrategyPlugin,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{
    ServerConfig, ShutdownReason, collaboration, permission_rules, serve,
    session_typescript::SessionTypeScriptConfig,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[cfg(windows)]
mod pipe_client;

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // The client transport does not acquire daemon locks or load product state.
    if std::env::args().nth(1).as_deref() == Some("--connect-pipe") {
        let mut args = std::env::args().skip(2);
        let name = args.next().ok_or("--connect-pipe requires a name")?;
        if args.next().is_some() {
            return Err("--connect-pipe cannot be combined with server options".into());
        }
        #[cfg(windows)]
        return pipe_client::run(&name).map_err(Into::into);
        #[cfg(not(windows))]
        {
            let _ = name;
            return Err("--connect-pipe requires Windows".into());
        }
    }
    // Closing an operation's span logs its duration (time.busy + time.idle).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .init();
    let mut config = ServerConfig {
        websocket_address: Some(([127, 0, 0, 1], 8787).into()),
        ..Default::default()
    };
    let mut threads = 2usize;
    let mut model_config = None;
    let mut mock_model = false;
    let mut plugin_dir = None;
    let mut node = None;
    let mut js_runtime = None;
    let mut session_rows = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--unix" => {
                config.unix_path = Some(PathBuf::from(args.next().ok_or("--unix requires a path")?))
            }
            "--pipe" => config.pipe_name = Some(args.next().ok_or("--pipe requires a name")?),
            "--ws" => {
                config.websocket_address = Some(
                    args.next()
                        .ok_or("--ws requires an address")?
                        .parse::<SocketAddr>()?,
                )
            }
            "--no-ws" => config.websocket_address = None,
            "--database" => {
                config.database_path = Some(PathBuf::from(
                    args.next().ok_or("--database requires a path")?,
                ))
            }
            "--home" => {
                config.target_home =
                    Some(PathBuf::from(args.next().ok_or("--home requires a path")?));
            }
            "--product-home" => {
                config.product_home = Some(PathBuf::from(
                    args.next().ok_or("--product-home requires a path")?,
                ));
            }
            "--threads" => threads = args.next().ok_or("--threads requires a count")?.parse()?,
            "--model-config" => {
                model_config = Some(PathBuf::from(
                    args.next().ok_or("--model-config requires a path")?,
                ))
            }
            "--mock-model" => mock_model = true,
            "--plugin-dir" => {
                plugin_dir = Some(PathBuf::from(
                    args.next().ok_or("--plugin-dir requires a path")?,
                ))
            }
            "--node" => node = Some(PathBuf::from(args.next().ok_or("--node requires a path")?)),
            "--js-runtime" => {
                js_runtime = Some(PathBuf::from(
                    args.next().ok_or("--js-runtime requires a path")?,
                ))
            }
            "--session-rows" => {
                session_rows = Some(PathBuf::from(
                    args.next().ok_or("--session-rows requires a path")?,
                ))
            }
            "--help" => {
                println!(
                    "dim-app-server (--product-home PATH | --model-config PATH | --mock-model) [--unix PATH | --pipe NAME] [--ws HOST:PORT | --no-ws] [--threads COUNT] [--home PATH | --database PATH] [--plugin-dir PATH] [--session-rows FILE] [--node PATH --js-runtime PATH]"
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    if (node.is_some() || js_runtime.is_some()) && plugin_dir.is_none() && session_rows.is_none() {
        return Err("--node and --js-runtime need --plugin-dir or --session-rows".into());
    }
    if let Some(plugin_dir) = plugin_dir {
        match (&node, &js_runtime) {
            (Some(node), Some(js_runtime)) if config.product_home.is_some() => {
                config.managed_plugin =
                    Some(dim_app_server::managed_provider::ManagedPluginConfig {
                        plugin_dir,
                        node: node.clone(),
                        js_runtime: js_runtime.clone(),
                    })
            }
            _ => {
                return Err(
                    "Managed plugins require --product-home, --plugin-dir, --node and --js-runtime"
                        .into(),
                );
            }
        }
    }
    if let Some(path) = session_rows {
        let (Some(node), Some(js_runtime)) = (node, js_runtime) else {
            return Err("--session-rows requires --node and --js-runtime".into());
        };
        // Package names in the rows resolve from the file's own location.
        let unreadable =
            |error: std::io::Error| format!("Cannot read Session rows {}: {error}", path.display());
        let anchor = std::fs::canonicalize(&path).map_err(unreadable)?;
        let bytes = std::fs::read(&anchor).map_err(unreadable)?;
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Invalid Session rows {} at line {}, column {}",
                path.display(),
                error.line(),
                error.column()
            )
        })?;
        config.session_typescript = Some(SessionTypeScriptConfig {
            node,
            js_runtime,
            anchor,
            rows,
            process_rows: Vec::new(),
        });
    }
    if threads == 0 {
        return Err("--threads must be positive".into());
    }
    if mock_model && model_config.is_some() {
        return Err("Cannot combine --model-config and --mock-model".into());
    }
    if config.product_home.is_some() && (mock_model || model_config.is_some()) {
        return Err("Cannot combine --product-home with a development model profile".into());
    }
    if !mock_model && model_config.is_none() && config.product_home.is_none() {
        return Err("Product home is required; use --product-home PATH, --model-config PATH or --mock-model".into());
    }
    if config.product_home.is_some() || model_config.is_some() {
        config.session_defaults.tools = [
            "read", "write", "edit", "glob", "grep", "exec", "wait", "skill", "branch", "agent",
            "ask_user",
        ]
        .map(String::from)
        .to_vec();
        config.session_defaults.permission_policy.preset =
            dim_agent_types::PermissionPreset::WorkspaceWrite;
    } else {
        // These permissions belong to the development host.
        config.session_defaults.permission_policy.allowed_tools =
            vec!["echo".into(), "wait".into()];
    }
    let models = if let Some(path) = model_config.as_deref() {
        let bytes = std::fs::read(path)?;
        let file: ModelConfigFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Invalid model configuration {} at line {}, column {}",
                path.display(),
                error.line(),
                error.column()
            )
        })?;
        let models = Arc::new(AimuxModels::from_config(file)?);
        use sha2::{Digest, Sha256};
        config.daemon_profile = dim_app_server::protocol::DaemonProfile::Configured {
            model_config_sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        Some(models)
    } else {
        config.daemon_profile = if config.product_home.is_some() {
            dim_app_server::protocol::DaemonProfile::Product
        } else {
            dim_app_server::protocol::DaemonProfile::Mock
        };
        None
    };
    let mut agent_config = AgentConfig::default();
    if let Some(models) = &models {
        config.model_catalog = Some(models.clone());
        agent_config.model = models.default_model().clone();
        config.session_defaults.model = dim_agent_types::ModelSelection {
            provider_id: models.default_model().provider_id.clone(),
            model_id: models.default_model().model_id.clone(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        };
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()?;
    let operation = async move {
        let mut builder = AgentBuilder::new(agent_config);
        if config.product_home.is_some() || models.is_some() {
            let product_home = config
                .product_home
                .as_ref()
                .cloned()
                .unwrap_or_else(|| std::env::temp_dir().join("dimcode"));
            let output_root = std::env::current_dir()?
                .join(
                    config
                        .target_home
                        .clone()
                        .or_else(|| {
                            config
                                .database_path
                                .as_ref()
                                .and_then(|path| path.parent().map(PathBuf::from))
                        })
                        .unwrap_or_else(|| product_home.join("rust")),
                )
                .join(dim_app_server::storage::COMMAND_OUTPUT_DIRECTORY);
            defaults::install_local_execution(
                &mut builder,
                ModelProviderPlugin::process,
                WorkspaceToolsConfig { output_root },
            );
            permission_rules::install(&mut builder);
            builder
                .session_value(Arc::new(HistoryRenderer::default()))
                .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))));
            if models.is_some() {
                // A --models file adopts no capability snapshot: no compaction.
                builder
                    .session_value(Arc::new(ContextModelHost(None)))
                    .session_value(Arc::new(CompactionStrategyHost(None)))
                    .session_plugin(ConfiguredModelPlugin::new);
            } else {
                builder
                    .session_value(Arc::new(ContextModelHost(Some(Arc::new(
                        ProductContextModel,
                    )))))
                    .session_plugin(ProductSummaryStrategyPlugin::new)
                    .session_plugin(ContextMethodsPlugin::new)
                    .session_methods(context_methods::NAMESPACE)
                    .session_plugin(DefaultPrepareModelPlugin::new);
            }
            collaboration::install(&mut builder);
            dim_agent_questions::install(&mut builder);
        } else if mock_model {
            defaults::install(&mut builder);
            defaults::mock_model_hosts(&mut builder);
        }
        if mock_model {
            defaults::tool_host(&mut builder, Arc::new(MockTools));
            collaboration::install(&mut builder);
        }
        let server = serve(builder, config).await?;
        println!(
            "{}",
            serde_json::json!({"unixPath": server.unix_path, "pipeName": server.pipe_name, "websocketAddress": server.websocket_address.map(|a| a.to_string())})
        );
        let shutdown = server.shutdown_signal();
        let join = server.join();
        tokio::pin!(join);
        tokio::select! {
            result = &mut join => result,
            signal = tokio::signal::ctrl_c() => { signal?; shutdown.cancel(); join.await },
            signal = terminate_signal() => { signal?; shutdown.cancel(); join.await }
        }
    };
    let reason = runtime.block_on(operation)?;
    if reason == ShutdownReason::Forced {
        std::process::exit(0);
    }
    Ok(())
}

async fn terminate_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
            .recv()
            .await;
        Ok(())
    }
    #[cfg(windows)]
    {
        tokio::signal::windows::ctrl_break()?.recv().await;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    std::future::pending().await
}
