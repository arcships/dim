#![allow(dead_code)] // TODO: remove once the TUI is wired to the App Server

mod args;
mod browser;
mod client;
mod display;
mod event_order;
mod input;
mod json_ord;
mod login;
mod model;
mod projection;
mod session;
mod session_host;
mod ui;
mod usage;
mod wire;

use args::{Action, Options};
use std::io::{IsTerminal, Read};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;

fn main() {
    let result = prepare().and_then(|options| {
        let Some(options) = options else { return Ok(0) };
        if matches!(options.action, Action::Chat)
            && options.prompt.is_none()
            && !options.no_tui
            && ui::tui::is_interactive()
        {
            let env = ui::Env {
                home: options.home.clone(),
                workspace: options.cwd.clone(),
                fullscreen: options
                    .fullscreen
                    .unwrap_or_else(|| ui::config::saved_fullscreen(&options.home)),
            };
            let host = session_host::SessionHost::start(options);
            return Ok(ui::tui::run(Box::new(host), env));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(run(options))
    });
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Error: {}", display::safe(&error.to_string()));
            std::process::exit(1);
        }
    }
}

fn prepare() -> Result<Option<Options>> {
    let mut options = Options::parse(std::env::args().skip(1))?;
    if matches!(options.action, Action::Help) {
        print!("{}", args::HELP);
        return Ok(None);
    }
    if matches!(options.action, Action::Version) {
        println!("dimcode {}", env!("CARGO_PKG_VERSION"));
        return Ok(None);
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
    if matches!(options.action, Action::Chat)
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
        if prompt.trim().is_empty() {
            return Err("Provide --prompt TEXT, piped stdin, or an interactive terminal".into());
        }
        options.prompt = Some(prompt);
    }
    Ok(Some(options))
}

async fn run(options: Options) -> Result<i32> {
    if matches!(options.action, Action::DaemonStop) {
        return client::stop(&options).await;
    }
    let mut client = client::connect(&options).await?;
    match &options.action {
        Action::Login(provider) => login::login(&mut client, &options, provider).await,
        Action::Account(provider) => {
            #[derive(serde::Deserialize)]
            struct Snapshot {
                account: Option<Identity>,
            }
            #[derive(serde::Deserialize)]
            struct Identity {
                issuer: String,
                subject: String,
            }
            let snapshot: Snapshot = client
                .typed("account.status", serde_json::json!({"providerId":provider}))
                .await?;
            match snapshot.account {
                Some(account) => println!(
                    "Signed in: {} ({})",
                    display::safe(&account.subject),
                    display::safe(&account.issuer)
                ),
                None => println!("Signed out."),
            }
            Ok(0)
        }
        Action::Logout(provider) => {
            client
                .rpc("account.logout", serde_json::json!({"providerId":provider}))
                .await?;
            println!("Signed out.");
            Ok(0)
        }
        Action::Models(provider) => {
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
                for model in catalog.models {
                    let availability = if catalog.disabled_model_ids.contains(&model.id) {
                        "disabled"
                    } else if model.unavailable_reason.is_some() {
                        "unavailable"
                    } else {
                        "available"
                    };
                    println!(
                        "{}\t{}\t{availability}",
                        display::safe(&model.id).replace(['\n', '\t'], " "),
                        display::safe(&model.name).replace(['\n', '\t'], " "),
                    );
                }
            } else {
                eprintln!("No cached catalog. Sign in and run models PROVIDER --refresh.");
            }
            Ok(0)
        }
        Action::McpList => {
            #[derive(serde::Deserialize)]
            struct Catalog {
                servers: Vec<Server>,
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Server {
                id: String,
                source: String,
                enabled: bool,
                transport: String,
                approval: String,
                approval_token: Option<String>,
            }
            let catalog: Catalog = client
                .typed("mcp.catalog", serde_json::json!({"cwd":options.cwd}))
                .await?;
            for server in catalog.servers {
                // Debug escaping preserves IDs on one line, including C1 and
                // bidi controls that JSON permits as literal Unicode characters.
                println!(
                    "{:?}\t{}\t{}\t{}\t{}\t{}",
                    server.id,
                    display::safe(&server.source),
                    display::safe(&server.transport),
                    if server.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    display::safe(&server.approval),
                    display::safe(server.approval_token.as_deref().unwrap_or("-"))
                );
            }
            eprintln!(
                "Columns: ID, source, transport, enablement, approval, fingerprint.\nBefore approving, inspect this workspace's .mcp.json and the code or endpoint it runs. Pass the fingerprint from this listing; approval may start discovery in active sessions."
            );
            Ok(0)
        }
        Action::McpApprove { id, token } => {
            client
                .rpc(
                    "mcp.approve",
                    serde_json::json!({"cwd":options.cwd,"id":id,"approvalToken":token}),
                )
                .await?;
            println!("Project MCP configuration approved. Active sessions may now connect.");
            Ok(0)
        }
        Action::McpRevoke(id) => {
            client
                .rpc("mcp.revoke", serde_json::json!({"cwd":options.cwd,"id":id}))
                .await?;
            println!(
                "Project MCP approval revoked and connections closed. Earlier external effects are not undone."
            );
            Ok(0)
        }
        Action::Chat => session::chat(&mut client, &options).await,
        Action::Help
        | Action::Version
        | Action::DaemonStop
        | Action::DefaultTerminalMode { .. } => unreachable!(),
    }
}
