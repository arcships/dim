use crate::{Result, export::Format, permission};
use dim_agent_types::PermissionPreset;
use std::path::PathBuf;

/// What `--server` must name, shared by every error about the value it
/// supplies.
pub(crate) const SERVER_REQUIREMENT: &str = "--server must name a standalone dim-app-server binary";

pub const HELP: &str = "dimcode [OPTIONS]\n\n\
Pure Rust scrollback client. With redirected stdin, run one prompt and exit.\n\n\
Commands:\n\
  tui                              Open the interactive scrollback client\n\
  acp [--agent ID]                  Serve ACP over stdin/stdout\n\
  exec [-p TEXT] [--image PATH ...] [--json] [--permission PRESET]\n\
                                   Run text and/or local images and exit\n\
  exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]\n\
  session export SESSION_ID --format markdown|json [--output PATH]\n\
  session rename SESSION TITLE     Set the manual title; --clear removes it\n\
  session delete SESSION           Permanently delete a Session\n\
  session cleanup (--older-than DURATION | --keep N) [--yes]\n\
                                   Show the Sessions that will go, then\n\
                                   delete them after confirmation\n\
  export SESSION_ID [--output PATH] [--database PATH]\n\
                                   Write a Session's whole Timeline as JSONL;\n\
                                   it holds prompts, code and tool output.\n\
                                   --database defaults to the home's\n\
                                   rust/dimcode.sqlite\n\
  login [PROVIDER] [--directory URL] [--device] [--redirect-uri URI]\n\
                                   Sign in to a Provider the daemon describes;\n\
                                   without PROVIDER, list the accounts it describes\n\
  account PROVIDER                 Show non-secret account status\n\
  logout PROVIDER                  Sign out of an account provider\n\
  models PROVIDER [--refresh | --clear-preferences MODEL\n\
                  | --set-limits MODEL --context-window N --max-output-tokens N\n\
                  | --set-missing-limits --context-window N --max-output-tokens N]\n\
  mcp list                         List MCP servers and their enablement\n\
  mcp login ID [--no-browser] [--client-secret]\n\
                                   Sign in; secrets use hidden terminal input\n\
  mcp logout ID                    Sign out and report token revocation\n\
  mcp enable ID                    Enable an MCP server shown by mcp list\n\
  mcp disable ID                   Disable it; an unlisted ID withdraws only\n\
                                   this workspace's stored enablement\n\
  skills list                      List Skills, their sources and states\n\
  skills enable|disable ID         Turn one Skill on or off\n\
  skills reload                    Rescan this workspace's Skills\n\
  hooks list                       List hook handlers, their sources and states\n\
  hooks enable                     Run this workspace's .dimcode/hooks.json\n\
  hooks disable                    Stop running this workspace's hooks\n\
  hooks edit                       Edit the user's hook document\n\
  hooks set PATH                   Write the user's hook document from PATH,\n\
                                   or from stdin for -\n\
  recording [on|off]               Record model requests to the home's\n\
                                   model-log/recordings.jsonl; prompts, code\n\
                                   and tool output are written there\n\
  plugin list                      List plugins by name, the one in use and its status\n\
  plugin install PATH|GIT_URL [--ref R] [--id ID] [--no-enable]\n\
                                   Install a plugin and turn it on\n\
  plugin enable NAME [--use ID|builtin|default]\n\
  plugin disable NAME\n\
  plugin reload NAME               Reload a plugin after editing it\n\
  plugin update ID                 Load the latest commit of a Git plugin\n\
  plugin remove ID\n\
  runtime list                     List the plugin runtimes and their state\n\
  runtime install node|python      Turn a plugin runtime on and acquire it\n\
  daemon stop                      Gracefully stop an existing daemon\n\
  daemon vacuum [--database PATH]  Rewrite an offline database to reclaim\n\
                                   space; --database defaults to the home's\n\
                                   rust/dimcode.sqlite\n\
  migration [run|inspect] [--source OLD_HOME]\n\
                                   Import or inspect Global and CN separately;\n\
                                   Global stays in ~/.dim, CN in ~/.dimcode;\n\
                                   confirm stopped clients or use --source-stopped\n\
  migration list|show TASK|cancel TASK\n\
                                   Read tasks or request cancellation\n\
  migration retry TASK [--source OLD_HOME]\n\
                                   Resume a task, reconciling imported objects\n\
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
  --server PATH                    Run a standalone dim-app-server binary;\n\
                                   the default runs this executable's\n\
                                   app-server subcommand\n\
  --model-config PATH              Explicit API-key model configuration\n\
  --provider ID --model ID         Model selection for a new session\n\
  --permission PRESET              read-only, workspace-write or full-access for a\n\
                                   new session, or for the resumed main branch\n\
  -h, --help                       Show this help without starting a daemon\n\
  -V, --version                    Show version without starting a daemon\n\n\
Ctrl+C stops a running turn and waits for settlement; idle Ctrl+C or EOF exits.\n\
Without -p, exec reads redirected stdin through EOF, including with --image.\n\
Idle commands: /export <markdown|json> PATH, /rename TITLE|--clear, /exit.\n\
PATH is relative to the CLI current directory; ~ is literal.\n\
/permissions (aliases /approvals, /approval) opens separate File access, Tool\n\
  approval (Ask/Bypass), and Process isolation controls in the terminal UI.\n\
The line prompt accepts file-access read-only|workspace-read-write|unrestricted,\n\
  tool-approval ask|bypass, PRESET, allow|deny exact|prefix COMMAND, or remove N.\n\
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
Account commands sign in to every Provider the daemon describes with a login\n\
  method. A build without the Dim account provider says so and offers API-key\n\
  Providers instead.\n\
