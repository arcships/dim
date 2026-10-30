//! The App Server command line, shared by `dim-app-server` and `dimcode app-server`.

use crate::{
    ServerConfig, ShutdownReason, collaboration, permission_rules,
    runtime_manager::Runtimes,
    serve,
    session_typescript::{PythonConfig, SessionTypeScriptConfig},
};
use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    context_model::ProductContextModel,
    defaults,
    history::HistoryRenderer,
    host::{IdentityContext, PrepareContextHost},
    model_provider::ModelProviderPlugin,
    summary_strategy::ProductSummaryStrategyConfig,
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

/// Reads a development model configuration, `--model-config PATH`, as the
/// profile's model catalog with the profile hash a client checks it by.
pub fn model_profile(
    path: &std::path::Path,
) -> Result<(Arc<AimuxModels>, crate::protocol::DaemonProfile), Box<dyn std::error::Error>> {
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
    let profile = crate::protocol::DaemonProfile::Configured {
        model_config_sha256: {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(&bytes))
        },
    };
    Ok((models, profile))
}

/// The Session defaults a product home and a development model configuration
/// serve: `tools` unset, so a Session declares every host tool and every tool
/// a part registers (#364), under the workspace-write preset. Every caller
/// that serves this assembly takes them from here, so the two cannot drift.
pub fn product_session_defaults(defaults: &mut dim_agent_runtime::SessionConfig) {
    defaults.tools = None;
    defaults.permission_policy.preset = dim_agent_types::PermissionPreset::WorkspaceWrite;
}

/// [`run`] with a managed provider plugin, which an account build's own
/// executable supplies; it is installed when the server has a product home.
pub fn run_with(
    args: impl IntoIterator<Item = String>,
    managed_provider: Option<crate::managed_provider::ManagedProviderRow>,
) -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = args.into_iter().collect();
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
    let mut session_rows = None;
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
            "--session-rows" => {
                session_rows = Some(PathBuf::from(
                    args.next().ok_or("--session-rows requires a path")?,
                ))
            }
            "--defaults" => {
                config.plugin_defaults = Some(std::fs::canonicalize(
                    args.next().ok_or("--defaults requires a path")?,
                )?)
            }
            "--python" => {
                python = Some(PathBuf::from(
                    args.next().ok_or("--python requires a path")?,
                ))
            }
            "--help" => {
                println!(
                    "dim-app-server (--product-home PATH | --model-config PATH | --mock-model) [--unix PATH | --pipe NAME] [--ws HOST:PORT | --no-ws] [--threads COUNT] [--home PATH | --database PATH] [--defaults FILE] [--session-rows FILE] [--node PATH --js-runtime PATH] [--python PATH] [--web-assets PATH]\n\
                     dim-app-server --export-timeline SESSION_ID --database PATH"
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    let anchor = session_rows
        .clone()
        .or_else(|| config.plugin_defaults.clone());
    if (node.is_some() || js_runtime.is_some() || python.is_some()) && anchor.is_none() {
        return Err("--node, --js-runtime and --python need --session-rows or --defaults".into());
    }
    if config.product_home.is_some() {
        config.managed_provider = managed_provider;
    }
    if config.web_assets.is_none() {
        config.web_assets = config.product_home.as_ref().map(|home| home.join("web"));
    }
    if node.is_some() != js_runtime.is_some() || (python.is_some() && node.is_none()) {
        return Err("--node and --js-runtime go together, and --python needs them".into());
    }
    if session_rows.is_some() && (node.is_none() || js_runtime.is_none()) {
        return Err("--session-rows requires --node and --js-runtime".into());
    }
    if let (Some(path), Some(node), Some(js_runtime)) = (anchor, node, js_runtime) {
        let unreadable =
            |error: std::io::Error| format!("Cannot read Session rows {}: {error}", path.display());
        let anchor = std::fs::canonicalize(&path).map_err(unreadable)?;
        let bytes = std::fs::read(&anchor).map_err(unreadable)?;
        let rows: Vec<serde_json::Value> = if session_rows.is_some() {
            serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "Invalid Session rows {} at line {}, column {}",
                    path.display(),
                    error.line(),
                    error.column()
                )
            })?
        } else {
            vec![]
        };
        let python = python.map(|interpreter| PythonConfig {
            interpreter,
            project: anchor.parent().expect("a file has a directory").to_owned(),
            packages: None,
        });
        config.session_typescript = Some(SessionTypeScriptConfig {
            node,
            js_runtime,
            anchor,
            python,
            acquire: None,
            rows,
        });
    } else if let Some(defaults) = &config.plugin_defaults {
        let home = config
            .product_home
            .as_ref()
            .or(config.target_home.as_ref())
            .ok_or("--defaults needs --product-home or --home")?;
        let home = std::path::absolute(home)?;
        let preset = crate::plugin_manager::Defaults::read(defaults)?.runtimes;
        let runtimes = Arc::new(Runtimes::load(&home, &preset)?);
        config.session_typescript =
            Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![]));
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
        product_session_defaults(&mut config.session_defaults);
    } else {
        config.session_defaults.permission_policy.allowed_tools =
            vec!["echo".into(), "wait".into()];
    }
    let models = if let Some(path) = model_config.as_deref() {
        let (models, profile) = model_profile(path)?;
        config.daemon_profile = profile;
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
                dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
            );
            permission_rules::install(&mut builder);
            builder
                .session_value(Arc::new(HistoryRenderer::default()))
                .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))));
            if let Some(models) = &models {
                defaults::install_compaction(
                    &mut builder,
                    models.clone(),
                    ProductSummaryStrategyConfig::default(),
                );
                dim_agent_aimux::install(&mut builder);
            } else {
                defaults::install_compaction(
                    &mut builder,
                    Arc::new(ProductContextModel),
                    ProductSummaryStrategyConfig::default(),
                );
                builder.require_feature(
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
