mod args;
mod attachments;
mod browser;
mod client;
mod clipboard;
mod composer;
mod configure;
mod display;
mod export;
mod goal;
mod goal_command;
mod images;
mod input;
mod leaf;
mod login;
mod maintenance;
mod management;
mod management_command;
mod mcp;
mod mcp_command;
mod member_approval;
mod migration_command;
mod model;
mod permission;
mod permission_command;
mod plugin_command;
mod plugins;
mod projection;
mod question;
mod recording;
mod sanitize;
mod session;
mod session_command;
mod session_host;
mod ui;
mod update;
mod usage;
mod version;
mod web_install;
mod wire;

use args::{Action, Options};
use rutis::host::CtxHost;
use std::io::{IsTerminal, Read};
use std::sync::Arc;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;

/// Runs the CLI with the process arguments and returns its exit code. The
/// host binary must answer `app-server` with the App Server command line,
/// which the CLI starts as its daemon unless `--server` names another.
pub fn run_cli() -> i32 {
    let result = prepare().and_then(|options| {
        let Some(options) = options else { return Ok(0) };
        let acp = matches!(
            options.action,
            Action::Acp {
                auth_setup: None,
                ..
            }
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(run(options));
        // Owned plugin work has drained; blocked OS stdio must not hold CLI exit.
        if acp {
            runtime.shutdown_background();
        }
        result
    });
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {}", crate::sanitize::safe(&error.to_string()));
            1
        }
    }
}

/// The composition root: builds the client tree, loads what the action needs
/// and disposes the tree before the exit code goes back. rutis reports
/// nothing to the terminal: the CLI hears its plugins' failures where it
/// awaits them, and the terminal may belong to the TUI.
async fn run(options: Options) -> Result<i32> {
    let root = rutis::Ctx::root_with_sink(tokio::runtime::Handle::current(), Arc::new(|_| {}));
    let outcome = act(&root, options).await;
    let closed = root.shutdown().await;
    let code = outcome?;
    closed?;
    Ok(code)
}

/// Loads `DaemonLink` into `root` and returns it with the connection it
/// provides.
async fn daemon_link(
    root: &rutis::Ctx,
    options: &Options,
    start: bool,
) -> Result<(rutis::host::FiberView, Arc<plugins::Connection>)> {
    let link = root.plugin(plugins::DaemonLink {
        options: options.clone(),
        start,
    });
    (&link).await.map_err(plugin_error)?;
    let connection = root
        .get::<plugins::Connection>()
        .ok_or("DaemonLink provided no connection")?;
    Ok((link, connection))
}

/// Runs the ordinary TUI plugin directly, for an older daemon without the
/// `ui.leaf.2` capability and for initial model setup. The caller
/// owns the terminal; the daemon is only the protocol v3 business peer.
async fn run_tui_with_connection(
    root: &rutis::Ctx,
    options: &Options,
    link: rutis::host::FiberView,
    _connection: Arc<plugins::Connection>,
) -> Result<i32> {
    let env = ui::Env {
        home: options.home.clone(),
        workspace: options.cwd.clone(),
        fullscreen: options
            .fullscreen
            .unwrap_or_else(|| ui::config::saved_fullscreen(&options.home)),
    };
    let (leave, left) = tokio::sync::oneshot::channel();
    let tui = root.plugin(plugins::Tui {
        env,
        options: options.clone(),
        leave: std::sync::Mutex::new(Some(leave)),
    });
    (&tui).await.map_err(plugin_error)?;
    let leave = left
        .await
        .map_err(|_| "the terminal interface ended without a result")?;
    tui.dispose().await?;
    link.dispose().await?;
    match leave {
        ui::tui::Leave::Exit(code) => Ok(code),
        ui::tui::Leave::Restart(restart) => relaunch(root, options, restart).await,
    }
}

