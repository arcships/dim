use crate::{Result, export::Format, permission};
use dim_agent_types::PermissionPreset;
use std::path::{Path, PathBuf};

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
                                   List a Provider's models; --refresh reloads\n\
                                   the area's list\n\
  models PROVIDER enable|disable|remove|inspect MODEL\n\
                                   Enable or disable one model, remove it,\n\
                                   or print its descriptor\n\
  models PROVIDER edit MODEL       Edit the model's fields in the editor\n\
  models PROVIDER set MODEL PATH   Write the model's fields from PATH,\n\
                                   or from stdin for -\n\
  models PROVIDER add PATH         Create a model from PATH, or from stdin\n\
                                   for -\n\
  mcp list                         List MCP servers and their enablement\n\
  mcp login ID [--no-browser] [--client-secret]\n\
                                   Sign in; secrets use hidden terminal input\n\
  mcp logout ID                    Sign out and report token revocation\n\
  mcp enable ID                    Enable an MCP server shown by mcp list\n\
  mcp disable ID                   Disable it; an unlisted ID withdraws only\n\
                                   this workspace's stored enablement\n\
  mcp add PATH                     Create an entry from PATH, or from stdin for -\n\
  mcp edit ID                      Edit an entry's patch in the editor\n\
  mcp set ID PATH                  Write an entry's patch from PATH,\n\
                                   or from stdin for -\n\
  mcp remove ID                    Delete an entry\n\
  mcp test ID                      Probe a stored entry and list its tools\n\
  mcp inspect ID                   Show an entry's effective configuration\n\
  mcp disconnect ID                Close an entry's live connections\n\
  skills list                      List Skills, their sources and states\n\
  skills enable|disable ID         Turn one Skill on or off\n\
  skills reload                    Rescan this workspace's Skills\n\
  hooks list                       List hook handlers, their sources and states\n\
  hooks enable                     Run this workspace's .dimcode/hooks.json\n\
  hooks disable                    Stop running this workspace's hooks\n\
  hooks edit                       Edit the user's hook document\n\
  hooks set PATH                   Write the user's hook document from PATH,\n\
                                   or from stdin for -\n\
  scheduled list                   List scheduled jobs, their windows and\n\
                                   last runs\n\
  scheduled enable|disable ID      Pause or resume one job\n\
  scheduled edit ID                Edit a job document in the editor\n\
  scheduled set ID PATH            Write a job document from PATH,\n\
                                   or from stdin for -\n\
  scheduled add PATH               Create a job from PATH, or from stdin for -\n\
  scheduled remove ID              Delete a job\n\
  scheduled run ID                 Run a job now\n\
  scheduled history ID             List a job's recent runs\n\
  permissions list                 List the user's command rules\n\
  permissions add PATH             Add one rule from PATH, or from stdin for -\n\
  permissions remove RULE          Remove a rule, given as one quoted\n\
                                   'DECISION MATCH COMMAND'\n\
  workflow list                    List workflow definitions, their sources\n\
                                   and states\n\
  workflow enable|disable NAME     Turn a project definition on or off\n\
  workflow edit NAME               Edit a user definition in the editor\n\
  workflow set NAME PATH           Write a user definition from PATH,\n\
                                   or from stdin for -\n\
  workflow add PATH                Create a definition from PATH,\n\
                                   or from stdin for -\n\
  workflow start NAME [PATH]       Start a definition with parameters from PATH\n\
  workflow history NAME            List a definition's recent runs\n\
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
  runtime config NAME              Set a runtime's --source or --auto and apply it\n\
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
  --model ID [--provider ID]        Temporary model selection (also on resume)\n\
  --effort LEVEL                   Temporary reasoning effort (also on resume)\n\
  --autocompact auto|TOKENS         Temporary compaction window (k/m suffixes)\n\
  --permission PRESET              read-only, workspace-write or full-access for a\n\
                                   new session, or for the resumed main branch\n\
  -h, --help                       Show this help without starting a daemon\n\
  -V, --version                    Show version without starting a daemon\n\n\
