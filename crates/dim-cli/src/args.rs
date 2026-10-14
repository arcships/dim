use crate::Result;
use std::path::PathBuf;

pub const HELP: &str = "dimcode [OPTIONS]\n\n\
Pure Rust scrollback client. With redirected stdin, run one prompt and exit.\n\n\
Commands:\n\
  login PROVIDER --directory URL [--device] [--redirect-uri URI]\n\
  account PROVIDER                 Show non-secret account status\n\
  logout PROVIDER                  Sign out of an account provider\n\
  models PROVIDER [--refresh]       List model IDs and availability\n\
  mcp list                         List MCP servers and enablement\n\
  daemon stop                      Gracefully stop an existing daemon\n\n\
Options:\n\
  -p, --prompt TEXT                Run one prompt and exit\n\
  --resume SESSION_ID              Attach and continue a persisted session\n\
  --home PATH                      Product home (default: ~/.dimcode)\n\
  --cwd PATH                       Workspace (default: current directory)\n\
  --server PATH                    App Server binary (default: sibling binary)\n\
  --model-config PATH              Explicit API-key model configuration\n\
  --plugin-dir PATH                Managed account plugin package\n\
  --node PATH --js-runtime PATH    Fixed plugin runtime locations\n\
  --provider ID --model ID         Model selection for a new session\n\
  -h, --help                       Show this help without starting a daemon\n\
  -V, --version                    Show version without starting a daemon\n\n\
Ctrl+C stops a running turn and waits for settlement; idle Ctrl+C or EOF exits.\n\
Approval accepts only y/yes; empty input denies. Noninteractive approval exits 3.\n\
Exit codes: 0 success, 1 configuration/transport error, 2 model/turn failure,\n\
            3 interactive approval required, 130 interrupted.\n";

#[derive(Debug)]
pub enum Action {
    Chat,
    Login(String),
    Account(String),
    Logout(String),
    Models(String),
    McpList,
    DaemonStop,
    Help,
    Version,
}

pub struct Options {
    pub action: Action,
    pub prompt: Option<String>,
    pub resume: Option<String>,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub server: Option<PathBuf>,
    pub model_config: Option<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
    pub node: Option<PathBuf>,
    pub js_runtime: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub directory: Option<String>,
    pub device: bool,
    pub refresh: bool,
    pub redirect_uri: String,
}

impl Options {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self {
            action: Action::Chat,
            prompt: None,
            resume: None,
            home: std::env::var_os("DIMCODE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                        .map(|home| PathBuf::from(home).join(".dimcode"))
                        .unwrap_or_default()
                }),
            cwd: std::env::current_dir()?,
            server: None,
            model_config: None,
            plugin_dir: None,
            node: None,
            js_runtime: None,
            provider: None,
            model: None,
            directory: None,
            device: false,
            refresh: false,
            redirect_uri: "http://localhost:1455/auth/callback".into(),
        };
        let mut args = args;
        while let Some(arg) = args.next() {
            let value = |args: &mut dyn Iterator<Item = String>| -> Result<String> {
                args.next()
                    .ok_or_else(|| format!("{arg} requires a value").into())
            };
            match arg.as_str() {
                "-h" | "--help" => {
                    options.action = Action::Help;
                    return Ok(options);
                }
                "-V" | "--version" => {
                    options.action = Action::Version;
                    return Ok(options);
                }
                "-p" | "--prompt" => options.prompt = Some(value(&mut args)?),
                "--resume" => options.resume = Some(value(&mut args)?),
                "--home" => options.home = PathBuf::from(value(&mut args)?),
                "--cwd" => options.cwd = PathBuf::from(value(&mut args)?),
                "--server" => options.server = Some(PathBuf::from(value(&mut args)?)),
                "--model-config" => options.model_config = Some(PathBuf::from(value(&mut args)?)),
                "--plugin-dir" => options.plugin_dir = Some(PathBuf::from(value(&mut args)?)),
                "--node" => options.node = Some(PathBuf::from(value(&mut args)?)),
                "--js-runtime" => options.js_runtime = Some(PathBuf::from(value(&mut args)?)),
                "--provider" => options.provider = Some(value(&mut args)?),
                "--model" => options.model = Some(value(&mut args)?),
                "--directory" => options.directory = Some(value(&mut args)?),
                "--redirect-uri" => options.redirect_uri = value(&mut args)?,
                "--device" => options.device = true,
                "--refresh" => options.refresh = true,
                "mcp" if matches!(options.action, Action::Chat) => {
                    options.action = match value(&mut args)?.as_str() {
                        "list" => Action::McpList,
                        _ => return Err("Use mcp list".into()),
                    };
                }
                "daemon" if matches!(options.action, Action::Chat) => {
                    if value(&mut args)? != "stop" {
                        return Err("Use daemon stop".into());
                    }
                    options.action = Action::DaemonStop;
                }
                "login" | "account" | "logout" | "models"
                    if matches!(options.action, Action::Chat) =>
                {
                    let provider = value(&mut args)?;
                    options.action = match arg.as_str() {
                        "login" => Action::Login(provider),
                        "account" => Action::Account(provider),
                        "logout" => Action::Logout(provider),
                        _ => Action::Models(provider),
                    };
                }
                _ => return Err(format!("Unknown argument: {arg}; use --help").into()),
            }
        }
        if options
            .prompt
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        {
            return Err("Prompt cannot be empty".into());
        }
        if options.home.as_os_str().is_empty() {
            return Err("Set --home PATH when the user home environment variable is absent".into());
        }
        if options.provider.is_some() != options.model.is_some() {
            return Err("Use --provider and --model together".into());
        }
        let plugin_options = [&options.plugin_dir, &options.node, &options.js_runtime];
        if plugin_options.iter().any(|value| value.is_some())
            && plugin_options.iter().any(|value| value.is_none())
        {
            return Err("Use --plugin-dir, --node and --js-runtime together".into());
        }
        if options.model_config.is_some() && options.plugin_dir.is_some() {
            return Err("--model-config cannot be combined with account plugin options".into());
        }
        if options.resume.is_some() && options.model.is_some() {
            return Err("A resumed session retains its model; omit --provider and --model".into());
        }
        if !matches!(options.action, Action::Chat)
            && (options.prompt.is_some() || options.resume.is_some() || options.model.is_some())
        {
            return Err("Commands cannot be combined with chat options".into());
        }
        if options.refresh && !matches!(options.action, Action::Models(_)) {
            return Err("--refresh requires the models command".into());
        }
        if matches!(options.action, Action::Login(_)) && options.directory.is_none() {
            return Err("login requires --directory URL".into());
        }
        if !matches!(options.action, Action::Login(_))
            && (options.directory.is_some() || options.device)
        {
            return Err("Login options require the login command".into());
        }
        options.cwd = std::fs::canonicalize(options.cwd)?;
        if !options.cwd.is_dir() {
            return Err("--cwd must name a directory".into());
        }
        // Stopping an existing daemon does not consume model or plugin files.
        if matches!(options.action, Action::DaemonStop) {
            return Ok(options);
        }
        for path in [&mut options.model_config, &mut options.server]
            .into_iter()
            .flatten()
        {
            *path = std::fs::canonicalize(&*path)?;
            if !path.is_file() {
                return Err("--server and --model-config must name regular files".into());
            }
        }
        for path in [
            &mut options.plugin_dir,
            &mut options.node,
            &mut options.js_runtime,
        ]
        .into_iter()
        .flatten()
        {
            *path = std::fs::canonicalize(&*path)?;
        }
        Ok(options)
    }
}