Exit codes: 0 success, 1 configuration/transport error, a delete that removed\n\
            nothing or a cleanup in which a delete failed, 2 model/turn\n\
            failure, 3 interactive approval or answer required, 130\n\
            interrupted.\n";

/// What `dimcode session` names besides `export`.
const USAGE_SESSION: &str =
    "Use session export, session rename, session delete or session cleanup; see --help";

#[derive(Clone, Debug)]
pub enum Action {
    Chat,
    Tui,
    Exec,
    Migration(crate::migration_command::Command),
    Acp {
        agent_id: Option<String>,
        auth_setup: Option<String>,
    },
    Export {
        session_id: String,
        format: Format,
        output: Option<PathBuf>,
    },
    /// `session rename`: `title` sets the manual title, `clear` sends `null`.
    SessionRename {
        session_id: String,
        title: Option<String>,
        clear: bool,
    },
    /// `session delete`: permanently delete the named Session.
    SessionDelete {
        session_id: String,
    },
    /// `session cleanup`: delete the Sessions one selector names.
    SessionCleanup {
        /// Sessions last active at least this long ago.
        older_than: Option<std::time::Duration>,
        /// The newest Sessions to keep, leaving the rest to delete.
        keep: Option<usize>,
        /// Delete without asking.
        yes: bool,
    },
    /// `export`: the Session's whole Timeline (ADR 005 D3).
    Timeline {
        session_id: String,
        output: Option<PathBuf>,
        database: Option<PathBuf>,
    },
    /// `daemon vacuum`: rewrite an offline database to reclaim space (#600).
    DaemonVacuum {
        database: Option<PathBuf>,
    },
    /// `login [PROVIDER]`: without one, list the described accounts.
    Login(Option<String>),
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
    /// `<area> ...`: one request to a management area's owner.
    Management(crate::model::session::ManagementRequest),
    /// `<area> edit`: the area's document in the external editor.
    ManagementEdit(crate::model::session::Area),
    /// `<area> set PATH`: the area's document from a file, or from stdin for
    /// `-`.
    ManagementSet {
        area: crate::model::session::Area,
        path: std::path::PathBuf,
    },
    /// `recording [on|off]`: the daemon's model request record.
    Recording {
        enabled: Option<bool>,
    },
    /// `plugin ...`: one of PluginManager's methods and its params.
    Plugin {
        method: String,
        params: serde_json::Value,
    },
    /// `runtime list`: the plugin runtimes and their state.
    RuntimeList,
    /// `runtime install NAME`: turn a plugin runtime on and acquire it.
    RuntimeInstall(String),
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

#[derive(Clone)]
pub struct Options {
    pub action: Action,
    pub prompt: Option<String>,
    pub images: Vec<PathBuf>,
    pub json: bool,
    pub resume: Option<String>,
    pub home: PathBuf,
    pub home_explicit: bool,
    pub cwd: PathBuf,
    pub server: Option<PathBuf>,
    pub model_config: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permission: Option<PermissionPreset>,
    pub file_access: Option<dim_agent_types::FileAccess>,
    pub tool_approval: Option<dim_agent_types::ToolApproval>,
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
    /// The model whose limits `models --set-limits` declares.
    pub set_limits: Option<String>,
    /// Declare both limits for every model whose limits are undeclared.
    pub set_missing_limits: bool,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
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
            home_explicit: false,
            cwd: std::env::current_dir()?,
            server: None,
            model_config: None,
            provider: None,
            model: None,
            permission: None,
            file_access: None,
            tool_approval: None,
            directory: None,
            device: false,
            ui_identity: None,
            created_session: false,
            no_tui: false,
            fullscreen: None,
            refresh: false,
            clear_preferences: None,
            set_limits: None,
            set_missing_limits: false,
            context_window: None,
            max_output_tokens: None,
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
                "--source" => {
                    let Action::Migration(command) = &mut options.action else {
                        return Err("--source requires migration".into());
                    };
                    if command
                        .source
                        .replace(PathBuf::from(value(&mut args)?))
                        .is_some()
                    {
                        return Err("Supply --source only once".into());
                    }
                }
                "--source-stopped" => {
                    let Action::Migration(command) = &mut options.action else {
                        return Err("--source-stopped requires migration".into());
                    };
                    command.source_stopped = true;
                }
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
                    let database = match &mut options.action {
                        Action::Timeline { database, .. } => database,
                        Action::DaemonVacuum { database } => database,
                        _ => return Err("--database requires export or daemon vacuum".into()),
                    };
                    if database.replace(PathBuf::from(value(&mut args)?)).is_some() {
                        return Err("Supply the database path only once".into());
                    }
                }
                "--clear" => {
                    let Action::SessionRename { clear, .. } = &mut options.action else {
                        return Err("--clear requires session rename".into());
                    };
                    *clear = true;
                }
                "--older-than" => {
                    let value = parse_age(&value(&mut args)?)?;
                    let Action::SessionCleanup { older_than, .. } = &mut options.action else {
                        return Err("--older-than requires session cleanup".into());
                    };
                    if older_than.replace(value).is_some() {
                        return Err("Supply --older-than only once".into());
                    }
                }
                "--keep" => {
                    let count = parse_count("--keep", &value(&mut args)?)?;
                    let Action::SessionCleanup { keep, .. } = &mut options.action else {
                        return Err("--keep requires session cleanup".into());
                    };
                    if keep.replace(count).is_some() {
                        return Err("Supply --keep only once".into());
                    }
                }
                "--yes" => {
                    let Action::SessionCleanup { yes, .. } = &mut options.action else {
                        return Err("--yes requires session cleanup".into());
                    };
                    *yes = true;
                }
                "--home" => {
                    options.home = PathBuf::from(value(&mut args)?);
                    options.home_explicit = true;
                }
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
                "--set-limits" => {
                    if options.set_limits.replace(value(&mut args)?).is_some() {
                        return Err("Supply the model to set limits for only once".into());
                    }
                }
                "--set-missing-limits" => options.set_missing_limits = true,
                "--context-window" => {
                    let limit = parse_limit("--context-window", &value(&mut args)?)?;
                    if options.context_window.replace(limit).is_some() {
                        return Err("Supply --context-window only once".into());
                    }
                }
                "--max-output-tokens" => {
                    let limit = parse_limit("--max-output-tokens", &value(&mut args)?)?;
                    if options.max_output_tokens.replace(limit).is_some() {
                        return Err("Supply --max-output-tokens only once".into());
                    }
                }
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
                "acp" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Acp {
                        agent_id: None,
                        auth_setup: None,
                    };
                }
                "--agent" => {
                    let Action::Acp { agent_id, .. } = &mut options.action else {
                        return Err("--agent requires acp".into());
                    };
                    if agent_id.replace(value(&mut args)?).is_some() {
                        return Err("Supply --agent only once".into());
                    }
                }
                "--auth-setup" => {
                    let Action::Acp { auth_setup, .. } = &mut options.action else {
                        return Err("--auth-setup requires acp".into());
                    };
                    if auth_setup.replace(value(&mut args)?).is_some() {
                        return Err("Supply --auth-setup only once".into());
                    }
                }
                "tui" if matches!(options.action, Action::Chat) => options.action = Action::Tui,
                "exec" if matches!(options.action, Action::Chat) => options.action = Action::Exec,
                "resume" if matches!(options.action, Action::Exec) => {
                    if options.resume.replace(value(&mut args)?).is_some() {
                        return Err("Supply the Session only once".into());
                    }
                }
                "session" if matches!(options.action, Action::Chat) => {
                    match value(&mut args)?.as_str() {
                        "-h" | "--help" => {
                            options.action = Action::Help;
                            return Ok(options);
                        }
                        "export" => {
                            options.action = Action::Export {
                                session_id: value(&mut args)?,
                                format: Format::Markdown,
                                output: None,
                            };
                        }
                        "rename" => {
                            let session_id = value(&mut args)?;
                            let title = match args.peek().map(String::as_str) {
                                Some(next) if !next.starts_with("--") => Some(value(&mut args)?),
                                _ => None,
                            };
                            options.action = Action::SessionRename {
                                session_id,
                                title,
                                clear: false,
                            };
                        }
                        "delete" => {
                            options.action = Action::SessionDelete {
                                session_id: value(&mut args)?,
                            };
                        }
                        "cleanup" => {
                            options.action = Action::SessionCleanup {
                                older_than: None,
                                keep: None,
                                yes: false,
                            };
                        }
                        _ => return Err(USAGE_SESSION.into()),
                    }
                }
                "export" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Timeline {
                        session_id: value(&mut args)?,
                        output: None,
                        database: None,
                    };
                }
                "plugin" if matches!(options.action, Action::Chat) => {
                    let (method, params) = crate::plugin_command::parse(&mut args)?;
                    options.action = Action::Plugin { method, params };
                }
                "migration" if matches!(options.action, Action::Chat) => {
                    options.action = crate::migration_command::parse(&mut args)?;
                    if matches!(options.action, Action::Help) {
                        return Ok(options);
                    }
                }
                "runtime" if matches!(options.action, Action::Chat) => {
                    options.action = match value(&mut args)?.as_str() {
                        "list" => Action::RuntimeList,
                        "install" => Action::RuntimeInstall(value(&mut args)?),
                        _ => return Err("Use runtime list or runtime install node|python".into()),
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
                "skills" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
                    let verb = value(&mut args)?;
                    let action = match verb.as_str() {
                        "list" => ManagementAction::List,
                        "reload" => ManagementAction::Reload,
                        command @ ("enable" | "disable") => ManagementAction::Toggle {
                            id: value(&mut args)?,
                            enabled: command == "enable",
                        },
                        _ => {
                            return Err(
                                "Use skills list, skills enable ID, skills disable ID or skills reload"
                                    .into(),
                            );
                        }
                    };
                    options.action = Action::Management(ManagementRequest {
                        area: Area::Skills,
                        action,
                    });
                }
                "hooks" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
                    let verb = value(&mut args)?;
                    options.action = match verb.as_str() {
                        "list" => Action::Management(ManagementRequest {
                            area: Area::Hooks,
                            action: ManagementAction::List,
                        }),
                        command @ ("enable" | "disable") => Action::Management(ManagementRequest {
                            area: Area::Hooks,
                            action: ManagementAction::Toggle {
                                id: crate::management::hooks::PROJECT.to_owned(),
                                enabled: command == "enable",
                            },
                        }),
                        "edit" => Action::ManagementEdit(Area::Hooks),
                        "set" => Action::ManagementSet {
                            area: Area::Hooks,
                            path: value(&mut args)?.into(),
                        },
                        _ => {
                            return Err(
                                "Use hooks list, hooks enable, hooks disable, hooks edit or hooks set PATH"
                                    .into(),
                            );
                        }
                    };
                }
                "recording" if matches!(options.action, Action::Chat) => {
                    let enabled = match args.peek().cloned().as_deref() {
                        Some("on") => {
                            args.next();
                            Some(true)
                        }
                        Some("off") => {
                            args.next();
                            Some(false)
                        }
                        Some(other) if !other.starts_with('-') => {
                            return Err(format!("Use recording [on|off], not {other:?}").into());
                        }
                        _ => None,
                    };
                    options.action = Action::Recording { enabled };
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
                    options.action = match value(&mut args)?.as_str() {
                        "stop" => Action::DaemonStop,
                        "vacuum" => Action::DaemonVacuum { database: None },
                        _ => {
                            return Err("Use daemon stop or daemon vacuum [--database PATH]".into());
                        }
                    };
                }
                "login" | "account" | "logout" | "models"
                    if matches!(options.action, Action::Chat) =>
                {
                    let provider = match args.peek().map(String::as_str) {
                        Some("-h" | "--help") => {
                            options.action = Action::Help;
                            return Ok(options);
                        }
                        Some(next) if !next.starts_with('-') => Some(value(&mut args)?),
                        _ => None,
                    };
                    if arg == "login" {
                        options.action = Action::Login(provider);
                    } else {
                        let provider = provider.ok_or_else(|| format!("{arg} requires a value"))?;
                        options.action = match arg.as_str() {
                            "account" => Action::Account(provider),
                            "logout" => Action::Logout(provider),
                            _ => Action::Models(provider),
                        };
                    }
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
        if options.json && !matches!(options.action, Action::Exec | Action::Migration(_)) {
            return Err("--json requires exec or migration".into());
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
        if let Action::SessionRename { title, clear, .. } = &options.action
            && (title.is_some() == *clear
                || title
                    .as_deref()
                    .is_some_and(|title| title.trim().is_empty()))
        {
            return Err(
                "Use session rename SESSION TITLE or session rename SESSION --clear".into(),
            );
        }
        if let Action::SessionCleanup {
            older_than, keep, ..
        } = &options.action
            && older_than.is_some() == keep.is_some()
        {
            return Err(
                "Use session cleanup --older-than DURATION or session cleanup --keep N".into(),
            );
        }
        if options.clear_preferences.is_some()
            && (!matches!(options.action, Action::Models(_)) || options.refresh)
        {
            return Err("--clear-preferences requires models PROVIDER and cannot be combined with --refresh".into());
        }
        if options.refresh && !matches!(options.action, Action::Models(_)) {
            return Err("--refresh requires the models command".into());
        }
        if options.set_limits.is_some() && options.set_missing_limits {
            return Err("Use --set-limits MODEL or --set-missing-limits, not both".into());
        }
        if (options.set_limits.is_some() || options.set_missing_limits)
            && (!matches!(options.action, Action::Models(_))
                || options.refresh
                || options.clear_preferences.is_some())
        {
            return Err("--set-limits and --set-missing-limits require models PROVIDER and cannot be combined with --refresh or --clear-preferences".into());
        }
        if options.set_limits.is_some() || options.set_missing_limits {
            if options.context_window.is_none() || options.max_output_tokens.is_none() {
                return Err("--set-limits and --set-missing-limits require --context-window and --max-output-tokens".into());
            }
        } else if options.context_window.is_some() || options.max_output_tokens.is_some() {
            return Err(
                "--context-window and --max-output-tokens require --set-limits or --set-missing-limits"
                    .into(),
            );
        }
        if (options.no_browser || options.client_secret)
            && !matches!(options.action, Action::McpLogin(_))
        {
            return Err("--no-browser and --client-secret require mcp login ID".into());
        }
        if (!matches!(options.action, Action::Login(_) | Action::Acp { .. })
            && options.directory.is_some())
            || (!matches!(options.action, Action::Login(_)) && options.device)
        {
            return Err("Login options require the login command".into());
        }
        options.cwd = std::fs::canonicalize(options.cwd)?;
        if !options.cwd.is_dir() {
            return Err("--cwd must name a directory".into());
        }
        if matches!(options.action, Action::DaemonStop) {
            return Ok(options);
        }
        if let Some(path) = &mut options.server {
            *path = std::fs::canonicalize(&*path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    format!("{SERVER_REQUIREMENT}; {} does not exist", path.display())
                } else {
                    format!("{SERVER_REQUIREMENT}; {}: {error}", path.display())
                }
            })?;
            if !path.is_file() {
                return Err(format!(
                    "{SERVER_REQUIREMENT}; {} is not a regular file",
                    path.display()
                )
                .into());
            }
        }
        if let Some(path) = &mut options.model_config {
            *path = std::fs::canonicalize(&*path)?;
            if !path.is_file() {
                return Err("--model-config must name a regular file".into());
            }
        }
        Ok(options)
    }
}

