//! The App Server command line, shared by `dim-app-server` and `dimcode app-server`.

use crate::{
    ServerConfig, ShutdownReason, collaboration, permission_rules, serve,
    session_typescript::{PythonConfig, SessionTypeScriptConfig},
};
use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    context_methods::ContextMethodsPlugin,
    context_model::ProductContextModel,
    defaults,
    history::HistoryRenderer,
    host::{CompactionStrategyHost, ContextModelHost, IdentityContext, PrepareContextHost},
    model_provider::ModelProviderPlugin,
    summary_strategy::ProductSummaryStrategyPlugin,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

/// Runs the App Server command line: `args` are the arguments after the
/// program name. Both the `dim-app-server` binary and `dimcode app-server`
/// enter here.
pub fn run(args: impl IntoIterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    run_with(args, None)
}

/// [`run`] with a managed provider plugin, which an account build's own
/// executable supplies; it is installed when the server has a product home.
pub fn run_with(
    args: impl IntoIterator<Item = String>,
    managed_provider: Option<crate::managed_provider::ManagedProviderRow>,
) -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = args.into_iter().collect();
    // The client transport does not acquire daemon locks or load product state.
    if argv.first().map(String::as_str) == Some("--connect-pipe") {
        let mut args = argv.iter().skip(1);
        let name = args.next().ok_or("--connect-pipe requires a name")?;
        if args.next().is_some() {
            return Err("--connect-pipe cannot be combined with server options".into());
        }
        #[cfg(windows)]
        return crate::pipe_client::run(name).map_err(Into::into);
        #[cfg(not(windows))]
        {
            let _ = name;
            return Err("--connect-pipe requires Windows".into());
        }
    }
    // The Timeline export reads one database read-only and loads no product
    // state; `dimcode export` runs it.
    if argv.first().map(String::as_str) == Some("--export-timeline") {
        let [session, flag, database] = &argv[1..] else {
            return Err("Use --export-timeline SESSION_ID --database PATH".into());
        };
        if flag != "--database" {
            return Err("Use --export-timeline SESSION_ID --database PATH".into());
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        runtime.block_on(crate::storage::export_timeline(
            PathBuf::from(database),
            session,
            &mut out,
        ))?;
        return Ok(());
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
    let mut node = None;
    let mut js_runtime = None;
    let mut plugin_defaults = None;
    let mut python = None;
    let mut args = argv.into_iter();
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
            "--web-assets" => {
                config.web_assets = Some(PathBuf::from(
                    args.next().ok_or("--web-assets requires a path")?,
                ))
            }
            "--threads" => threads = args.next().ok_or("--threads requires a count")?.parse()?,
            "--model-config" => {
                model_config = Some(PathBuf::from(
                    args.next().ok_or("--model-config requires a path")?,
                ))
            }
            "--mock-model" => mock_model = true,
            "--node" => node = Some(PathBuf::from(args.next().ok_or("--node requires a path")?)),
            "--js-runtime" => {
                js_runtime = Some(PathBuf::from(
                    args.next().ok_or("--js-runtime requires a path")?,
                ))
            }
            "--plugin-defaults" => {
                plugin_defaults = Some(PathBuf::from(
                    args.next().ok_or("--plugin-defaults requires a path")?,
                ))
            }
            "--python" => {
                python = Some(PathBuf::from(
                    args.next().ok_or("--python requires a path")?,
                ))
            }
            "--help" => {
                println!(
                    "dim-app-server (--product-home PATH | --model-config PATH | --mock-model) [--unix PATH | --pipe NAME] [--ws HOST:PORT | --no-ws] [--threads COUNT] [--home PATH | --database PATH] [--plugin-defaults FILE] [--node PATH --js-runtime PATH] [--python PATH] [--web-assets PATH]\n\
                     dim-app-server --export-timeline SESSION_ID --database PATH"
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    if node.is_some() != js_runtime.is_some() {
        return Err("--node and --js-runtime go together".into());
    }
    if python.is_some() && node.is_none() {
        return Err("--python needs --node and --js-runtime".into());
    }
    if config.product_home.is_some() {
        config.managed_provider = managed_provider;
    }
    // Without `--web-assets`, the product home's `web/`, where `dimcode web
    // install` puts the frontend; the entry looks at it on each open
    // (web-workspace.md §2.4).
    if config.web_assets.is_none() {
        config.web_assets = config.product_home.as_ref().map(|home| home.join("web"));
    }
    // The default config file: its plugins' rows and which rows start
    // disabled or are locked (PluginManager §7).
    let (rows, defaults) = match &plugin_defaults {
        Some(path) => crate::plugins::DefaultsFile::read(path)?,
        None => (vec![], Default::default()),
    };
    config.plugin_defaults = defaults;
    if let (Some(node), Some(js_runtime)) = (node, js_runtime) {
        // Package names in rows resolve from the default config file; the
        // rows of installed plugins name absolute entries.
        let anchor = match &plugin_defaults {
            Some(path) => std::fs::canonicalize(path)?,
            None => std::fs::canonicalize(js_runtime.join("package.json"))?,
        };
        // `py:` modules are imported from the anchor's directory and from
        // each Python plugin's directory.
        let python = python.map(|interpreter| PythonConfig {
            interpreter,
            project: anchor.parent().expect("a file has a directory").to_owned(),
        });
        config.session_typescript = Some(SessionTypeScriptConfig {
            node,
            js_runtime,
            anchor,
            python,
            rows,
        });
    } else if !rows.is_empty() {
        return Err("plugins with code need --node and --js-runtime".into());
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
        // Product Sessions declare every tool the process provides.
        config.session_defaults.tools = None;
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
        config.daemon_profile = crate::protocol::DaemonProfile::Configured {
            model_config_sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        Some(models)
    } else {
        config.daemon_profile = if config.product_home.is_some() {
            crate::protocol::DaemonProfile::Product
        } else {
            crate::protocol::DaemonProfile::Mock
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
                .join(crate::storage::COMMAND_OUTPUT_DIRECTORY);
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
                    .session_value(Arc::new(CompactionStrategyHost(None)));
                dim_agent_aimux::install(&mut builder);
            } else {
                builder
                    .session_value(Arc::new(ContextModelHost(Some(Arc::new(
                        ProductContextModel,
                    )))))
                    .require_feature(
                        dim_agent_runtime::Feature::new("ContextCompaction")
                            .session("ProductSummaryStrategy", |names| {
                                Ok(Box::new(ProductSummaryStrategyPlugin::new(names)?))
                            })
                            .session("ContextMethods", |names| {
                                Ok(Box::new(ContextMethodsPlugin::new(names)?))
                            }),
                    )
                    .require_feature(
                        dim_agent_runtime::Feature::new("PrepareModelHost")
                            .session("PrepareModelHost", |names| {
                                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                            }),
                    );
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