/// The interactive entry on a daemon that keeps UI leaves (Tui design
/// §3.6): this process creates or resumes the Session, mounts its leaf
/// node, asks the daemon to open a session with it, then runs its own
/// `Tui`. It ends when the `Tui` leaves or the session ends.
async fn open_tui_leaf(root: &rutis::Ctx, options: &Options) -> Result<i32> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Created {
        session_id: String,
    }

    let (link, connection) = daemon_link(root, options, true).await?;
    if !connection.offers("ui.leaf.2") {
        return run_tui_with_connection(root, options, link, connection).await;
    }
    let mut client = connection.take().ok_or("App Server did not start")?;
    link.dispose().await?;
    let session_id = if let Some(id) = &options.resume {
        if let Some(preset) = options.permission {
            permission::select(&mut client, id, preset)
                .await
                .map_err(|error| format!("Cannot set the permission preset: {error}"))?;
        }
        client
            .rpc("session.attach", serde_json::json!({ "sessionId": id }))
            .await?;
        id.clone()
    } else {
        let mut params = serde_json::json!({ "cwd": options.cwd });
        let mut preferences = None;
        let mut target = None;
        if let (Some(provider), Some(model)) = (&options.provider, &options.model) {
            params["model"] = serde_json::json!({
                "provider": provider,
                "modelId": model,
            });
            preferences =
                session_host::saved_model_preferences(&mut client, provider, model).await?;
            if let Some(preferences) = &preferences {
                params["modelPreferences"] = preferences.clone();
            }
            target = Some((provider.clone(), model.clone()));
        }
        if let Some(policy) = permission::creation_policy(options) {
            params["permissionPolicy"] = policy;
        }
        let created: Created = match client.typed("session.create", params).await {
            Ok(created) => created,
            Err(error)
                if options.provider.is_none()
                    && options.model_config.is_none()
                    && client::app_error(&error) == Some("provider_failed") =>
            {
                drop(client);
                let (link, connection) = daemon_link(root, options, false).await?;
                return run_tui_with_connection(root, options, link, connection).await;
            }
            Err(error) => {
                let error = model_adoption_error(
                    &mut client,
                    target.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                    error,
                    preferences.as_ref(),
                )
                .await;
                return Err(format!(
                    "Cannot create session: {error}. Configure a provider or use --model-config PATH with an API-key environment reference"
                ).into());
            }
        };
        client
            .rpc(
                "session.attach",
                serde_json::json!({ "sessionId": created.session_id }),
            )
            .await?;
        created.session_id
    };

    let leaf = leaf::Leaf::bind(options)?;
    let params = serde_json::json!({
        "sessionId": session_id,
        "kind": "tui",
        "peer": leaf.peer,
        "address": leaf.address,
    });
    let (node, ended) = leaf.node();
    let node = root.plugin(node);
    (&node).await.map_err(plugin_error)?;
    let opened = client.typed::<leaf::Opened>("ui.open", params).await;
    drop(client);
    let opened = match opened {
        Ok(opened) => opened,
        Err(error) => {
            node.dispose().await?;
            return Err(error);
        }
    };
    let (leave, left) = tokio::sync::oneshot::channel();
    let tui = root.plugin(leaf::LeafTui::new(options, session_id, opened, leave));
    if let Err(error) = (&tui).await {
        let _ = tui.dispose().await;
        node.dispose().await?;
        return Err(plugin_error(error));
    }
    let leave = tokio::select! {
        leave = left => leave.ok(),
        _ = ended => None,
    };
    tui.dispose().await?;
    node.dispose().await?;
    match leave {
        Some(ui::tui::Leave::Exit(code)) => Ok(code),
        Some(ui::tui::Leave::Restart(restart)) => relaunch(root, options, restart).await,
        None => Ok(0),
    }
}

