use crate::{Result, export::Format, permission};
use dim_agent_types::PermissionPreset;
use std::path::PathBuf;

pub const HELP: &str = "dimcode [OPTIONS]\n\n\
Pure Rust scrollback client. With redirected stdin, run one prompt and exit.\n\n\
Commands:\n\
  tui                              Open the interactive scrollback client\n\
  exec [-p TEXT] [--image PATH ...] [--json] [--permission PRESET]\n\
                                   Run text and/or local images and exit\n\
  exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]\n\
  session export SESSION_ID --format markdown|json [--output PATH]\n\
  login PROVIDER --directory URL [--device] [--redirect-uri URI]\n\
  account PROVIDER                 Show non-secret account status\n\
  logout PROVIDER                  Sign out of an account provider\n\
  models PROVIDER [--refresh]       List model IDs and availability\n\
  mcp list                         List MCP servers and their enablement\n\
  mcp login ID [--no-browser] [--client-secret]\n\
                                   Sign in; secrets use hidden terminal input\n\
  mcp logout ID                    Sign out and report token revocation\n\
  mcp enable ID                    Enable an MCP server shown by mcp list\n\
  mcp disable ID                   Disable it; an unlisted ID withdraws only\n\
                                   this workspace's stored enablement\n\
  daemon stop                      Gracefully stop an existing daemon\n\n\
Options:\n\
  -p, --prompt TEXT                Run one prompt and exit\n\
  --image PATH                     Add a local image to exec (repeatable)\n\
  --resume SESSION_ID              Attach and continue a persisted session\n\
  --home PATH                      Product home (default: ~/.dimcode)\n\
  --cwd PATH                       Workspace (default: current directory)\n\
  --server PATH                    App Server binary (default: sibling binary)\n\
  --model-config PATH              Explicit API-key model configuration\n\
  --plugin-dir PATH                Managed account plugin package\n\
  --node PATH --js-runtime PATH    Fixed plugin runtime locations\n\
  --provider ID --model ID         Model selection for a new session\n\
  --permission PRESET              read-only, workspace-write or full-access for a\n\
                                   new session, or for the resumed main branch\n\
  -h, --help                       Show this help without starting a daemon\n\
  -V, --version                    Show version without starting a daemon\n\n\
Enter sends an idle draft; Ctrl+J inserts a newline; bracketed paste does not submit.\n\
Type @ for files, use Tab to select, or Ctrl+V for the local clipboard.\n\
Ctrl+C clears a nonempty idle draft; Ctrl+C or EOF exits only an empty draft.\n\
During terminal paste, Ctrl+C twice cancels that paste and keeps the draft.\n\
During execution, Ctrl+C requests stop and waits for settlement.\n\
Without -p, exec reads redirected stdin through EOF, including with --image.\n\
Idle MCP commands: /mcp, /mcp login [--no-browser] [--client-secret] ID, /mcp logout ID.\n\
Idle commands: /export <markdown|json> PATH, /exit.\n\
PATH is relative to the CLI current directory; ~ is literal.\n\
/approvals (alias /permissions) shows the preset and command rules; it also takes\n\
  PRESET, allow|deny exact|prefix COMMAND, or remove N.\n\
Approval accepts only y/yes; empty input denies. For a simple exec command, a/always\n\
  also saves a command rule. Noninteractive approval exits 3.\n\
Questions take option numbers (comma separated for several) or 0 to type your\n\
  own; empty input on the first question declines. Noninteractive questions exit 3.\n\
Exit codes: 0 success, 1 configuration/transport error, 2 model/turn failure,\n\
            3 interactive approval or answer required, 130 interrupted.\n";

#[derive(Debug)]
pub enum Action {
    Chat,
    Tui,
    Exec,
    Export {
        session_id: String,
        format: Format,
        output: Option<PathBuf>,
    },
    Login(String),
    Account(String),
    Logout(String),
    Models(String),
    McpList,
    McpLogin(String),
    McpLogout(String),
    McpEnable {
        id: String,
        enabled: bool,
    },
    DaemonStop,
    Help,
    Version,
}

pub struct Options {
    pub action: Action,
    pub prompt: Option<String>,
    pub images: Vec<PathBuf>,
    pub json: bool,
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
    pub permission: Option<PermissionPreset>,
    pub directory: Option<String>,
    pub device: bool,
    pub refresh: bool,
    pub redirect_uri: String,
    pub no_browser: bool,
    pub client_secret: bool,
}

