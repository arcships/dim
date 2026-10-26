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
  export SESSION_ID [--output PATH] [--database PATH]\n\
                                   Write a Session's whole Timeline as JSONL;\n\
                                   it holds prompts, code and tool output.\n\
                                   --database defaults to the home's\n\
                                   rust/dimcode.sqlite\n\
  login PROVIDER [--directory URL] [--device] [--redirect-uri URI]\n\
  account PROVIDER                 Show non-secret account status\n\
  logout PROVIDER                  Sign out of an account provider\n\
  models PROVIDER [--refresh | --clear-preferences MODEL]\n\
  mcp list                         List MCP servers and their enablement\n\
  mcp login ID [--no-browser] [--client-secret]\n\
                                   Sign in; secrets use hidden terminal input\n\
  mcp logout ID                    Sign out and report token revocation\n\
  mcp enable ID                    Enable an MCP server shown by mcp list\n\
  mcp disable ID                   Disable it; an unlisted ID withdraws only\n\
                                   this workspace's stored enablement\n\
  plugin list                      List plugins: built-in, default and installed\n\
  plugin install PATH|GIT-URL [--ref REF] [--id ID]\n\
                                   Install a plugin directory; it runs at once.\n\
                                   A local path is used in place\n\
  plugin enable ID | plugin disable ID\n\
  plugin reload ID                 Bring in a plugin's edited code and content\n\
  plugin update ID [--yes]         Move a Git plugin to its branch's newest commit\n\
  plugin remove ID                 Remove an installed plugin\n\
  daemon stop                      Gracefully stop an existing daemon\n\
  web [--port N] [--no-open]       Hold the Web workspace until Ctrl+C
web --off                        Turn off the daemon-owned Web switch
web install                      Install this version's Web workspace\n\
Options:\n\
  -p, --prompt TEXT                Run one prompt and exit\n\
  --image PATH                     Add a local image to exec (repeatable)\n\
  --resume SESSION_ID              Attach and continue a persisted session\n\
  --home PATH                      Product home (default: ~/.dimcode)\n\
  --cwd PATH                       Workspace (default: current directory)\n\
  --no-tui                         Line client instead of the terminal UI\n\
  --fullscreen | --scrollback      Terminal UI mode; append 'default' to save it\n\
  --server PATH                    App Server binary (default: this executable)\n\
  --model-config PATH              Explicit API-key model configuration\n\
  --provider ID --model ID         Model selection for a new session\n\
  --permission PRESET              read-only, workspace-write or full-access for a\n\
                                   new session, or for the resumed main branch\n\
  -h, --help                       Show this help without starting a daemon\n\
  -V, --version                    Show version without starting a daemon\n\n\
Ctrl+C stops a running turn and waits for settlement; idle Ctrl+C or EOF exits.\n\
Without -p, exec reads redirected stdin through EOF, including with --image.\n\
Idle commands: /export <markdown|json> PATH, /exit.\n\
PATH is relative to the CLI current directory; ~ is literal.\n\
/approvals (alias /permissions) shows the preset and command rules; it also takes\n\
  PRESET, allow|deny exact|prefix COMMAND, or remove N.\n\
Approval accepts only y/yes; empty input denies. For a simple exec command, a/always\n\
  also saves a command rule. Noninteractive approval exits 3.\n\
The terminal UI offers a newer version on its npm channel (alpha, beta or latest),\n\
  installs it with the package manager it came from and restarts onto the session;\n\
  DIMCODE_UPDATE_CHECK=0 skips that check.\n\
The terminal UI reports the run's state to the terminal (OSC 7501);\n\
  DIM_TERMINAL_STATUS=off turns that off.\n\
It also draws a progress bar where the terminal is known to (OSC 9;4);\n\
  DIM_TERMINAL_PROGRESS=off turns that off, =on draws it in any terminal.\n\
Questions take option numbers (comma separated for several) or 0 to type your\n\
  own; empty input on the first question declines. Noninteractive questions exit 3.\n\
Exit codes: 0 success, 1 configuration/transport error, 2 model/turn failure,\n\
            3 interactive approval or answer required, 130 interrupted.\n";