Model startup:\n\
  --model ID overrides DIM_MODEL; --provider optionally names a configured\n\
    Provider. A bare ID uses the effective Provider or an unambiguous match.\n\
  --effort LEVEL overrides DIM_EFFORT_LEVEL, then the model's saved effort.\n\
    LEVEL must be supported by the selected Provider model; service tier is\n\
    preserved and the override is not saved as a model preference.\n\
  DIM_BASE_URL + DIM_API_FORMAT select a Session-local direct connection.\n\
    Both are required; they conflict with --provider and --model-config.\n\
    A new Session also requires --model or DIM_MODEL; resume can retain its ID.\n\
    DIM_API_FORMAT: anthropic, openai-chat, openai-responses, openai-compatible,\n\
      gemini, xai or xai-responses. DIM_BASE_URL may include a gateway prefix.\n\
  DIM_API_KEY uses x-api-key (anthropic), x-goog-api-key (gemini), or Bearer\n\
    (other formats). DIM_AUTH_TOKEN uses Bearer for any format. Set at most one.\n\
  DIM_CUSTOM_HEADERS is newline-separated Name: Value, split at the first colon.\n\
    Names are case-insensitive; duplicates and generated auth conflicts fail.\n\
    Credentials and headers require direct mode; anonymous gateways are allowed.\n\
  DIM_MAX_CONTEXT_TOKENS and DIM_MAX_OUTPUT_TOKENS declare positive capacities\n\
    up to 9007199254740991; output cannot exceed context.\n\
  --autocompact overrides DIM_AUTO_COMPACT_WINDOW: auto or a positive count,\n\
    optionally suffixed k/m (1000/1000000). This is not the model capacity.\n\
  Effort, autocompact and capacity startup controls require the product Provider\n\
    and cannot be used with --model-config; set API-key generation settings\n\
    in that configuration file instead.\n\
  Model overrides with --model-config require --provider, including on resume.\n\
  Startup overrides apply before the first input, including on resume, without\n\
    changing saved Providers, preferences, defaults or the daemon environment.\n\
    Explicit overrides force complete configuration inheritance for new work.\n\
    DIM_* startup inputs are ignored by help, version and management commands.\n\
  Examples (keep secrets in the environment, not command arguments):\n\
    dimcode exec --model model-id --effort high --autocompact 128k -p 'Hello'\n\
    DIM_BASE_URL=https://gateway.example/prefix DIM_API_FORMAT=openai-chat \\\n\
      DIM_MODEL=model-id dimcode exec -p 'Hello'\n\
    dimcode exec resume SESSION_ID --effort low --autocompact auto -p 'Continue'\n\
  To supply an API connection, use --model-config PATH with a JSON file:\n\
    defaultModel: {providerId, modelId}; providers: [{id, kind, baseUrl,\n\
      apiKeyEnv, headers, models: [{id, contextWindow, maxOutputTokens, config}]}].\n\
  kind selects the API format, not the model name: anthropic, openai-responses,\n\
    openai-chat, openai-compatible, gemini, xai or xai-responses.\n\
  baseUrl is the API base URL; the selected format supplies the request path.\n\
  apiKeyEnv names an environment variable, not the key itself. Keys use\n\
    x-api-key for anthropic, x-goog-api-key for gemini, or Bearer otherwise.\n\
  headers is a JSON object for additional request headers, including gateway\n\
    authentication when needed. Keep credentials out of command arguments.\n\
  Each model requires contextWindow, maxOutputTokens and config.maxTokens:\n\
    the total context capacity, output capacity and request output budget.\n\
  The daemon inherits its launch environment; a later client's environment\n\
    does not update it. Use a separate --home for a different model config.\n\
  Example: dimcode --home /path/to/isolated-home --model-config model.json\n\n\
Ctrl+C stops a running turn and waits for settlement; idle Ctrl+C or EOF exits.\n\
Without -p, exec reads redirected stdin through EOF, including with --image.\n\
Idle commands: /export <markdown|json> PATH, /rename TITLE|--clear, /exit.\n\
PATH is relative to the CLI current directory; ~ is literal.\n\
/permissions (aliases /approvals, /approval) selects a permission preset in the terminal UI, with\n\
  Read only, Workspace write and Full access choices, and\n\
  a Command rules row. Process isolation is in Settings.\n\