/// Starts the updated CLI on the session the interface left. The daemon
/// still runs the old executable, so it stops first and the new CLI starts
/// its own. The original arguments carry over except the interface mode,
/// which follows the one in use, and the session, which is the one left;
/// a resumed session keeps its model, so the model flags drop with it.
async fn relaunch(root: &rutis::Ctx, options: &Options, restart: ui::tui::Restart) -> Result<i32> {
    let (link, connection) = daemon_link(root, options, false).await?;
    if let Some(client) = connection.take() {
        client::shutdown(client).await?;
    }
    link.dispose().await?;
    let mut args = Vec::new();
    let mut original = std::env::args_os().skip(1);
    while let Some(arg) = original.next() {
        let resumed = restart.session_id.is_some();
        match arg.to_str() {
            Some("--fullscreen" | "--scrollback") => {}
            Some("--resume") => {
                original.next();
            }
            Some("--provider" | "--model") if resumed => {
                original.next();
            }
            _ => args.push(arg),
        }
    }
    args.push(
        (if restart.fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        })
        .into(),
    );
    if let Some(session_id) = restart.session_id {
        args.extend(["--resume".into(), session_id.into()]);
    }
    let mut command = std::process::Command::new(&restart.relaunch);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(format!("Cannot start {}: {error}", restart.relaunch.display()).into())
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|error| format!("Cannot start {}: {error}", restart.relaunch.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// Links the daemon and loads `WebHold`; returns when the hold ends.
async fn hold_web(
    root: &rutis::Ctx,
    options: &Options,
    port: Option<u16>,
    open_browser: bool,
) -> Result<i32> {
    daemon_link(root, options, true).await?;
    let (done, held) = tokio::sync::oneshot::channel();
    let hold = root.plugin(plugins::WebHold {
        port,
        open_browser,
        home: options.home.clone(),
        done: std::sync::Mutex::new(Some(done)),
    });
    (&hold).await.map_err(plugin_error)?;
    held.await
        .map_err(|_| "the Web workspace hold ended without a result")?
}

/// `dimcode web --off`: clear the daemon-owned switch without disconnecting a
/// foreground holder.
async fn disable_web(root: &rutis::Ctx, options: &Options) -> Result<i32> {
    let (link, connection) = daemon_link(root, options, false).await?;
    let Some(mut client) = connection.take() else {
        println!("Daemon is not running.");
        let _ = link.dispose().await;
        return Ok(0);
    };
    #[derive(serde::Deserialize)]
    struct Status {
        open: bool,
    }
    let status: Status = client.typed("web.disable", serde_json::json!({})).await?;
    println!(
        "{}",
        if status.open {
            "Web switch off; the entry is still held by another client."
        } else {
            "Web switch off."
        }
    );
    let _ = link.dispose().await;
    Ok(0)
}

/// A failed load as the plugin reported it, without the framework's framing.
fn plugin_error(error: Arc<rutis::host::ErrorKind>) -> Error {
    match &*error {
        rutis::host::ErrorKind::PluginFailed(source) => source.to_string().into(),
        other => other.to_string().into(),
    }
}

/// The failure of adopting `target` into a Session. When the target's
/// catalog entry reports missing limits, the declaration command is appended;
/// the catalog is read only on this failure path, never from the message.
pub(crate) async fn model_adoption_error(
    client: &mut client::Client,
    target: Option<(&str, &str)>,
    error: Error,
    preferences: Option<&serde_json::Value>,
) -> Error {
    let error: Error = match preferences {
        Some(preferences) => format!("Model adoption with saved preferences {preferences} failed: {error}. Clear them with Ctrl+X in /models or dimcode models PROVIDER --clear-preferences MODEL; saved preferences were kept.").into(),
        None => error,
    };
    let Some((provider_id, model_id)) = target else {
        return error;
    };
    let Some(hint) = declaration_hint(client, provider_id, model_id).await else {
        return error;
    };
    format!("{error} {hint}").into()
}

/// The command that declares both limits of one model, when its catalog
/// entry says no source declares them.
async fn declaration_hint(
    client: &mut client::Client,
    provider_id: &str,
    model_id: &str,
) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Snapshot {
        catalog: Option<Catalog>,
    }
    #[derive(serde::Deserialize)]
    struct Catalog {
        models: Vec<CatalogModel>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CatalogModel {
        id: String,
        #[serde(default)]
        missing_limits: Vec<String>,
    }
    let snapshot: Snapshot = client
        .typed(
            "provider.catalog",
            serde_json::json!({"providerId": provider_id}),
        )
        .await
        .ok()?;
    let undeclared = snapshot.catalog.is_some_and(|catalog| {
        catalog
            .models
            .into_iter()
            .any(|model| model.id == model_id && !model.missing_limits.is_empty())
    });
    undeclared.then(|| {
        crate::ui::i18n::tf(
            "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N.",
            &[("provider", provider_id), ("model", model_id)],
        )
    })
}

/// A model this crate marks `undeclared`: it has no declared limit and no
/// other reason to be unavailable, so declaring the limits is the one thing
/// that unblocks it. A disabled model or one unavailable for another reason
/// keeps that state. `dimcode models` and the `/models` picker share this
/// predicate for both the mark and the count.
pub(crate) fn limits_undeclared(
    missing_limits: &[String],
    unavailable_reason: Option<&str>,
    disabled: bool,
) -> bool {
    !disabled
        && !missing_limits.is_empty()
        && unavailable_reason.is_none_or(|reason| reason == "limits_undeclared")
}

fn prepare() -> Result<Option<Options>> {
    let mut options = Options::parse(std::env::args().skip(1))?;
    if matches!(options.action, Action::Help) {
        print!("{}", args::HELP);
        return Ok(None);
    }
    if matches!(options.action, Action::Version) {
        println!("dimcode {}", version::current());
        return Ok(None);
    }
    if matches!(options.action, Action::McpLogin(_)) && !std::io::stdin().is_terminal() {
        return Err(
            "MCP sign-in requires a terminal; run dimcode mcp login ID interactively".into(),
        );
    }
    if matches!(options.action, Action::Tui) && !std::io::stdin().is_terminal() {
        return Err("tui requires an interactive terminal".into());
    }
    if matches!(options.action, Action::Exec)
        && options.prompt.is_none()
        && options.images.is_empty()
        && std::io::stdin().is_terminal()
    {
        return Err("exec requires --prompt TEXT, --image PATH or redirected stdin".into());
    }
    if let Action::DefaultTerminalMode { fullscreen } = options.action {
        if let Some(error) = ui::config::save_fullscreen(&options.home, fullscreen) {
            return Err(error.into());
        }
        let mode = if fullscreen {
            "fullscreen"
        } else {
            "scrollback"
        };
        println!("Default terminal mode: {mode}");
        return Ok(None);
    }
    if matches!(options.action, Action::Chat | Action::Exec)
        && options.prompt.is_none()
        && !std::io::stdin().is_terminal()
    {
        let mut prompt = String::new();
        std::io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut prompt)?;
        if prompt.len() > 16 * 1024 * 1024 {
            return Err("stdin prompt exceeds 16 MiB".into());
        }
        if prompt.trim().is_empty() && options.images.is_empty() {
            return Err("Provide --prompt TEXT, piped stdin, or an interactive terminal".into());
        }
        if !prompt.is_empty() {
            options.prompt = Some(prompt);
        }
    }
    Ok(Some(options))
}