#[derive(Clone, Debug)]
pub enum Action {
    Chat,
    Tui,
    Exec,
    Export {
        session_id: String,
        format: Format,
        output: Option<PathBuf>,
    },
    /// `export`: the Session's whole Timeline (ADR 005 D3).
    Timeline {
        session_id: String,
        output: Option<PathBuf>,
        database: Option<PathBuf>,
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
    Plugin(Plugin),
    DaemonStop,
    /// `web`: hold the daemon's local web entry open in the foreground.
    Web {
        port: Option<u16>,
        open_browser: bool,
        /// `--off`: clear the daemon-owned switch and exit.
        off: bool,
    },
    /// `web install`: this version's Web workspace into `<home>/web`.
    WebInstall,
    /// `--fullscreen default` / `--scrollback default`: save the terminal
    /// UI mode used when neither flag is given.
    DefaultTerminalMode {
        fullscreen: bool,
    },
    Help,
    Version,
}

/// The registration a hosted leaf `Tui` is (Tui design §3.6).
#[derive(Clone, Debug)]
pub struct UiIdentity {
    pub ui_id: String,
    pub token: String,
}

/// A `plugin` subcommand (PluginManager §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    List,
    Install {
        source: String,
        reference: Option<String>,
        id: Option<String>,
    },
    SetEnabled {
        id: String,
        enabled: bool,
    },
    Reload(String),
    Update {
        id: String,
        confirmed: bool,
    },
    Remove(String),
}