impl Options {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self {
            action: Action::Chat,
            prompt: None,
            images: Vec::new(),
            json: false,
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
            permission: None,
            directory: None,
            device: false,
            refresh: false,
            redirect_uri: "http://localhost:1455/auth/callback".into(),
            no_browser: false,
            client_secret: false,
        };
        let mut args = args;
        let mut export_format = false;
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
                "-p" | "--prompt" => {
                    if options.prompt.replace(value(&mut args)?).is_some() {
                        return Err("Supply the prompt only once".into());
                    }
                }
                "--resume" => {
                    if options.resume.replace(value(&mut args)?).is_some() {
                        return Err("Supply the Session only once".into());
                    }
                }
                "--json" => options.json = true,
                "--image" => options.images.push(PathBuf::from(value(&mut args)?)),
                "--format" => {
                    let Action::Export { format, .. } = &mut options.action else {
                        return Err("--format requires session export".into());
                    };
                    if export_format {
                        return Err("Supply the export format only once".into());
                    }
                    *format = value(&mut args)?.parse()?;
                    export_format = true;
                }
                "--output" => {
                    let Action::Export { output, .. } = &mut options.action else {
                        return Err("--output requires session export".into());
                    };
                    if output.replace(PathBuf::from(value(&mut args)?)).is_some() {
                        return Err("Supply the output path only once".into());
                    }
                }
                "--home" => options.home = PathBuf::from(value(&mut args)?),
                "--cwd" => options.cwd = PathBuf::from(value(&mut args)?),
                "--server" => options.server = Some(PathBuf::from(value(&mut args)?)),
                "--model-config" => options.model_config = Some(PathBuf::from(value(&mut args)?)),
                "--plugin-dir" => options.plugin_dir = Some(PathBuf::from(value(&mut args)?)),
                "--node" => options.node = Some(PathBuf::from(value(&mut args)?)),
                "--js-runtime" => options.js_runtime = Some(PathBuf::from(value(&mut args)?)),
                "--provider" => options.provider = Some(value(&mut args)?),
                "--model" => options.model = Some(value(&mut args)?),
                "--permission" => {
                    let preset = permission::parse_preset(&value(&mut args)?)?;
                    if options.permission.replace(preset).is_some() {
                        return Err("Supply --permission only once".into());
                    }
                }
                "--directory" => options.directory = Some(value(&mut args)?),
                "--redirect-uri" => options.redirect_uri = value(&mut args)?,
                "--device" => options.device = true,
                "--refresh" => options.refresh = true,
                "--no-browser" => options.no_browser = true,
                "--client-secret" => options.client_secret = true,
                value if value.starts_with("--client-secret=") => return Err(
                    "Use --client-secret without a value; enter the secret at the hidden prompt"
                        .into(),
                ),
                "tui" if matches!(options.action, Action::Chat) => options.action = Action::Tui,
                "exec" if matches!(options.action, Action::Chat) => options.action = Action::Exec,
                "resume" if matches!(options.action, Action::Exec) => {
                    if options.resume.replace(value(&mut args)?).is_some() {
                        return Err("Supply the Session only once".into());
                    }
                }
                "session" if matches!(options.action, Action::Chat) => {
                    if value(&mut args)? != "export" {
                        return Err(
                            "Use session export SESSION_ID --format markdown|json [--output PATH]"
                                .into(),
                        );
                    }
                    options.action = Action::Export {
                        session_id: value(&mut args)?,
                        format: Format::Markdown,
                        output: None,
                    };
                }
                "mcp" if matches!(options.action, Action::Chat) => {
                    options.action = match value(&mut args)?.as_str() {
                        "list" => Action::McpList,
                        "login" => {
                            let id = loop {
                                match value(&mut args)?.as_str() {
                                    "--no-browser" => options.no_browser = true,
                                    "--client-secret" => options.client_secret = true,
                                    value if value.starts_with("--") => return Err("Use mcp login ID [--no-browser] [--client-secret]".into()),
                                    value => break value.to_owned(),
                                }
                            };
                            Action::McpLogin(id)
                        },
                        "logout" => Action::McpLogout(value(&mut args)?),
                        command @ ("enable" | "disable") => Action::McpEnable {
                            id: value(&mut args)?,
                            enabled: command == "enable",
                        },
                        _ => return Err("Use mcp list, mcp login ID, mcp logout ID, mcp enable ID or mcp disable ID".into()),
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
                _ if options.client_secret => return Err(
                    "Use --client-secret without a value; enter the secret at the hidden prompt"
                        .into(),
                ),
                _ => return Err(format!("Unknown argument: {arg}; use --help").into()),
            }
        }
        if options
            .prompt
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
            && options.images.is_empty()
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
        if !matches!(options.action, Action::Chat | Action::Tui | Action::Exec)
            && (options.prompt.is_some()
                || options.resume.is_some()
                || options.model.is_some()
                || options.permission.is_some())
        {
            return Err("Commands cannot be combined with chat options".into());
        }
        if matches!(options.action, Action::Tui) && options.prompt.is_some() {
            return Err("tui does not accept --prompt; use exec".into());
        }
        if options.json && !matches!(options.action, Action::Exec) {
            return Err("--json requires exec".into());
        }
        if !options.images.is_empty() && !matches!(options.action, Action::Exec) {
            return Err("--image requires exec".into());
        }
        if options.images.len() > 20 {
            return Err("At most 20 images may be submitted".into());
        }
        if matches!(options.action, Action::Export { .. }) && !export_format {
            return Err("session export requires --format markdown|json".into());
        }
        if options.refresh && !matches!(options.action, Action::Models(_)) {
            return Err("--refresh requires the models command".into());
        }
        if (options.no_browser || options.client_secret)
            && !matches!(options.action, Action::McpLogin(_))
        {
            return Err("--no-browser and --client-secret require mcp login ID".into());
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