The line prompt accepts PRESET, allow|deny exact|prefix COMMAND, or remove N.\n\
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
    McpDisconnect(String),
    /// `<area> ...`: one request to a management area's owner.
    Management(crate::model::session::ManagementRequest),
    /// `<area> edit [ID]`: the area's document in the external editor, or one
    /// entry's for an area whose documents are per entry.
    ManagementEdit {
        area: crate::model::session::Area,
        id: Option<String>,
    },
    /// `<area> set [ID] PATH`: the same write from a file, or from stdin for
    /// `-`.
    ManagementSet {
        area: crate::model::session::Area,
        id: Option<String>,
        path: std::path::PathBuf,
    },
    /// `<area> add PATH`: a new entry from a file, or from stdin for `-`.
    ManagementAdd {
        area: crate::model::session::Area,
        path: std::path::PathBuf,
    },
    /// `<area> start ID [PATH]`: run one entry, with its input from a file or
    /// from stdin for `-`.
    ManagementStart {
        area: crate::model::session::Area,
        id: String,
        path: Option<std::path::PathBuf>,
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
    /// `runtime config NAME [--source download|system] [--auto on|off]`:
    /// write the runtime's configuration and apply it.
    RuntimeConfig {
        name: String,
        source: Option<String>,
        auto: Option<bool>,
    },
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

/// The workspace every request names: the launch directory, resolved once as
/// a UTF-8 string. A `Path` cannot stand in for it, so no request can carry a
/// path serde refuses or one a lossy conversion would alter; the client's own
/// file system work asks for [`Workspace::path`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace(String);

impl Workspace {
    /// The workspace `path` names. A path that is not valid UTF-8 names none:
    /// the refusal shows it with the invalid bytes replaced.
    fn resolve(path: &Path) -> Result<Self> {
        match path.to_str() {
            Some(text) => Ok(Self(text.to_owned())),
            None => {
                Err(format!("The workspace path {} must be valid UTF-8", path.display()).into())
            }
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The same workspace, for the client's own file system work.
    pub fn path(&self) -> &Path {
        Path::new(&self.0)
    }
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
    /// The launch directory: where the daemon starts and what the client's
    /// own file system work resolves against. Requests carry [`Self::workspace`]
    /// instead.
    cwd: PathBuf,
    /// The workspace the command names in its requests, resolved once the
    /// options are complete; `None` for a command that names none.
    workspace: Option<Workspace>,
    pub server: Option<PathBuf>,
    pub model_config: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub startup: Option<serde_json::Value>,
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
            workspace: None,
            server: None,
            model_config: None,
            provider: None,
            model: None,
            effort: None,
            startup: None,
            permission: None,
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
        let mut autocompact = None;
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
                "--effort" => {
                    if options.effort.replace(value(&mut args)?).is_some() {
                        return Err("Supply --effort only once".into());
                    }
                }
                "--autocompact" => {
                    if autocompact.replace(value(&mut args)?).is_some() {
                        return Err("Supply --autocompact only once".into());
                    }
                }
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
                        "config" => {
                            let name = value(&mut args)?;
                            let (mut source, mut auto) = (None, None);
                            while let Some(flag) = args.next() {
                                let given = args
                                    .next()
                                    .ok_or_else(|| format!("{flag} requires a value"))?;
                                match flag.as_str() {
                                    "--source" => match given.as_str() {
                                        "download" | "system" => source = Some(given),
                                        other => {
                                            return Err(format!(
                                                "--source is download or system, not {other}"
                                            )
                                            .into());
                                        }
                                    },
                                    "--auto" => match given.as_str() {
                                        "on" => auto = Some(true),
                                        "off" => auto = Some(false),
                                        other => {
                                            return Err(format!(
                                                "--auto is on or off, not {other}"
                                            )
                                            .into());
                                        }
                                    },
                                    other => {
                                        return Err(format!(
                                            "Use runtime config NAME [--source download|system] \
                                             [--auto on|off], not {other}"
                                        )
                                        .into());
                                    }
                                }
                            }
                            Action::RuntimeConfig { name, source, auto }
                        }
                        _ => {
                            return Err(
                                "Use runtime list, runtime install or runtime config".into()
                            );
                        }
                    };
                }
                "mcp" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
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
                        "disconnect" => Action::McpDisconnect(value(&mut args)?),
                        command @ ("enable" | "disable") => Action::McpEnable {
                            id: value(&mut args)?,
                            enabled: command == "enable",
                        },
                        "add" => Action::ManagementAdd {
                            area: Area::Mcp,
                            path: value(&mut args)?.into(),
                        },
                        "edit" => Action::ManagementEdit {
                            area: Area::Mcp,
                            id: Some(value(&mut args)?),
                        },
                        "set" => Action::ManagementSet {
                            area: Area::Mcp,
                            id: Some(value(&mut args)?),
                            path: value(&mut args)?.into(),
                        },
                        "remove" => Action::Management(ManagementRequest {
                            area: Area::Mcp,
                            action: ManagementAction::Remove {
                                id: value(&mut args)?,
                            },
                        }),
                        "test" => Action::Management(ManagementRequest {
                            area: Area::Mcp,
                            action: ManagementAction::Run {
                                id: value(&mut args)?,
                                document: None,
                                seed: None,
                            },
                        }),
                        "inspect" => Action::Management(ManagementRequest {
                            area: Area::Mcp,
                            action: ManagementAction::Inspect {
                                id: value(&mut args)?,
                            },
                        }),
                        _ => {
                            return Err("Use mcp list, mcp add PATH, mcp edit ID, mcp set ID PATH, mcp remove ID, mcp test ID, mcp inspect ID, mcp disconnect ID, mcp login ID, mcp logout ID, mcp enable ID or mcp disable ID".into());
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
                        "edit" => Action::ManagementEdit {
                            area: Area::Hooks,
                            id: None,
                        },
                        "set" => Action::ManagementSet {
                            area: Area::Hooks,
                            id: None,
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
                "scheduled" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
                    let verb = value(&mut args)?;
                    options.action = match verb.as_str() {
                        "list" => Action::Management(ManagementRequest {
                            area: Area::Scheduled,
                            action: ManagementAction::List,
                        }),
                        command @ ("enable" | "disable") => Action::Management(ManagementRequest {
                            area: Area::Scheduled,
                            action: ManagementAction::Toggle {
                                id: value(&mut args)?,
                                enabled: command == "enable",
                            },
                        }),
                        "edit" => Action::ManagementEdit {
                            area: Area::Scheduled,
                            id: Some(value(&mut args)?),
                        },
                        "set" => Action::ManagementSet {
                            area: Area::Scheduled,
                            id: Some(value(&mut args)?),
                            path: value(&mut args)?.into(),
                        },
                        "add" => Action::ManagementAdd {
                            area: Area::Scheduled,
                            path: value(&mut args)?.into(),
                        },
                        command @ ("remove" | "run" | "history") => {
                            let id = value(&mut args)?;
                            let action = match command {
                                "remove" => ManagementAction::Remove { id },
                                "run" => ManagementAction::Run {
                                    id,
                                    document: None,
                                    seed: None,
                                },
                                _ => ManagementAction::Inspect { id },
                            };
                            Action::Management(ManagementRequest {
                                area: Area::Scheduled,
                                action,
                            })
                        }
                        _ => {
                            return Err("Use scheduled list, enable ID, disable ID, edit ID, set ID PATH, add PATH, remove ID, run ID or history ID".into());
                        }
                    };
                }
                "workflow" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
                    let verb = value(&mut args)?;
                    options.action = match verb.as_str() {
                        "list" => Action::Management(ManagementRequest {
                            area: Area::Workflow,
                            action: ManagementAction::List,
                        }),
                        command @ ("enable" | "disable") => Action::Management(ManagementRequest {
                            area: Area::Workflow,
                            action: ManagementAction::Toggle {
                                id: value(&mut args)?,
                                enabled: command == "enable",
                            },
                        }),
                        "edit" => Action::ManagementEdit {
                            area: Area::Workflow,
                            id: Some(value(&mut args)?),
                        },
                        "set" => Action::ManagementSet {
                            area: Area::Workflow,
                            id: Some(value(&mut args)?),
                            path: value(&mut args)?.into(),
                        },
                        "add" => Action::ManagementAdd {
                            area: Area::Workflow,
                            path: value(&mut args)?.into(),
                        },
                        "start" => Action::ManagementStart {
                            area: Area::Workflow,
                            id: value(&mut args)?,
                            path: args.next().map(Into::into),
                        },
                        "history" => Action::Management(ManagementRequest {
                            area: Area::Workflow,
                            action: ManagementAction::Inspect {
                                id: value(&mut args)?,
                            },
                        }),
                        _ => {
                            return Err(
                                "Use workflow list, enable NAME, disable NAME, edit NAME, set NAME PATH, add PATH, start NAME [PATH] or history NAME"
                                    .into(),
                            );
                        }
                    };
                }
                "permissions" if matches!(options.action, Action::Chat) => {
                    use crate::model::session::{Area, ManagementAction, ManagementRequest};
                    let verb = value(&mut args)?;
                    options.action = match verb.as_str() {
                        "list" => Action::Management(ManagementRequest {
                            area: Area::Permissions,
                            action: ManagementAction::List,
                        }),
                        "add" => Action::ManagementAdd {
                            area: Area::Permissions,
                            path: value(&mut args)?.into(),
                        },
                        "remove" => Action::Management(ManagementRequest {
                            area: Area::Permissions,
                            action: ManagementAction::Remove {
                                id: value(&mut args)?,
                            },
                        }),
                        _ => {
                            return Err("Use permissions list, permissions add PATH or permissions remove 'DECISION MATCH COMMAND'".into());
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
                        if arg == "account" {
                            options.action = Action::Account(provider);
                        } else if arg == "logout" {
                            options.action = Action::Logout(provider);
                        } else {
                            use crate::model::session::{
                                Area, ManagementAction, ManagementRequest,
                            };
                            match args.peek().cloned().as_deref() {
                                Some("enable" | "disable") => {
                                    let enabled = value(&mut args)? == "enable";
                                    options.action = Action::Management(ManagementRequest {
                                        area: Area::Models {
                                            provider_id: provider.clone(),
                                        },
                                        action: ManagementAction::Toggle {
                                            id: value(&mut args)?,
                                            enabled,
                                        },
                                    });
                                }
                                Some("edit") => {
                                    args.next();
                                    options.action = Action::ManagementEdit {
                                        area: Area::Models {
                                            provider_id: provider.clone(),
                                        },
                                        id: Some(value(&mut args)?),
                                    };
                                }
                                Some("set") => {
                                    args.next();
                                    options.action = Action::ManagementSet {
                                        area: Area::Models {
                                            provider_id: provider.clone(),
                                        },
                                        id: Some(value(&mut args)?),
                                        path: value(&mut args)?.into(),
                                    };
                                }
                                Some("add") => {
                                    args.next();
                                    options.action = Action::ManagementAdd {
                                        area: Area::Models {
                                            provider_id: provider.clone(),
                                        },
                                        path: value(&mut args)?.into(),
                                    };
                                }
                                Some(verb @ ("remove" | "inspect")) => {
                                    args.next();
                                    let id = value(&mut args)?;
                                    let action = if verb == "remove" {
                                        ManagementAction::Remove { id }
                                    } else {
                                        ManagementAction::Inspect { id }
                                    };
                                    options.action = Action::Management(ManagementRequest {
                                        area: Area::Models {
                                            provider_id: provider.clone(),
                                        },
                                        action,
                                    });
                                }
                                _ => options.action = Action::Models(provider),
                            }
                        }
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
        if matches!(options.action, Action::Chat | Action::Tui | Action::Exec) {
            options.read_startup(autocompact.take())?;
        }
        if options
            .effort
            .as_ref()
            .is_some_and(|effort| effort.is_empty() || effort.trim() != effort)
        {
            return Err("--effort / DIM_EFFORT_LEVEL must be a nonempty effort name without surrounding whitespace".into());
        }
        if options.provider.is_some() && options.model.is_none() {
            return Err("--provider requires --model or DIM_MODEL".into());
        }
        if !matches!(options.action, Action::Chat | Action::Tui | Action::Exec)
            && (options.prompt.is_some()
                || options.resume.is_some()
                || options.model.is_some()
                || options.effort.is_some()
                || autocompact.is_some()
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

    fn read_startup(&mut self, mut autocompact: Option<String>) -> Result<()> {
        if self.model.is_none() {
            self.model = startup_env("DIM_MODEL")?;
        }
        if self.effort.is_none() {
            self.effort = startup_env("DIM_EFFORT_LEVEL")?;
        }
        if autocompact.is_none() {
            autocompact = startup_env("DIM_AUTO_COMPACT_WINDOW")?;
        }
        if self
            .model
            .as_ref()
            .is_some_and(|model| model.is_empty() || model.trim() != model)
        {
            return Err(
                "--model / DIM_MODEL must be a nonempty model ID without surrounding whitespace"
                    .into(),
            );
        }
        let base_url = startup_env("DIM_BASE_URL")?;
        let api_format = startup_env("DIM_API_FORMAT")?;
        let api_key = startup_env("DIM_API_KEY")?;
        let auth_token = startup_env("DIM_AUTH_TOKEN")?;
        let headers = startup_env("DIM_CUSTOM_HEADERS")?;
        let mut startup = serde_json::json!({});
        match (base_url, api_format) {
            (Some(base_url), Some(api_format)) => {
                if self.provider.is_some() || self.model_config.is_some() {
                    return Err(
                        "Direct startup conflicts with --provider and --model-config".into(),
                    );
                }
                if self.model.is_none() && self.resume.is_none() {
                    return Err(
                        "Direct startup requires --model or DIM_MODEL for a new Session".into(),
                    );
                }
                let url = url::Url::parse(&base_url)
                    .map_err(|_| "DIM_BASE_URL must be an HTTP(S) base URL")?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || base_url.trim() != base_url
                    || base_url.bytes().any(|byte| byte.is_ascii_control())
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err("DIM_BASE_URL must be an HTTP(S) base URL without credentials, query or fragment".into());
                }
                if !matches!(
                    api_format.as_str(),
                    "anthropic"
                        | "openai-chat"
                        | "openai-responses"
                        | "openai-compatible"
                        | "gemini"
                        | "xai"
                        | "xai-responses"
                ) {
                    return Err("DIM_API_FORMAT must be anthropic, openai-chat, openai-responses, openai-compatible, gemini, xai or xai-responses".into());
                }
                if api_key.is_some() && auth_token.is_some() {
                    return Err("DIM_API_KEY and DIM_AUTH_TOKEN are mutually exclusive".into());
                }
                let generated_auth = if auth_token.is_some() {
                    Some("authorization")
                } else if api_key.is_some() {
                    Some(match api_format.as_str() {
                        "anthropic" => "x-api-key",
                        "gemini" => "x-goog-api-key",
                        _ => "authorization",
                    })
                } else {
                    None
                };
                let mut connection = serde_json::json!({"baseUrl":base_url,"apiFormat":api_format});
                for (name, value) in [("apiKey", api_key), ("authToken", auth_token)] {
                    if let Some(value) = value {
                        reqwest::header::HeaderValue::from_str(&value)
                            .map_err(|_| "Invalid startup credential header value")?;
                        connection[name] = serde_json::json!(value);
                    }
                }
                if let Some(headers) = headers {
                    let mut parsed = serde_json::Map::new();
                    for line in headers.lines() {
                        let (name, value) = line
                            .split_once(':')
                            .ok_or("DIM_CUSTOM_HEADERS requires one Name: Value per line")?;
                        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                            .map_err(|_| "Invalid DIM_CUSTOM_HEADERS name")?;
                        let value = value.trim_matches([' ', '\t']);
                        reqwest::header::HeaderValue::from_str(value)
                            .map_err(|_| "Invalid DIM_CUSTOM_HEADERS value")?;
                        if generated_auth == Some(name.as_str()) {
                            return Err(
                                "DIM_CUSTOM_HEADERS conflicts with generated authentication".into(),
                            );
                        }
                        if parsed
                            .insert(name.to_string(), serde_json::json!(value))
                            .is_some()
                        {
                            return Err("DIM_CUSTOM_HEADERS contains a repeated header name".into());
                        }
                    }
                    connection["headers"] = serde_json::Value::Object(parsed);
                }
                startup["connection"] = connection;
            }
            (None, None) => {
                if api_key.is_some() || auth_token.is_some() || headers.is_some() {
                    return Err("DIM_API_KEY, DIM_AUTH_TOKEN and DIM_CUSTOM_HEADERS require DIM_BASE_URL and DIM_API_FORMAT".into());
                }
            }
            _ => return Err("Direct startup requires both DIM_BASE_URL and DIM_API_FORMAT".into()),
        }
        for (env, field) in [
            ("DIM_MAX_CONTEXT_TOKENS", "contextWindow"),
            ("DIM_MAX_OUTPUT_TOKENS", "maxOutputTokens"),
        ] {
            if let Some(value) = startup_env(env)? {
                startup[field] = serde_json::json!(parse_limit(env, &value)?);
            }
        }
        if let (Some(context), Some(output)) = (
            startup["contextWindow"].as_u64(),
            startup["maxOutputTokens"].as_u64(),
        ) && output > context
        {
            return Err("DIM_MAX_OUTPUT_TOKENS cannot exceed DIM_MAX_CONTEXT_TOKENS".into());
        }
        if let Some(value) = &autocompact {
            startup["autoCompactWindow"] = if value == "auto" {
                serde_json::json!("auto")
            } else {
                let (digits, multiplier) = match value.as_bytes().last() {
                    Some(b'k' | b'K') => (&value[..value.len() - 1], 1_000),
                    Some(b'm' | b'M') => (&value[..value.len() - 1], 1_000_000),
                    _ => (value.as_str(), 1),
                };
                let tokens = parse_limit("--autocompact / DIM_AUTO_COMPACT_WINDOW", digits)?
                    .checked_mul(multiplier)
                    .filter(|n| *n <= dim_agent_types::JS_MAX_SAFE_INTEGER)
                    .ok_or("--autocompact / DIM_AUTO_COMPACT_WINDOW is too large")?;
                serde_json::json!(tokens)
            };
        }
        if self.model_config.is_some() {
            if self.model.is_some() && self.provider.is_none() {
                return Err("--model / DIM_MODEL requires --provider when used with --model-config; select the configured Provider explicitly or omit the model override".into());
            }
            if self.effort.is_some() {
                return Err("--effort / DIM_EFFORT_LEVEL require a product Provider catalog with capability declarations and cannot be used with --model-config; configure the API-key model's generation settings in --model-config instead".into());
            }
            if startup.as_object().is_some_and(|o| !o.is_empty()) {
                return Err("--autocompact / DIM_AUTO_COMPACT_WINDOW, DIM_MAX_CONTEXT_TOKENS and DIM_MAX_OUTPUT_TOKENS require the product Provider and cannot be used with --model-config; omit these startup controls or use a product Provider".into());
            }
        } else if self.model.is_some()
            || self.effort.is_some()
            || startup.as_object().is_some_and(|o| !o.is_empty())
        {
            self.startup = Some(startup);
        }
        Ok(())
    }

    pub(crate) fn creation_startup(&self, params: &mut serde_json::Value) {
        if let Some(model) = &self.model {
            params["model"] = serde_json::json!({"modelId":model});
            if let Some(provider) = &self.provider {
                params["model"]["provider"] = serde_json::json!(provider);
            }
        }
        if let Some(startup) = &self.startup {
            params["startup"] = startup.clone();
        }
    }

    pub(crate) fn creation_preferences(
        &self,
        mut saved: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        if let Some(effort) = &self.effort {
            saved.get_or_insert_with(|| serde_json::json!({}))["reasoningEffort"] =
                serde_json::json!(effort);
        }
        saved
    }
}

fn startup_env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) if value.is_empty() => Err(format!("{name} must not be empty").into()),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{name} must be valid Unicode").into())
        }
    }
}