async fn run_acp(root: &rutis::Ctx, options: &Options, agent_id: Option<String>) -> Result<i32> {
    let mut signals = plugins::Signals::listen()?;
    let client = client::connect_inner(options, true)
        .await?
        .ok_or("App Server did not start")?;
    let (gateway, finished) = dim_acp::AcpGateway::new(
        client,
        dim_acp::Io {
            reader: Box::new(tokio::io::stdin()),
            writer: Box::new(tokio::io::stdout()),
        },
        agent_id,
        version::current(),
    );
    let directory = options
        .directory
        .clone()
        .unwrap_or_else(|| login::account_directory().into());
    let mut terminal_args = vec![
        "acp".into(),
        "--redirect-uri".into(),
        options.redirect_uri.clone(),
    ];
    for (flag, path) in [
        ("--home", Some(&options.home)),
        ("--server", options.server.as_ref()),
        ("--model-config", options.model_config.as_ref()),
    ] {
        if let Some(path) = path {
            terminal_args.extend([
                flag.into(),
                std::path::absolute(path)?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "ACP terminal authentication requires UTF-8 paths")?,
            ]);
        }
    }
    terminal_args.extend(["--directory".into(), directory.clone()]);
    let gateway = gateway.login(dim_acp::auth::Login {
        directory,
        redirect_uri: options.redirect_uri.clone(),
        open_browser: if std::env::var("DIMCODE_OAUTH_MANUAL").is_ok_and(|value| value == "1") {
            None
        } else {
            Some(browser::open)
        },
        terminal_args: Some(terminal_args),
    });
    let gateway = root.plugin(gateway);
    (&gateway).await.map_err(plugin_error)?;
    let (code, result) = tokio::select! {
        result = finished => (0, result.map_err(|_| "ACP gateway ended without a result")?),
        () = signals.received() => (130, Ok(())),
    };
    gateway.dispose().await?;
    result?;
    Ok(code)
}