/// A model limit as `--set-limits` and `--set-missing-limits` take it: a
/// positive integer a listed model's capacity can hold as a JSON number.
fn parse_limit(flag: &str, value: &str) -> Result<u64> {
    let limit: u64 = value
        .parse()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if limit == 0 || limit > dim_agent_types::JS_MAX_SAFE_INTEGER {
        return Err(
            format!("{flag} must be a positive integer of at most 9007199254740991").into(),
        );
    }
    Ok(limit)
}

/// `--older-than` takes a count and a unit, such as `30d`, `12h` or `45m`.
fn parse_age(value: &str) -> Result<std::time::Duration> {
    const USAGE: &str = "--older-than takes a number and a unit d, h or m, such as 30d";
    let mut chars = value.chars();
    let unit = match chars.next_back() {
        Some('d') => 86_400,
        Some('h') => 3_600,
        Some('m') => 60,
        _ => return Err(USAGE.into()),
    };
    let count: u64 = chars.as_str().parse().map_err(|_| USAGE)?;
    let seconds = count
        .checked_mul(unit)
        .ok_or("--older-than is too large to measure")?;
    Ok(std::time::Duration::from_secs(seconds))
}

/// `--keep` takes the number of Sessions to keep.
fn parse_count(flag: &str, value: &str) -> Result<usize> {
    value
        .parse()
        .map_err(|_| format!("{flag} must be a whole number").into())
}

/// The product home: `DIMCODE_HOME`, else `~/.dimcode-alpha` for an alpha
/// build and `~/.dimcode` otherwise, so an alpha never writes a release's
/// sessions or settings.
pub fn default_home() -> PathBuf {
    let directory = if crate::update::channel(env!("CARGO_PKG_VERSION")) == "alpha" {
        ".dimcode-alpha"
    } else {
        ".dimcode"
    };
    std::env::var_os("DIMCODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(directory))
                .unwrap_or_default()
        })
}