impl Options {
    /// The launch directory, for the client's own file system work.
    pub fn launch_dir(&self) -> &Path {
        &self.cwd
    }

    /// Resolves the workspace every request this command sends names, once
    /// the options are complete. A prompt from `-p` is here at parse; one
    /// read from stdin arrives before this step, so a Chat that resumes a
    /// Session and runs headless is still known to name none. A command that
    /// names one is refused here, before the daemon is contacted, when the
    /// path is not valid UTF-8.
    pub fn resolve_workspace(&mut self) -> Result<()> {
        self.workspace = self
            .names_workspace()
            .then(|| Workspace::resolve(&self.cwd))
            .transpose()?;
        Ok(())
    }

    /// The workspace every request this command sends names, resolved once
    /// the options were complete. Only a command that names one reaches a
    /// request.
    pub fn workspace(&self) -> &Workspace {
        self.workspace
            .as_ref()
            .expect("a command that names the workspace resolved it")
    }

    /// The workspace a request names, for a command that names one.
    pub fn named_workspace(&self) -> Option<&str> {
        self.workspace.as_ref().map(Workspace::as_str)
    }

    /// Whether this command names the workspace in a request it sends. Such a
    /// command resolves it once its options are complete, so a path that is
    /// not valid UTF-8 is refused before the daemon is contacted.
    fn names_workspace(&self) -> bool {
        match &self.action {
            // A Chat that resumes a Session and runs headless names none: it
            // attaches, reads history and submits, and the composer never
            // runs. A new Chat or Session still does, and so does an
            // interactive one.
            Action::Chat => self.resume.is_none() || self.prompt.is_none(),
            Action::Tui => true,
            Action::Exec => self.resume.is_none(),
            Action::McpList
            | Action::McpLogin(_)
            | Action::McpLogout(_)
            | Action::McpEnable { .. }
            | Action::McpDisconnect(_) => true,
            Action::Management(request) => request.area.names_workspace(),
            Action::ManagementEdit { area, .. }
            | Action::ManagementSet { area, .. }
            | Action::ManagementAdd { area, .. }
            | Action::ManagementStart { area, .. } => area.names_workspace(),
            Action::Migration(_)
            | Action::Acp { .. }
            | Action::Export { .. }
            | Action::SessionRename { .. }
            | Action::SessionDelete { .. }
            | Action::SessionCleanup { .. }
            | Action::Timeline { .. }
            | Action::DaemonVacuum { .. }
            | Action::Login(_)
            | Action::Account(_)
            | Action::Logout(_)
            | Action::Models(_)
            | Action::Recording { .. }
            | Action::Plugin { .. }
            | Action::RuntimeList
            | Action::RuntimeInstall(_)
            | Action::RuntimeConfig { .. }
            | Action::DaemonStop
            | Action::Web { .. }
            | Action::WebInstall
            | Action::DefaultTerminalMode { .. }
            | Action::Help
            | Action::Version => false,
        }
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

#[cfg(test)]
mod workspace_tests {
    use super::*;

    /// Every parse that reaches a request takes the same step the CLI takes:
    /// a piped prompt, when the row has one, then `resolve_workspace`.
    #[test]
    fn parsed_commands_resolve_only_workspaces_their_requests_name() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let document = home.path().join("job.json");
        std::fs::write(&document, "{}").unwrap();
        let document = document.to_str().unwrap();
        // Each row is its arguments, the prompt a pipe would carry, and
        // whether its requests name the workspace.
        let cases = [
            (vec!["scheduled", "list"], None, false),
            (vec!["scheduled", "enable", "job-1"], None, false),
            (vec!["scheduled", "disable", "job-1"], None, false),
            (vec!["scheduled", "edit", "job-1"], None, false),
            (vec!["scheduled", "set", "job-1", document], None, false),
            (vec!["scheduled", "add", document], None, false),
            (vec!["scheduled", "remove", "job-1"], None, false),
            (vec!["scheduled", "run", "job-1"], None, false),
            (vec!["scheduled", "history", "job-1"], None, false),
            (
                vec!["exec", "resume", "session", "-p", "continue"],
                None,
                false,
            ),
            (
                vec!["exec", "--resume", "session", "-p", "continue"],
                None,
                false,
            ),
            (vec!["--resume", "session", "-p", "continue"], None, false),
            (vec!["--resume", "session"], Some("continue"), false),
            (vec!["--version"], None, false),
            (vec!["--resume", "session"], None, true),
            (vec!["-p", "start"], None, true),
            (vec!["exec", "-p", "start"], None, true),
            (vec!["tui"], None, true),
            (vec!["mcp", "list"], None, true),
            (vec!["mcp", "login", "docs"], None, true),
            (vec!["mcp", "logout", "docs"], None, true),
            (vec!["mcp", "enable", "docs"], None, true),
            (vec!["mcp", "disconnect", "docs"], None, true),
            (vec!["mcp", "edit", "docs"], None, true),
            (vec!["skills", "list"], None, true),
            (vec!["hooks", "list"], None, true),
            (vec!["hooks", "set", document], None, true),
            (vec!["workflow", "list"], None, true),
            (vec!["workflow", "start", "job"], None, true),
        ];
        for (arguments, piped, names_workspace) in cases {
            let args = ["--home", directory, "--cwd", directory]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(str::to_owned);
            let mut options =
                Options::parse(args).unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            // What `prepare` does with a prompt on stdin before it resolves
            // the workspace.
            if let Some(prompt) = piped {
                assert!(options.prompt.is_none(), "{arguments:?}");
                options.prompt = Some(prompt.to_owned());
            }
            options
                .resolve_workspace()
                .unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            assert_eq!(
                options.named_workspace(),
                names_workspace.then_some(directory),
                "{arguments:?}"
            );
            assert_eq!(options.launch_dir(), Path::new(directory));
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_workspace_that_is_not_utf8_is_refused() {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/dim-\xff-work"))
        };
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[0xd800]))
        };
        let error = Workspace::resolve(&path).unwrap_err();
        assert!(
            error.to_string().ends_with("must be valid UTF-8"),
            "{error}"
        );
    }
}