async fn act(root: &rutis::Ctx, mut options: Options) -> Result<i32> {
    // `block_on` polls `act` on this thread, so the thread-local language holds.
    crate::ui::i18n::set(crate::ui::i18n::interface_locale(&options.home));
    if let Action::Migration(command) = &options.action {
        return migration_command::run(root, &options, command).await;
    }
    if let Action::Acp {
        auth_setup: Some(method),
        ..
    } = &options.action
    {
        let mut client = client::connect_inner(&options, true)
            .await?
            .ok_or("App Server did not start")?;
        let (provider, device) = dim_acp::auth::terminal_method(&mut client, method).await?;
        options.device = device;
        return login::login(&mut client, &options, Some(&provider)).await;
    }
    if let Action::Acp { agent_id, .. } = &options.action {
        return run_acp(root, &options, agent_id.clone()).await;
    }
    if matches!(options.action, Action::Chat | Action::Tui)
        && options.prompt.is_none()
        && !options.no_tui
        && ui::tui::is_interactive()
    {
        if options.permission.is_none() {
            options.permission = ui::config::saved_permission(&options.home)
                .and_then(|preset| permission::parse_preset(&preset).ok());
            options.file_access = ui::config::read(&options.home, "fileAccess")
                .and_then(|value| serde_json::from_value(value).ok());
            options.tool_approval = ui::config::read(&options.home, "toolApproval")
                .and_then(|value| serde_json::from_value(value).ok());
        }
        return open_tui_leaf(root, &options).await;
    }
    if matches!(options.action, Action::WebInstall) {
        if !version::RELEASE {
            return Err("A build from source has no published Web workspace; set DIMCODE_WEB_ASSETS to a frontend built from this source, or build the App Server with the embedded-web feature".into());
        }
        let executable = std::env::current_exe().unwrap_or_default();
        let assets = client::web_assets(&executable, &options.home);
        return web_install::install(
            &options.home,
            version::VERSION,
            &update::registry(),
            assets,
            &mut std::io::stdout(),
        )
        .await;
    }
    if let Action::Web {
        port,
        open_browser,
        off,
    } = options.action
    {
        if off {
            return disable_web(root, &options).await;
        }
        return hold_web(root, &options, port, open_browser).await;
    }

    if let Action::Timeline {
        session_id,
        output,
        database,
    } = &options.action
    {
        export::timeline(&options, session_id, output.as_deref(), database.as_deref()).await?;
        return Ok(0);
    }
    if let Action::DaemonVacuum { database } = &options.action {
        maintenance::vacuum(&options, database.as_deref()).await?;
        return Ok(0);
    }
    let images = images::load(&options.images)?;
    let stop = matches!(options.action, Action::DaemonStop);
    let (_, connection) = daemon_link(root, &options, !stop).await?;
    if stop {
        return client::stop(connection.take()).await;
    }
    let mut client = connection.take().ok_or("App Server did not start")?;
    match &options.action {
        Action::Export {
            session_id,
            format,
            output,
        } => {
            export::write(&mut client, session_id, *format, output.as_deref()).await?;
            Ok(0)
        }
        Action::SessionRename {
            session_id,
            title,
            clear,
        } => {
            let title = if *clear { None } else { title.as_deref() };
            session_command::rename(&mut client, session_id, title).await?;
            Ok(0)
        }
        Action::SessionDelete { session_id } => {
            session_command::delete(&mut client, session_id).await?;
            Ok(0)
        }
        Action::SessionCleanup {
            older_than,
            keep,
            yes,
        } => session_command::cleanup(&mut client, *older_than, *keep, *yes).await,
        Action::Login(provider) => login::login(&mut client, &options, provider.as_deref()).await,
        Action::Account(provider) => login::account(&mut client, provider).await,
        Action::Logout(provider) => login::logout(&mut client, provider).await,
        Action::Models(provider) => {
            if let Some(model) = &options.clear_preferences {
                session_host::clear_saved_model_preferences(&mut client, provider, model).await?;
                println!("{}", crate::ui::i18n::t("Saved model preferences cleared"));
                return Ok(0);
            }
            if options.set_limits.is_some() || options.set_missing_limits {
                let (Some(context_window), Some(max_output_tokens)) =
                    (options.context_window, options.max_output_tokens)
                else {
                    return Err(
                        "--set-limits and --set-missing-limits require --context-window and --max-output-tokens"
                            .into(),
                    );
                };
                let target = match &options.set_limits {
                    Some(model) => session_host::LimitTarget::Model(model.clone()),
                    None => session_host::LimitTarget::Missing,
                };
                session_host::declare_model_limits(
                    &mut client,
                    provider,
                    target,
                    context_window,
                    max_output_tokens,
                )
                .await?;
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Catalog {
                models: Vec<Model>,
                disabled_model_ids: Vec<String>,
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Model {
                id: String,
                name: String,
                unavailable_reason: Option<String>,
                #[serde(default)]
                missing_limits: Vec<String>,
            }
            let params = serde_json::json!({"providerId":provider});
            let catalog = if options.refresh {
                Some(client.typed::<Catalog>("provider.refresh", params).await?)
            } else {
                #[derive(serde::Deserialize)]
                struct Snapshot {
                    catalog: Option<Catalog>,
                }
                client
                    .typed::<Snapshot>("provider.catalog", params)
                    .await?
                    .catalog
            };
            if let Some(catalog) = catalog {
                if catalog.models.is_empty() {
                    let notice = if options.refresh {
                        crate::ui::i18n::t("No models. The provider listed none.").to_owned()
                    } else {
                        crate::ui::i18n::tf(
                            "No models. Run dimcode models {id} --refresh to list them.",
                            &[("id", provider)],
                        )
                    };
                    eprintln!("{notice}");
                }
                // Undeclared limits are the only blocker of a model this
                // command can fix, so those are the ones it marks and names
                // the declaring command for; a disabled model or one
                // unavailable for another reason keeps that state.
                let mut undeclared = 0;
                for model in catalog.models {
                    let disabled = catalog.disabled_model_ids.contains(&model.id);
                    let limits_only = limits_undeclared(
                        &model.missing_limits,
                        model.unavailable_reason.as_deref(),
                        disabled,
                    );
                    if limits_only {
                        undeclared += 1;
                    }
                    let availability = if disabled {
                        "disabled"
                    } else if limits_only {
                        "undeclared"
                    } else if model.unavailable_reason.is_some() {
                        "unavailable"
                    } else {
                        "available"
                    };
                    println!(
                        "{}\t{}\t{availability}",
                        crate::sanitize::safe(&model.id).replace(['\n', '\t'], " "),
                        crate::sanitize::safe(&model.name).replace(['\n', '\t'], " "),
                    );
                }
                if undeclared > 0 {
                    eprintln!(
                        "{}",
                        crate::ui::i18n::tf(
                            "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N.",
                            &[("id", provider)],
                        )
                    );
                }
            } else {
                eprintln!(
                    "{}",
                    crate::ui::i18n::t(
                        "No cached catalog. Sign in and run models PROVIDER --refresh."
                    )
                );
            }
            Ok(0)
        }
        Action::Plugin { method, params } => {
            plugin_command::run(&mut client, method, params.clone()).await
        }
        Action::RuntimeList => {
            runtime_list(&mut client).await?;
            Ok(0)
        }
        Action::RuntimeInstall(name) => {
            let status: serde_json::Value = client
                .typed("runtime.install", serde_json::json!({ "name": name }))
                .await?;
            println!("{}", runtime_line(&status));
            Ok(0)
        }
        Action::RuntimeConfig { name, source, auto } => {
            let mut params = serde_json::json!({ "name": name });
            if let Some(source) = source {
                params["source"] = serde_json::json!(source);
            }
            if let Some(auto) = auto {
                params["auto"] = serde_json::json!(auto);
            }
            let status: serde_json::Value = client.typed("runtime.configure", params).await?;
            println!("{}", runtime_line(&status));
            Ok(0)
        }
        Action::McpList => {
            mcp_command::list(&mut client, &options).await?;
            Ok(0)
        }
        Action::McpLogin(id) => {
            let mut input = input::Input::new()?;
            mcp_command::sign_in(
                &mut client,
                &options,
                &mut input,
                id,
                options.no_browser,
                options.client_secret,
            )
            .await
        }
        Action::McpLogout(id) => mcp_command::sign_out(&mut client, &options, id).await,
        Action::McpEnable { id, enabled } => {
            let (rows, notes) =
                mcp_command::set_enabled(&mut client, &options, id, *enabled).await?;
            mcp_command::print_report(rows, notes);
            Ok(0)
        }
        Action::Management(request) => {
            management_command::run(&mut client, &options, request).await
        }
        Action::ManagementEdit { area, id } => {
            management_command::edit(&mut client, &options, *area, id.clone()).await
        }
        Action::ManagementSet { area, id, path } => {
            management_command::set(&mut client, &options, *area, id.clone(), path).await
        }
        Action::ManagementAdd { area, path } => {
            management_command::add(&mut client, &options, *area, path).await
        }
        Action::Recording { enabled } => recording::run(&mut client, &options, *enabled).await,
        Action::Chat | Action::Tui | Action::Exec => {
            session::chat(&mut client, &options, images.blocks, images.notes).await
        }
        Action::Acp { .. }
        | Action::Migration(_)
        | Action::Help
        | Action::Version
        | Action::DaemonStop
        | Action::DaemonVacuum { .. }
        | Action::Timeline { .. }
        | Action::Web { .. }
        | Action::WebInstall
        | Action::DefaultTerminalMode { .. } => unreachable!(),
    }
}

/// One `runtime.list` entry as a tab-separated line.
fn runtime_line(status: &serde_json::Value) -> String {
    let auto = status["auto"]
        .as_bool()
        .map(|on| if on { "on" } else { "off" });
    ["name", "source"]
        .iter()
        .filter_map(|field| status[field].as_str())
        .chain(auto)
        .chain(
            ["version", "status", "reason"]
                .iter()
                .filter_map(|field| status[field].as_str()),
        )
        .map(|value| crate::sanitize::safe(value).replace(['\n', '\t'], " "))
        .collect::<Vec<_>>()
        .join("\t")
}

async fn runtime_list(client: &mut client::Client) -> Result<()> {
    let list: Vec<serde_json::Value> = client.typed("runtime.list", serde_json::json!({})).await?;
    for status in &list {
        println!("{}", runtime_line(status));
    }
    Ok(())
}