#[derive(Clone)]
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
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permission: Option<PermissionPreset>,
    pub directory: Option<String>,
    pub device: bool,
    /// Set only for a hosted leaf `Tui`.
    pub ui_identity: Option<UiIdentity>,
    /// The hosted leaf's Session was created by this invocation, so `resume`
    /// names a new Session rather than one the user chose to resume.
    pub created_session: bool,
    /// Use the scrollback client instead of the full terminal UI.
    pub no_tui: bool,
    /// Terminal UI mode for this run; `None` uses the saved default.
    pub fullscreen: Option<bool>,
    pub refresh: bool,
    pub clear_preferences: Option<String>,
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
            home: default_home(),
            cwd: std::env::current_dir()?,
            server: None,
            model_config: None,
            provider: None,
            model: None,
            permission: None,
            directory: None,
            device: false,
            ui_identity: None,
            created_session: false,
            no_tui: false,
            fullscreen: None,
            refresh: false,
            clear_preferences: None,
            // The redirect address registered for Dim account sign-in.
            redirect_uri: "http://localhost:54321/auth/callback".into(),
            no_browser: false,
            client_secret: false,
        };
        let mut args = args.peekable();
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
                    let (Action::Export { output, .. } | Action::Timeline { output, .. }) =
                        &mut options.action
                    else {
                        return Err("--output requires session export or export".into());
                    };
                    if output.replace(PathBuf::from(value(&mut args)?)).is_some() {
                        return Err("Supply the output path only once".into());
                    }
                }
                "--database" => {
                    let Action::Timeline { database, .. } = &mut options.action else {
                        return Err("--database requires export".into());
                    };
                    if database.replace(PathBuf::from(value(&mut args)?)).is_some() {
                        return Err("Supply the database path only once".into());
                    }
                }
                "--home" => options.home = PathBuf::from(value(&mut args)?),
                "--cwd" => options.cwd = PathBuf::from(value(&mut args)?),
                "--server" => options.server = Some(PathBuf::from(value(&mut args)?)),
                "--model-config" => options.model_config = Some(PathBuf::from(value(&mut args)?)),
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
                "--no-tui" => options.no_tui = true,
                "--fullscreen" | "--scrollback" => {
                    let fullscreen = arg == "--fullscreen";
                    if args.peek().is_some_and(|next| next == "default") {
                        args.next();
                        options.action = Action::DefaultTerminalMode { fullscreen };
                    } else {
                        options.fullscreen = Some(fullscreen);
                    }
                }
                "--refresh" => options.refresh = true,
                "--clear-preferences" => {
                    if options
                        .clear_preferences
                        .replace(value(&mut args)?)
                        .is_some()
                    {
                        return Err("Supply the model to clear only once".into());
                    }
                }
                "--no-browser" => options.no_browser = true,
                "--client-secret" => options.client_secret = true,
                value if value.starts_with("--client-secret=") => {
                    return Err(
                        "Use --client-secret without a value; enter the secret at the hidden prompt"
                            .into(),
                    );
                }
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
                "export" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Timeline {
                        session_id: value(&mut args)?,
                        output: None,
                        database: None,
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
                                    value if value.starts_with("--") => {
                                        return Err(
                                            "Use mcp login ID [--no-browser] [--client-secret]"
                                                .into(),
                                        );
                                    }
                                    value => break value.to_owned(),
                                }
                            };
                            Action::McpLogin(id)
                        }
                        "logout" => Action::McpLogout(value(&mut args)?),
                        command @ ("enable" | "disable") => Action::McpEnable {
                            id: value(&mut args)?,
                            enabled: command == "enable",
                        },
                        _ => {
                            return Err("Use mcp list, mcp login ID, mcp logout ID, mcp enable ID or mcp disable ID".into());
                        }
                    };
                }
                "plugin" if matches!(options.action, Action::Chat) => {
                    let command = match value(&mut args)?.as_str() {
                        "list" => Plugin::List,
                        "install" => {
                            let source = value(&mut args)?;
                            let (mut reference, mut id) = (None, None);
                            while let Some(flag) =
                                args.next_if(|next| next == "--ref" || next == "--id")
                            {
                                let slot = if flag == "--ref" {
                                    &mut reference
                                } else {
                                    &mut id
                                };
                                if slot.replace(value(&mut args)?).is_some() {
                                    return Err(format!("Supply {flag} only once").into());
                                }
                            }
                            Plugin::Install {
                                source,
                                reference,
                                id,
                            }
                        }
                        command @ ("enable" | "disable") => Plugin::SetEnabled {
                            id: value(&mut args)?,
                            enabled: command == "enable",
                        },
                        "reload" => Plugin::Reload(value(&mut args)?),
                        "update" => {
                            let id = value(&mut args)?;
                            let confirmed = args.next_if(|next| next == "--yes").is_some();
                            Plugin::Update { id, confirmed }
                        }
                        "remove" => Plugin::Remove(value(&mut args)?),
                        _ => {
                            return Err("Use plugin list, plugin install PATH|GIT-URL [--ref REF] [--id ID], or plugin enable|disable|reload|update|remove ID".into());
                        }
                    };
                    options.action = Action::Plugin(command);
                }
                "web" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Web {
                        port: None,
                        open_browser: true,
                        off: false,
                    };
                }
                "install"
                    if matches!(
                        options.action,
                        Action::Web {
                            port: None,
                            open_browser: true,
                            off: false
                        }
                    ) =>
                {
                    options.action = Action::WebInstall;
                }
                "--port" if matches!(options.action, Action::Web { .. }) => {
                    let value = value(&mut args)?;
                    if let Action::Web { port, .. } = &mut options.action {
                        *port = Some(
                            value
                                .parse()
                                .map_err(|_| format!("Invalid port: {value}"))?,
                        );
                    }
                }
                "--no-open" if matches!(options.action, Action::Web { .. }) => {
                    if let Action::Web { open_browser, .. } = &mut options.action {
                        *open_browser = false;
                    }
                }
                "--off" if matches!(options.action, Action::Web { .. }) => {
                    if let Action::Web { off, .. } = &mut options.action {
                        *off = true;
                    }
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
                _ if options.client_secret => {
                    return Err(
                        "Use --client-secret without a value; enter the secret at the hidden prompt"
                            .into(),
                    );
                }
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
        if options.images.len() > crate::attachments::MAX_IMAGES {
            return Err("At most 20 images may be submitted".into());
        }
        if matches!(options.action, Action::Export { .. }) && !export_format {
            return Err("session export requires --format markdown|json".into());
        }
        if options.clear_preferences.is_some()
            && (!matches!(options.action, Action::Models(_)) || options.refresh)
        {
            return Err("--clear-preferences requires models PROVIDER and cannot be combined with --refresh".into());
        }
        if options.refresh && !matches!(options.action, Action::Models(_)) {
            return Err("--refresh requires the models command".into());
        }
        if (options.no_browser || options.client_secret)
            && !matches!(options.action, Action::McpLogin(_))
        {
            return Err("--no-browser and --client-secret require mcp login ID".into());
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
        Ok(options)
    }
}

/// The product home: `DIMCODE_HOME`, else `~/.dimcode`.
pub fn default_home() -> PathBuf {
    std::env::var_os("DIMCODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".dimcode"))
                .unwrap_or_default()
        })
}
