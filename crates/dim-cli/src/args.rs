use crate::{Result, export::Format, permission};
use dim_agent_types::PermissionPreset;
use std::path::{Path, PathBuf};

/// What `--server` must name, shared by every error about the value it
/// supplies.
pub(crate) const SERVER_REQUIREMENT: &str = "--server must name a standalone dim-app-server binary";

pub const HELP: &str = r#"dimcode [OPTIONS]

Pure Rust scrollback client. With redirected stdin, run one prompt and exit.

Commands:
  update [--confirm-legacy-exited]
                                  Install the latest version on this channel
  tui [-p TEXT] [--image PATH ...]
                                  Open the interactive scrollback client;
                                  -p sends TEXT as its first message
  acp [--agent ID]                Serve ACP over stdin/stdout
  exec [-p TEXT] [--image PATH ...] [--json] [--permission PRESET]
                                  Run text and/or local images and exit
  exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]

  session export SESSION_ID --format markdown|json [--output PATH]
  session rename SESSION TITLE    Set the manual title; --clear removes it
  session delete SESSION          Permanently delete a Session
  session cleanup (--older-than DURATION | --keep N) [--yes]
                                  Show the Sessions that will go, then
                                  delete them after confirmation
  worktree list                   List the managed worktrees under the
                                  product home; a row names TASK_ID, ROOT,
                                  BRANCH, HEAD, CHANGED, UNTRACKED,
                                  LAST USED, SESSIONS and STATE
  worktree remove TASK_ID [--force]
                                  Remove one managed worktree and its
                                  record; --force removes uncommitted or
                                  untracked content too
  worktree prune (--older-than DURATION | --keep N) [--yes] [--force]
                                  Show the unused worktrees that will go
                                  and why the rest stay, then remove them
                                  after confirmation; --json prints the
                                  preview, and with --yes the applied reply
  export SESSION_ID [--output PATH] [--database PATH]
                                  Write a Session's whole Timeline as JSONL;
                                  it holds prompts, code and tool output.
                                  --database defaults to the home's
                                  rust/dimcode.sqlite

  login [PROVIDER] [--directory URL] [--device] [--redirect-uri URI]
                                  Sign in to a Provider the daemon describes;
                                  without PROVIDER, list the accounts
                                  it describes
  account PROVIDER                Show non-secret account status
  logout PROVIDER                 Sign out of an account provider
  models PROVIDER [--refresh | --clear-preferences MODEL
                  | --set-limits MODEL --context-window N --max-output-tokens N
                  | --set-missing-limits --context-window N
                    --max-output-tokens N]
                                  List a Provider's models; --refresh reloads
                                  the area's list
  models PROVIDER enable|disable|remove|inspect MODEL
                                  Enable or disable one model, remove it,
                                  or print its descriptor
  models PROVIDER edit MODEL      Edit the model's fields in the editor
  models PROVIDER set MODEL PATH  Write the model's fields from PATH,
                                  or from stdin for -
  models PROVIDER add PATH        Create a model from PATH, or from stdin
                                  for -

  mcp list                        List MCP servers and their enablement
  mcp login ID [--no-browser] [--client-secret]
                                  Sign in; secrets use hidden terminal input
  mcp logout ID                   Sign out and report token revocation
  mcp enable ID                   Enable an MCP server shown by mcp list
  mcp disable ID                  Disable it; an unlisted ID withdraws only
                                  this workspace's stored enablement
  mcp add PATH                    Create an entry from PATH, or from stdin for -
  mcp import PATH [--yes]         Preview a standard mcpServers document; with
                                  --yes, import the entries the flags select:
                                  --replace ID, --rename OLD=NEW and --only ID.
                                  --source user|project picks the writable
                                  source (default: user)
  mcp import --from ID [--yes]    The same preview and import from one known
                                  store of another local client
  mcp import --discover [--cwd DIR]
                                  List those stores with their states; no
                                  value is printed
  mcp edit ID                     Edit an entry's patch in the editor
  mcp set ID PATH                 Write an entry's patch from PATH,
                                  or from stdin for -
  mcp remove ID                   Delete an entry
  mcp test ID                     Probe a stored entry and list its tools
  mcp inspect ID                  Show an entry's effective configuration
  mcp disconnect ID               Close an entry's live connections
  mcp ... ID [--source SOURCE]    Every mcp command that names one stored
                                  entry takes it: user, project or plugin, as
                                  mcp list prints it; an ID more than one
                                  source lists needs it

  skills list                     List Skills, their sources and states
  skills enable|disable ID        Turn one Skill on or off
  skills reload                   Rescan this workspace's Skills
  hooks list                      List hook handlers, their sources and states
  hooks enable                    Run this workspace's .dimcode/hooks.json
  hooks disable                   Stop running this workspace's hooks
  hooks edit                      Edit the user's hook document
  hooks set PATH                  Write the user's hook document from PATH,
                                  or from stdin for -
  prompt                          List the built-in prompt overrides
  prompt show NAME                Print one override (base, summary, skills)
  prompt edit NAME                Edit one override in the editor; an
                                  empty draft writes nothing
  prompt set NAME PATH            Write one override from PATH, or stdin for -
  prompt reset NAME               Remove one override

  scheduled list                  List scheduled jobs, their windows and
                                  last runs
  scheduled enable|disable ID     Pause or resume one job
  scheduled edit ID               Edit a job document in the editor
  scheduled set ID PATH           Write a job document from PATH,
                                  or from stdin for -
  scheduled add PATH              Create a job from PATH, or from stdin for -
  scheduled remove ID             Delete a job
  scheduled run ID                Run a job now
  scheduled history ID            List a job's recent runs
  permissions list                List the user's command rules
  permissions add PATH            Add one rule from PATH, or from stdin for -
  permissions remove RULE         Remove a rule, given as one quoted
                                  'DECISION MATCH COMMAND'
  workflow list                   List workflow definitions, their sources
                                  and states
  workflow enable|disable NAME    Turn a project definition on or off
  workflow edit NAME              Edit a user definition in the editor
  workflow set NAME PATH          Write a user definition from PATH,
                                  or from stdin for -
  workflow add PATH               Create a definition from PATH,
                                  or from stdin for -
  workflow start NAME [PATH]      Start a definition with parameters from PATH
  workflow history NAME           List a definition's recent runs

  modality                        List every Modality category and
                                  operation with its availability, its
                                  reasons and its default and effective
                                  model; --json prints the snapshot
  modality set-default <category>[.<operation>] <provider>/<model>
                                  Set one operation's default model
  modality set-default <category>[.<operation>] --clear
                                  Clear it, restoring automatic resolution
  modality task list [--state S] [--category C] [--limit N] [--after ID]
                                  List Modality tasks, newest first
  modality task status ID         Print a task's state, progress and files
  modality task wait ID [--out DIR]
                                  Wait for a task; write its results with --out
  modality task cancel ID         Cancel a task
  image generate|edit PROMPT [FILE...] [--mask PATH]
  image enhance MODE [FILE...]    Submit an image task
  video generate|edit|enhance PROMPT [FILE...]
  speech synthesize TEXT
  speech transcribe FILE
  music|audio|motion generate PROMPT
  asset generate PROMPT [FILE...]
                                  Submit a task and write its results to
                                  --out (the current directory by default);
                                  --no-wait prints the task id only
  ocr FILE [--output-mode text|layout] [--out DIR]
                                  Recognize the text of an image or PDF;
                                  a short text prints inline and writes
                                  nothing, a longer one is written
                                  --param KEY=VALUE passes one adapter
                                  parameter: JSON when it parses as JSON,
                                  a string otherwise, so a string that
                                  would parse as JSON is written quoted
                                  (--param 'label="true"')
  recording [on|off]              Record model requests to the home's
                                  model-log/recordings.jsonl; prompts, code
                                  and tool output are written there
  realtime [--operation voice] [--model PROVIDER/MODEL]
                                  Open a realtime session; stdin lines are
                                  text frames and events print as JSON lines

  plugin list                     List plugins by name, the one in use
                                  and its status
  plugin install PATH|GIT_URL [--ref R] [--id ID] [--no-enable]
                                  Install a plugin and turn it on
  plugin install --optional NAME [--id ID] [--no-enable]
                                  Install an optional plugin the release ships
  plugin enable NAME [--use ID|builtin|default]
  plugin disable NAME
  plugin reload NAME              Reload a plugin after editing it
  plugin update ID                Load the latest commit of a Git plugin
  plugin remove ID
  runtime list                    List the plugin runtimes and their state
  runtime install node|python|uv  Turn a plugin runtime on and acquire it
  runtime config NAME             Set a runtime's --source or --auto
                                  and apply it

  daemon stop                     Gracefully stop an existing daemon
  daemon vacuum [--database PATH] Rewrite an offline database to reclaim
                                  space; --database defaults to the home's
                                  rust/dimcode.sqlite
  migration [run|inspect] [--source OLD_HOME]
                                  Import or inspect Global and CN separately;
                                  Global stays in ~/.dim, CN in ~/.dimcode;
                                  confirm stopped clients or use
                                  --source-stopped
  migration list|show TASK|cancel TASK
                                  Read tasks or request cancellation
  migration retry TASK [--source OLD_HOME]
                                  Resume a task, reconciling imported objects
  web [--port N] [--no-open]      Hold the Web workspace until Ctrl+C
  web --off                       Turn off the daemon-owned Web switch
  web install                     Install this version's Web workspace

Options:
  -p, --prompt TEXT               The prompt: exec runs it and exits; tui
                                  sends it as the first message of an
                                  interactive session
  --image PATH                    Add a local image to exec, or to the tui
                                  first message (repeatable)
  --resume SESSION_ID             Attach and continue a persisted session
  --home PATH                     Product home (default: ~/.dimcode)
  --cwd PATH                      Workspace (default: current directory)
  --no-tui                        Line client instead of the terminal UI
  --fullscreen | --scrollback     Terminal UI mode; append 'default' to save it
  --server PATH                   Run a standalone dim-app-server binary;
                                  the default runs this executable's
                                  app-server subcommand
  --model-config PATH             Explicit API-key model configuration
  --model ID [--provider ID]      Temporary model selection (also on resume)
  --effort LEVEL                  Temporary reasoning effort (also on resume)
  --autocompact auto|TOKENS       Temporary compaction window (k/m suffixes)
  --permission PRESET             read-only, workspace-write or full-access
                                  for a new session, or for the resumed main
                                  branch
  --worktree                      Start a new Session in a managed worktree
                                  of the workspace's repository, under the
                                  product home's worktrees directory
  --worktree-base REF             The commit that worktree starts from
                                  (default: HEAD)
  --worktree-branch NAME          The branch its creation makes (default:
                                  dim/TASK_ID); an existing branch is refused
  -h, --help                      Show this help without starting a daemon
  -V, --version                   Show version without starting a daemon

Model startup:
  --model ID overrides DIM_MODEL; --provider optionally names a configured
    Provider. A bare ID uses the effective Provider or an unambiguous match.
  --effort LEVEL overrides DIM_EFFORT_LEVEL, then the model's saved effort.
    LEVEL must be supported by the selected Provider model; service tier is
    preserved and the override is not saved as a model preference.
  DIM_BASE_URL + DIM_API_FORMAT select a Session-local direct connection.
    Both are required; they conflict with --provider and --model-config.
    A new Session also requires --model or DIM_MODEL; resume can retain its ID.
    DIM_API_FORMAT: anthropic, openai-chat, openai-responses, openai-compatible,
      gemini, xai or xai-responses. DIM_BASE_URL may include a gateway prefix.
  DIM_API_KEY uses x-api-key (anthropic), x-goog-api-key (gemini), or Bearer
    (other formats). DIM_AUTH_TOKEN uses Bearer for any format. Set at most one.
  DIM_CUSTOM_HEADERS is newline-separated Name: Value, split at the first colon.
    Names are case-insensitive; duplicates and generated auth conflicts fail.
    Credentials and headers require direct mode; anonymous gateways are allowed.
  DIM_MAX_CONTEXT_TOKENS and DIM_MAX_OUTPUT_TOKENS declare positive capacities
    up to 9007199254740991; the request output allowance is derived from them.
  --autocompact overrides DIM_AUTO_COMPACT_WINDOW: auto or a positive count,
    optionally suffixed k/m (1000/1000000). This is not the model capacity.
  Effort, autocompact and capacity startup controls require the product Provider
    and cannot be used with --model-config; set API-key generation settings
    in that configuration file instead.
  Model overrides with --model-config require --provider, including on resume.
  Startup overrides apply before the first input, including on resume, without
    changing saved Providers, preferences, defaults or the daemon environment.
    Explicit overrides force complete configuration inheritance for new work.
    DIM_* startup inputs are ignored by help, version and management commands.

Examples (keep secrets in the environment, not command arguments):
  dimcode exec --model model-id --effort high --autocompact 128k -p 'Hello'
  DIM_BASE_URL=https://gateway.example/prefix DIM_API_FORMAT=openai-chat \
    DIM_MODEL=model-id dimcode exec -p 'Hello'
  dimcode exec resume SESSION_ID --effort low --autocompact auto -p 'Continue'

API-key configuration:
  To supply an API connection, use --model-config PATH with a JSON file:
    defaultModel: {providerId, modelId}; providers: [{id, kind, baseUrl,
      apiKeyEnv, headers, models: [{id, contextWindow, maxOutputTokens,
      config}]}].
  kind selects the API format, not the model name: anthropic, openai-responses,
    openai-chat, openai-compatible, gemini, xai or xai-responses.
  baseUrl is the API base URL; the selected format supplies the request path.
  apiKeyEnv names an environment variable, not the key itself. Keys use
    x-api-key for anthropic, x-goog-api-key for gemini, or Bearer otherwise.
  headers is a JSON object for additional request headers, including gateway
    authentication when needed. Keep credentials out of command arguments.
  Each model requires contextWindow, maxOutputTokens and config.maxTokens:
    the total context capacity, output capacity and request output budget.
  The daemon inherits its launch environment; a later client's environment
    does not update it. Use a separate --home for a different model config.
  Example: dimcode --home /path/to/isolated-home --model-config model.json

Interaction:
  Ctrl+C stops a running turn and waits for settlement;
    idle Ctrl+C or EOF exits.
  Without -p, exec reads redirected stdin through EOF, including with --image.
  Idle commands: /export <markdown|json> PATH, /rename TITLE|--clear, /exit.
  PATH is relative to the CLI current directory; ~ is literal.

  /permissions (aliases /approvals, /approval) selects a permission preset
    in the terminal UI, with Read only, Workspace write and Full access
    choices, and a Command rules row. Process isolation is in Settings.
  The line prompt accepts PRESET, allow|deny exact|prefix COMMAND, or remove N.
  Approval accepts only y/yes; empty input denies. For a simple exec command,
    a/always also saves a command rule. Noninteractive approval exits 3.
  The terminal UI offers a newer version on its npm channel
    (alpha, beta or latest), installs it with the package manager it came from
    and restarts onto the session;
    DIMCODE_UPDATE_CHECK=0 skips that check.
  The terminal UI reports the run's state to the terminal (OSC 7501);
    DIM_TERMINAL_STATUS=off turns that off.
  A terminal that answers the startup probe's OSC 7501 query draws that record
    itself, so the window title stops turning there;
    DIM_TERMINAL_TITLE=always keeps its mark.
  It also draws a progress bar where the terminal is known to (OSC 9;4);
    DIM_TERMINAL_PROGRESS=off turns that off, =on draws it in any terminal.
  Questions take option numbers (comma separated for several) or 0 to type your
    own; empty input on the first question declines. Noninteractive questions
    exit 3.

  Account commands sign in to every Provider the daemon describes with a login
    method. A build without the Dim account provider says so and offers API-key
    Providers instead.

Exit codes:
  0 success, 1 configuration/transport error, a delete that removed nothing,
  a cleanup in which a delete failed or a result file that already exists,
  2 model/turn failure or a failed Modality task,
  3 interactive approval or answer required, 130 interrupted.
"#;

/// What `dimcode modality task` names.
const USAGE_TASK: &str = "Use modality task list | status ID | wait ID [--out DIR] | cancel ID";

/// What `dimcode modality` names.
const USAGE_MODALITY: &str = "Use modality, modality set-default \
<category>[.<operation>] <provider>/<model> | --clear, or modality task";

/// What `dimcode session` names besides `export`.
const USAGE_SESSION: &str =
    "Use session export, session rename, session delete or session cleanup; see --help";

/// `mcp import PATH`, `mcp import --from ID` or `mcp import --discover`:
/// preview a standard `mcpServers` document and, with `--yes`, apply the
/// entries the flags select. With `--from`, the owner reads one known store of
/// another local client and its text never reaches the client. Every status
/// and refusal comes from the owner.
#[derive(Clone, Debug)]
pub struct McpImport {
    /// The document, or `-` for stdin; absent with `--from` and `--discover`.
    pub path: Option<String>,
    /// List the known stores instead of previewing one.
    pub discover: bool,
    /// One known store id instead of a file.
    pub from: Option<String>,
    /// Apply the selected entries after the preview.
    pub yes: bool,
    /// Entries whose resolution replaces the target source's entry.
    pub replace: Vec<String>,
    /// `OLD=NEW` resolutions that write the entry under a new id.
    pub rename: Vec<(String, String)>,
    /// The only ids the import accepts; every other entry is skipped.
    pub only: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Action {
    Chat,
    Tui,
    Exec,
    Update {
        legacy_exited: bool,
    },
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
    /// `worktree ...`: one command over the managed worktrees under the
    /// product home (#1451).
    Worktree(crate::worktree_command::Command),
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
    McpImport(McpImport),
    /// `<area> ...`: one request to a management area's owner.
    Management(crate::model::session::ManagementRequest),
    /// `prompt ...`: the user's overrides of the built-in policy texts.
    Prompt(crate::prompt_command::Command),
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
    /// `modality ...`: one Modality command (Modality A.5).
    Modality(crate::modality::Command),
    /// `realtime`: one realtime session, with the operation to open.
    Realtime {
        operation: Option<String>,
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

/// The flags a Modality command takes (Modality A.5).
#[derive(Clone, Debug, Default)]
pub struct ModalityFlags {
    /// `--out DIR`: where results are written; the current directory when
    /// absent.
    pub out: Option<PathBuf>,
    /// `--no-wait`: stop after the submission and print the id.
    pub no_wait: bool,
    /// `task list --state STATE`: one task state.
    pub state: Option<String>,
    /// `task list --category CATEGORY`: one category.
    pub category: Option<String>,
    /// `task list --limit N`: at most N tasks; the daemon's default otherwise.
    pub limit: Option<u64>,
    /// `task list --after ID`: the page after this task.
    pub after: Option<String>,
    /// `ocr --output-mode text|layout`: what `ocr.recognize` returns.
    pub output_mode: Option<String>,
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
    /// The flags of a Modality command (A.5).
    pub modality: ModalityFlags,
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
    /// `--worktree`: start the new Session in a managed worktree of the
    /// workspace's repository (#1451).
    pub worktree: bool,
    /// `--worktree-base REF`: the commit the worktree starts from.
    pub worktree_base: Option<String>,
    /// `--worktree-branch NAME`: the branch its creation makes.
    pub worktree_branch: Option<String>,
    pub directory: Option<String>,
    pub device: bool,
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
    /// The source an `mcp` command that names one stored entry settles:
    /// `user`, `project` or `plugin`, as `mcp list` prints it. Without it the
    /// id is that entry only when one source lists it.
    pub mcp_source: Option<String>,
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
            modality: ModalityFlags::default(),
            cwd: std::env::current_dir()?,
            workspace: None,
            server: None,
            model_config: None,
            provider: None,
            model: None,
            effort: None,
            startup: None,
            permission: None,
            worktree: false,
            worktree_base: None,
            worktree_branch: None,
            directory: None,
            device: false,
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
            mcp_source: None,
        };
        let mut args = args.peekable();
        let mut export_format = false;
        let mut autocompact = None;
        let mut source = None;
        while let Some(arg) = args.next() {
            let value = |args: &mut dyn Iterator<Item = String>| -> Result<String> {
                args.next()
                    .ok_or_else(|| format!("{arg} requires a value").into())
            };
            match arg.as_str() {
                "--confirm-legacy-exited" => {
                    let Action::Update { legacy_exited } = &mut options.action else {
                        return Err("--confirm-legacy-exited requires update".into());
                    };
                    *legacy_exited = true;
                }
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
                    // What the value names is decided once the command is
                    // known: a migration's old home, or the source an `mcp`
                    // command's entry is in. The flag may stand on either
                    // side of the command's own words.
                    if source.replace(value(&mut args)?).is_some() {
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
                "--clear" => match &mut options.action {
                    Action::SessionRename { clear, .. } => *clear = true,
                    Action::Modality(crate::modality::Command::SetDefault { clear, .. }) => {
                        *clear = true;
                    }
                    _ => {
                        return Err(
                            "--clear requires session rename or modality set-default".into()
                        );
                    }
                },
                "--out" => {
                    let Action::Modality(command) = &options.action else {
                        return Err("--out requires a submission, modality task wait or ocr".into());
                    };
                    if !matches!(
                        command,
                        crate::modality::Command::TaskWait { .. }
                            | crate::modality::Command::Ocr { .. }
                    ) {
                        return Err("--out requires a submission, modality task wait or ocr".into());
                    }
                    options.modality.out = Some(PathBuf::from(value(&mut args)?));
                }
                "--no-wait" => {
                    let Action::Modality(crate::modality::Command::Ocr { .. }) = &options.action
                    else {
                        return Err("--no-wait requires a submission or ocr".into());
                    };
                    options.modality.no_wait = true;
                }
                "--output-mode" => {
                    let Action::Modality(crate::modality::Command::Ocr { .. }) = &options.action
                    else {
                        return Err("--output-mode requires ocr".into());
                    };
                    let mode = value(&mut args)?;
                    if mode != "text" && mode != "layout" {
                        return Err(format!("--output-mode is text or layout, not {mode}").into());
                    }
                    options.modality.output_mode = Some(mode);
                }
                "--state" => {
                    let Action::Modality(crate::modality::Command::TaskList) = &options.action
                    else {
                        return Err("--state requires modality task list".into());
                    };
                    options.modality.state = Some(value(&mut args)?);
                }
                "--category" => {
                    let Action::Modality(crate::modality::Command::TaskList) = &options.action
                    else {
                        return Err("--category requires modality task list".into());
                    };
                    options.modality.category = Some(value(&mut args)?);
                }
                "--limit" => {
                    let Action::Modality(crate::modality::Command::TaskList) = &options.action
                    else {
                        return Err("--limit requires modality task list".into());
                    };
                    let limit = value(&mut args)?;
                    options.modality.limit =
                        Some(limit.parse().map_err(|_| {
                            format!("--limit takes a number of tasks, not {limit}")
                        })?);
                }
                "--after" => {
                    let Action::Modality(crate::modality::Command::TaskList) = &options.action
                    else {
                        return Err("--after requires modality task list".into());
                    };
                    options.modality.after = Some(value(&mut args)?);
                }
                "--older-than" => {
                    let age = value(&mut args)?;
                    match &mut options.action {
                        Action::SessionCleanup { older_than, .. } => {
                            if older_than.replace(parse_age(&age)?).is_some() {
                                return Err("Supply --older-than only once".into());
                            }
                        }
                        // The daemon parses the same grammar, so the text the
                        // flag was given is what it receives.
                        Action::Worktree(crate::worktree_command::Command::Prune {
                            older_than,
                            ..
                        }) => {
                            parse_age(&age)?;
                            if older_than.replace(age).is_some() {
                                return Err("Supply --older-than only once".into());
                            }
                        }
                        _ => {
                            return Err(
                                "--older-than requires session cleanup or worktree prune".into()
                            );
                        }
                    }
                }
                "--keep" => {
                    let count = parse_count("--keep", &value(&mut args)?)?;
                    match &mut options.action {
                        Action::SessionCleanup { keep, .. } => {
                            if keep.replace(count).is_some() {
                                return Err("Supply --keep only once".into());
                            }
                        }
                        Action::Worktree(crate::worktree_command::Command::Prune {
                            keep, ..
                        }) => {
                            if keep.replace(count).is_some() {
                                return Err("Supply --keep only once".into());
                            }
                        }
                        _ => return Err("--keep requires session cleanup or worktree prune".into()),
                    }
                }
                "--yes" => match &mut options.action {
                    Action::SessionCleanup { yes, .. } => *yes = true,
                    Action::Worktree(crate::worktree_command::Command::Prune { yes, .. }) => {
                        *yes = true;
                    }
                    Action::McpImport(import) => import.yes = true,
                    _ => {
                        return Err(
                            "--yes requires session cleanup, worktree prune or mcp import".into(),
                        );
                    }
                },
                "--force" => match &mut options.action {
                    Action::Worktree(
                        crate::worktree_command::Command::Remove { force, .. }
                        | crate::worktree_command::Command::Prune { force, .. },
                    ) => *force = true,
                    _ => return Err("--force requires worktree remove or worktree prune".into()),
                },
                "--discover" => {
                    let Action::McpImport(import) = &mut options.action else {
                        return Err("--discover requires mcp import".into());
                    };
                    import.discover = true;
                }
                "--from" => {
                    let Action::McpImport(import) = &mut options.action else {
                        return Err("--from requires mcp import".into());
                    };
                    if import.from.replace(value(&mut args)?).is_some() {
                        return Err("Supply --from only once".into());
                    }
                }
                "--replace" => {
                    let Action::McpImport(import) = &mut options.action else {
                        return Err("--replace requires mcp import".into());
                    };
                    import.replace.push(value(&mut args)?);
                }
                "--rename" => {
                    let given = value(&mut args)?;
                    let Action::McpImport(import) = &mut options.action else {
                        return Err("--rename requires mcp import".into());
                    };
                    let Some((old, new)) = given.split_once('=') else {
                        return Err("--rename takes OLD=NEW".into());
                    };
                    if old.trim().is_empty() || new.trim().is_empty() {
                        return Err("--rename takes OLD=NEW with both ids nonempty".into());
                    }
                    import.rename.push((old.to_owned(), new.to_owned()));
                }
                "--only" => {
                    let Action::McpImport(import) = &mut options.action else {
                        return Err("--only requires mcp import".into());
                    };
                    import.only.push(value(&mut args)?);
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
                "--worktree" => options.worktree = true,
                "--worktree-base" => {
                    if options.worktree_base.replace(value(&mut args)?).is_some() {
                        return Err("Supply --worktree-base only once".into());
                    }
                }
                "--worktree-branch" => {
                    if options.worktree_branch.replace(value(&mut args)?).is_some() {
                        return Err("Supply --worktree-branch only once".into());
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
                "update" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Update {
                        legacy_exited: false,
                    }
                }
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
                "worktree" if matches!(options.action, Action::Chat) => {
                    use crate::worktree_command::Command;
                    options.action = match args.next().as_deref() {
                        Some("list") => Action::Worktree(Command::List),
                        Some("remove") => Action::Worktree(Command::Remove {
                            task_id: value(&mut args)?,
                            force: false,
                        }),
                        Some("prune") => Action::Worktree(Command::Prune {
                            older_than: None,
                            keep: None,
                            yes: false,
                            force: false,
                        }),
                        _ => return Err(crate::worktree_command::USAGE.into()),
                    };
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
                "modality" if matches!(options.action, Action::Chat) => {
                    use crate::modality::Command;
                    let verb = match args.peek().map(String::as_str) {
                        Some(next) if !next.starts_with("--") => Some(value(&mut args)?),
                        _ => None,
                    };
                    options.action = Action::Modality(match verb.as_deref() {
                        None => Command::List,
                        Some("set-default") => {
                            let operation = value(&mut args)?;
                            // The model is the next word unless the default is
                            // being cleared by the flag the loop reads.
                            let model = match args.peek().map(String::as_str) {
                                Some(next) if !next.starts_with("--") => Some(value(&mut args)?),
                                _ => None,
                            };
                            Command::SetDefault {
                                operation,
                                model,
                                clear: false,
                            }
                        }
                        Some("task") => match value(&mut args)?.as_str() {
                            "list" => Command::TaskList,
                            "status" => Command::TaskStatus {
                                task_id: value(&mut args)?,
                            },
                            "wait" => Command::TaskWait {
                                task_id: value(&mut args)?,
                            },
                            "cancel" => Command::TaskCancel {
                                task_id: value(&mut args)?,
                            },
                            _ => return Err(USAGE_TASK.into()),
                        },
                        Some(_) => return Err(USAGE_MODALITY.into()),
                    });
                }
                "image" | "video" | "speech" | "music" | "audio" | "asset" | "motion"
                    if matches!(options.action, Action::Chat) =>
                {
                    let category = arg.clone();
                    let submit = crate::modality::media::parse(
                        &category,
                        &mut args,
                        &mut options.json,
                        &mut options.modality,
                    )?;
                    options.action = Action::Modality(crate::modality::Command::Submit(submit));
                }
                "ocr" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Modality(crate::modality::Command::Ocr {
                        file: PathBuf::from(value(&mut args)?),
                    });
                }
                "realtime" if matches!(options.action, Action::Chat) => {
                    options.action = Action::Realtime { operation: None };
                }
                "--operation" => {
                    let Action::Realtime { operation } = &mut options.action else {
                        return Err("--operation requires realtime".into());
                    };
                    if operation.replace(value(&mut args)?).is_some() {
                        return Err("Supply --operation only once".into());
                    }
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
                        "import" => {
                            // `--discover` and `--from` stand in place of the
                            // path, so only a value that is not a flag is one.
                            let path = match args.peek().map(|next| next.starts_with("--")) {
                                Some(false) => Some(value(&mut args)?),
                                _ => None,
                            };
                            Action::McpImport(McpImport {
                                path,
                                discover: false,
                                from: None,
                                yes: false,
                                replace: Vec::new(),
                                rename: Vec::new(),
                                only: Vec::new(),
                            })
                        }
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
                                source: None,
                            },
                        }),
                        "test" => Action::Management(ManagementRequest {
                            area: Area::Mcp,
                            action: ManagementAction::Run {
                                id: value(&mut args)?,
                                source: None,
                                document: None,
                                seed: None,
                            },
                        }),
                        "inspect" => Action::Management(ManagementRequest {
                            area: Area::Mcp,
                            action: ManagementAction::Inspect {
                                id: value(&mut args)?,
                                source: None,
                            },
                        }),
                        _ => {
                            return Err("Use mcp list, mcp add PATH, mcp import PATH [--yes] | --from ID [--yes] | --discover, mcp edit ID, mcp set ID PATH, mcp remove ID, mcp test ID, mcp inspect ID, mcp disconnect ID, mcp login ID, mcp logout ID, mcp enable ID or mcp disable ID".into());
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
                            source: None,
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
                "prompt" if matches!(options.action, Action::Chat) => {
                    use crate::prompt_command::{Command, field};
                    options.action = match args.next().as_deref() {
                        None => Action::Prompt(Command::List),
                        Some("show") => Action::Prompt(Command::Show {
                            field: field(&value(&mut args)?)?,
                        }),
                        Some("edit") => Action::Prompt(Command::Edit {
                            field: field(&value(&mut args)?)?,
                        }),
                        Some("set") => Action::Prompt(Command::Set {
                            field: field(&value(&mut args)?)?,
                            path: value(&mut args)?,
                        }),
                        Some("reset") => Action::Prompt(Command::Reset {
                            field: field(&value(&mut args)?)?,
                        }),
                        Some(other) => {
                            return Err(format!(
                                "Use prompt [show|edit|set|reset] NAME, or prompt alone to list; {other} is not a prompt command"
                            )
                            .into());
                        }
                    };
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
                                source: None,
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
                                source: None,
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
                                "remove" => ManagementAction::Remove { id, source: None },
                                "run" => ManagementAction::Run {
                                    id,
                                    source: None,
                                    document: None,
                                    seed: None,
                                },
                                _ => ManagementAction::Inspect { id, source: None },
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
                                source: None,
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
                                source: None,
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
                                source: None,
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
                                            source: None,
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
                                        ManagementAction::Remove { id, source: None }
                                    } else {
                                        ManagementAction::Inspect { id, source: None }
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
        // `--source` names a migration's old home, or the source an `mcp`
        // command's entry is in; the flag may stand on either side of the
        // command's own words, so it is settled now that both are known.
        if let Some(source) = source {
            if let Action::Migration(command) = &mut options.action {
                command.source = Some(PathBuf::from(source));
            } else if matches!(options.action, Action::McpImport(_)) {
                // An import writes one writable source; a plugin entry is
                // owned by its plugin.
                match source.as_str() {
                    "user" | "project" => options.mcp_source = Some(source),
                    other => {
                        return Err(format!(
                            "--source is user or project for mcp import, not {other}"
                        )
                        .into());
                    }
                }
            } else if names_one_mcp_entry(&options.action) {
                if crate::mcp::source(&source).is_none() {
                    return Err(format!("--source is user, project or plugin, not {source}").into());
                }
                options.mcp_source = Some(source);
            } else {
                return Err(
                    "--source requires migration, mcp import or an mcp command that names one entry"
                        .into(),
                );
            }
        }
        if let Action::McpImport(import) = &options.action {
            let selection =
                !(import.replace.is_empty() && import.rename.is_empty() && import.only.is_empty());
            if import.discover {
                if import.path.is_some() || import.from.is_some() {
                    return Err("Use one of PATH, --from ID and --discover".into());
                }
                if import.yes || selection {
                    return Err("--discover takes no --yes, --replace, --rename or --only".into());
                }
                if options.mcp_source.is_some() {
                    return Err("--source is for mcp import PATH or --from".into());
                }
            } else {
                if import.path.is_some() == import.from.is_some() {
                    return Err("Use one of PATH, --from ID or --discover".into());
                }
                if !import.yes && selection {
                    return Err("--replace, --rename and --only require mcp import --yes".into());
                }
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
        // `--model` names the realtime session's model, and is a chat option
        // for every other command.
        let model_is_chat_option =
            options.model.is_some() && !matches!(options.action, Action::Realtime { .. });
        if !matches!(options.action, Action::Chat | Action::Tui | Action::Exec)
            && (options.prompt.is_some()
                || options.resume.is_some()
                || model_is_chat_option
                || options.effort.is_some()
                || autocompact.is_some()
                || options.permission.is_some()
                || options.worktree
                || options.worktree_base.is_some()
                || options.worktree_branch.is_some())
        {
            return Err("Commands cannot be combined with chat options".into());
        }
        if (options.worktree_base.is_some() || options.worktree_branch.is_some())
            && !options.worktree
        {
            return Err("--worktree-base and --worktree-branch require --worktree".into());
        }
        if options.worktree && options.resume.is_some() {
            return Err(
                "--worktree creates a new Session and cannot be combined with --resume".into(),
            );
        }
        if options.json
            && !matches!(
                options.action,
                Action::Exec | Action::Migration(_) | Action::Modality(_) | Action::Worktree(_)
            )
        {
            return Err(
                "--json requires exec, migration, a modality command or a worktree command".into(),
            );
        }
        let images_for_tui = matches!(options.action, Action::Tui) && options.prompt.is_some();
        if !options.images.is_empty() && !matches!(options.action, Action::Exec) && !images_for_tui
        {
            return Err("--image requires exec or tui -p".into());
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
        if let Action::Worktree(crate::worktree_command::Command::Prune {
            older_than, keep, ..
        }) = &options.action
            && older_than.is_some() == keep.is_some()
        {
            return Err(crate::worktree_command::SELECTOR_USAGE.into());
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

    /// The `worktree` member `--worktree` asks for: `base` and `branch` only
    /// when their flags named them, and nothing at all without `--worktree`,
    /// so a creation that asks for no worktree sends none (#1451).
    pub(crate) fn creation_worktree(&self) -> Option<serde_json::Value> {
        if !self.worktree {
            return None;
        }
        let mut request = serde_json::Map::new();
        if let Some(base) = &self.worktree_base {
            request.insert("base".into(), serde_json::json!(base));
        }
        if let Some(branch) = &self.worktree_branch {
            request.insert("branch".into(), serde_json::json!(branch));
        }
        Some(serde_json::Value::Object(request))
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
            | Action::McpDisconnect(_)
            | Action::McpImport(_) => true,
            Action::Management(request) => request.area.names_workspace(),
            Action::ManagementEdit { area, .. }
            | Action::ManagementSet { area, .. }
            | Action::ManagementAdd { area, .. }
            | Action::ManagementStart { area, .. } => area.names_workspace(),
            Action::Modality(_)
            | Action::Prompt(_)
            | Action::Realtime { .. }
            | Action::Migration(_)
            | Action::Acp { .. }
            | Action::Export { .. }
            | Action::SessionRename { .. }
            | Action::SessionDelete { .. }
            | Action::SessionCleanup { .. }
            | Action::Worktree(_)
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
            | Action::Update { .. }
            | Action::DaemonStop
            | Action::Web { .. }
            | Action::WebInstall
            | Action::DefaultTerminalMode { .. }
            | Action::Help
            | Action::Version => false,
        }
    }
}

/// Whether `action` is an `mcp` command that names one stored entry, which is
/// what `--source` settles: remove, edit, set, test, inspect, disconnect,
/// login, logout, enable and disable.
fn names_one_mcp_entry(action: &Action) -> bool {
    use crate::model::session::{Area, ManagementAction};
    match action {
        Action::McpLogin(_)
        | Action::McpLogout(_)
        | Action::McpDisconnect(_)
        | Action::McpEnable { .. } => true,
        Action::ManagementEdit { area, .. } | Action::ManagementSet { area, .. } => {
            area == &Area::Mcp
        }
        Action::Management(request) => {
            request.area == Area::Mcp
                && matches!(
                    request.action,
                    ManagementAction::Remove { .. }
                        | ManagementAction::Run { .. }
                        | ManagementAction::Inspect { .. }
                )
        }
        _ => false,
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
            (vec!["mcp", "import", "document.json"], None, true),
            (vec!["mcp", "edit", "docs"], None, true),
            (vec!["skills", "list"], None, true),
            (vec!["hooks", "list"], None, true),
            (vec!["hooks", "set", document], None, true),
            (vec!["workflow", "list"], None, true),
            (vec!["workflow", "start", "job"], None, true),
            (vec!["realtime"], None, false),
            (vec!["realtime", "--operation", "transcribe"], None, false),
            (
                vec!["realtime", "--model", "step-plan/stepaudio-2.5-realtime"],
                None,
                false,
            ),
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

    /// `realtime [--operation voice] [--model PROVIDER/MODEL]`: the operation
    /// is optional (the dispatcher opens `voice`), the model is the CLI's
    /// usual reference, and `--operation` belongs to no other command.
    #[test]
    fn realtime_takes_one_operation_and_one_model() {
        let parse = |arguments: &[&str]| {
            Options::parse(arguments.iter().map(|argument| (*argument).to_owned()))
        };
        let options = parse(&["realtime"]).unwrap();
        assert!(
            matches!(options.action, Action::Realtime { operation: None }),
            "{:?}",
            options.action
        );
        let options = parse(&["realtime", "--operation", "transcribe"]).unwrap();
        let Action::Realtime { operation } = &options.action else {
            panic!("{:?}", options.action);
        };
        assert_eq!(operation.as_deref(), Some("transcribe"));
        let options = parse(&["realtime", "--model", "step-plan/stepaudio-2.5-realtime"]).unwrap();
        assert!(
            matches!(options.action, Action::Realtime { operation: None }),
            "{:?}",
            options.action
        );
        assert_eq!(
            options.model.as_deref(),
            Some("step-plan/stepaudio-2.5-realtime")
        );
        let error = parse(&["--operation", "transcribe"]).err().unwrap();
        assert_eq!(error.to_string(), "--operation requires realtime");
        let error = parse(&["realtime", "--operation", "voice", "--operation", "voice"])
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Supply --operation only once");
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

#[cfg(test)]
mod source_tests {
    use super::*;

    /// Every `mcp` command that names one stored entry takes `--source`; a
    /// command that names none refuses it, and so does a value `mcp list`
    /// never prints. A migration's `--source` still names its old home.
    #[test]
    fn every_mcp_command_that_names_one_entry_takes_a_source() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let patch = home.path().join("patch.json");
        std::fs::write(&patch, "{}").unwrap();
        let patch = patch.to_str().unwrap();
        let parse = |arguments: &[&str]| {
            let args = ["--home", directory, "--cwd", directory]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(str::to_owned);
            Options::parse(args)
        };
        for arguments in [
            vec!["mcp", "remove", "docs", "--source", "project"],
            vec!["mcp", "edit", "docs", "--source", "project"],
            vec!["mcp", "set", "docs", patch, "--source", "project"],
            vec!["mcp", "test", "docs", "--source", "project"],
            vec!["mcp", "inspect", "docs", "--source", "project"],
            vec!["mcp", "disconnect", "docs", "--source", "project"],
            vec!["mcp", "login", "docs", "--source", "project"],
            vec!["mcp", "logout", "docs", "--source", "project"],
            vec!["mcp", "enable", "docs", "--source", "project"],
            vec!["mcp", "disable", "docs", "--source", "project"],
            // The flag may stand before the command's own words.
            vec!["--source", "user", "mcp", "remove", "docs"],
        ] {
            let options =
                parse(&arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            assert!(
                matches!(options.mcp_source.as_deref(), Some("project" | "user")),
                "{arguments:?}"
            );
        }
        // The flag names one stored entry: a command that names none has
        // nothing for it to settle.
        for arguments in [
            vec!["mcp", "list", "--source", "user"],
            vec!["mcp", "add", patch, "--source", "user"],
        ] {
            assert!(parse(&arguments).is_err(), "{arguments:?}");
        }
        // An import takes the one writable source it writes.
        for arguments in [
            vec!["mcp", "import", patch, "--source", "user"],
            vec!["mcp", "import", patch, "--source", "project", "--yes"],
        ] {
            let options =
                parse(&arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            assert!(
                matches!(options.mcp_source.as_deref(), Some("project" | "user")),
                "{arguments:?}"
            );
        }
        assert!(parse(&["mcp", "import", patch, "--source", "plugin"]).is_err());
        // Its values are the sources `mcp list` prints.
        assert!(parse(&["mcp", "remove", "docs", "--source", "workspace"]).is_err());
        // A migration's `--source` still names the old home.
        let options = parse(&["migration", "run", "--source", directory]).unwrap();
        match options.action {
            Action::Migration(command) => {
                assert_eq!(command.source, Some(PathBuf::from(directory)));
            }
            other => panic!("expected a migration, not {other:?}"),
        }
    }

    /// `mcp import` takes its selection flags only with `--yes`: they decide
    /// what an apply writes, and a preview writes nothing. `--rename` is one
    /// `OLD=NEW` pair per flag.
    #[test]
    fn mcp_import_takes_its_selection_flags_with_yes() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let parse = |arguments: &[&str]| {
            let args = ["--home", directory, "--cwd", directory]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(str::to_owned);
            Options::parse(args)
        };
        let options = parse(&[
            "mcp",
            "import",
            "document.json",
            "--yes",
            "--source",
            "project",
            "--replace",
            "docs",
            "--rename",
            "notes=notes2",
            "--only",
            "docs",
        ])
        .unwrap();
        assert_eq!(options.mcp_source.as_deref(), Some("project"));
        match options.action {
            Action::McpImport(import) => {
                assert_eq!(import.path.as_deref(), Some("document.json"));
                assert!(!import.discover);
                assert!(import.from.is_none());
                assert!(import.yes);
                assert_eq!(import.replace, ["docs"]);
                assert_eq!(import.rename, [("notes".to_owned(), "notes2".to_owned())]);
                assert_eq!(import.only, ["docs"]);
            }
            other => panic!("expected an import, not {other:?}"),
        }
        for arguments in [
            vec!["mcp", "import", "document.json", "--replace", "docs"],
            vec!["mcp", "import", "document.json", "--rename", "notes=notes2"],
            vec!["mcp", "import", "document.json", "--only", "docs"],
            vec!["mcp", "import", "document.json", "--rename", "notes"],
            vec![
                "mcp",
                "import",
                "document.json",
                "--yes",
                "--rename",
                "=notes2",
            ],
        ] {
            assert!(parse(&arguments).is_err(), "{arguments:?}");
        }
    }

    /// `mcp import` reads one document: a path, a known store id or the list
    /// of stores. A discovery prints no value, so it takes no selection.
    #[test]
    fn mcp_import_takes_exactly_one_source() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let parse = |arguments: &[&str]| {
            let args = ["--home", directory, "--cwd", directory]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(str::to_owned);
            Options::parse(args)
        };
        let options = parse(&["mcp", "import", "--discover"]).unwrap();
        match options.action {
            Action::McpImport(import) => {
                assert!(import.discover);
                assert!(import.path.is_none());
                assert!(import.from.is_none());
                assert!(!import.yes);
            }
            other => panic!("expected an import, not {other:?}"),
        }
        let options = parse(&["mcp", "import", "--from", "codex:user", "--yes"]).unwrap();
        match options.action {
            Action::McpImport(import) => {
                assert!(!import.discover);
                assert!(import.path.is_none());
                assert_eq!(import.from.as_deref(), Some("codex:user"));
                assert!(import.yes);
            }
            other => panic!("expected an import, not {other:?}"),
        }
        for arguments in [
            vec!["mcp", "import"],
            vec!["mcp", "import", "document.json", "--from", "codex:user"],
            vec!["mcp", "import", "document.json", "--discover"],
            vec!["mcp", "import", "--from", "codex:user", "--discover"],
            vec!["mcp", "import", "--discover", "--yes"],
            vec!["mcp", "import", "--discover", "--only", "docs"],
            vec!["mcp", "import", "--discover", "--source", "user"],
        ] {
            assert!(parse(&arguments).is_err(), "{arguments:?}");
        }
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    /// `tui` takes the first prompt `-p` names, with images only for it;
    /// `exec` and the bare chat keep their one-shot prompt.
    #[test]
    fn a_tui_invocation_takes_a_first_prompt_and_images_only_with_it() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let parse = |arguments: &[&str]| {
            let args = ["--home", directory, "--cwd", directory]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(str::to_owned);
            Options::parse(args)
        };
        let options = parse(&["tui", "-p", "hello"]).unwrap();
        assert!(matches!(options.action, Action::Tui));
        assert_eq!(options.prompt.as_deref(), Some("hello"));
        let options = parse(&["tui", "--resume", "session", "-p", "hello"]).unwrap();
        assert_eq!(options.resume.as_deref(), Some("session"));
        assert_eq!(options.prompt.as_deref(), Some("hello"));
        let options = parse(&["tui", "-p", "hello", "--image", "one.png"]).unwrap();
        assert_eq!(options.images, [PathBuf::from("one.png")]);
        assert!(parse(&["tui", "--image", "one.png"]).is_err());
        assert!(parse(&["tui", "-p", "hello", "--json"]).is_err());
        let options = parse(&["-p", "hello"]).unwrap();
        assert!(matches!(options.action, Action::Chat));
        assert_eq!(options.prompt.as_deref(), Some("hello"));
        let options = parse(&["exec", "-p", "hello"]).unwrap();
        assert!(matches!(options.action, Action::Exec));
        assert_eq!(options.prompt.as_deref(), Some("hello"));
    }
}

#[cfg(test)]
mod worktree_tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Options> {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().canonicalize().unwrap();
        let directory = directory.to_str().unwrap();
        let args = ["--home", directory, "--cwd", directory]
            .into_iter()
            .chain(arguments.iter().copied())
            .map(str::to_owned);
        Options::parse(args)
    }

    fn command(arguments: &[&str]) -> crate::worktree_command::Command {
        match parse(arguments).unwrap().action {
            Action::Worktree(command) => command,
            other => panic!("expected a worktree command, not {other:?}"),
        }
    }

    /// `worktree list`, `remove TASK_ID [--force]` and
    /// `prune (--older-than DURATION | --keep N) [--yes] [--force]` reach
    /// their commands, and no other command takes their flags.
    #[test]
    fn the_worktree_commands_parse_to_their_own_fields() {
        assert!(matches!(
            command(&["worktree", "list"]),
            crate::worktree_command::Command::List
        ));
        match command(&["worktree", "remove", "4f2abc"]) {
            crate::worktree_command::Command::Remove { task_id, force } => {
                assert_eq!(task_id, "4f2abc");
                assert!(!force);
            }
            other => panic!("expected a removal, not {other:?}"),
        }
        match command(&["worktree", "remove", "4f2abc", "--force"]) {
            crate::worktree_command::Command::Remove { force, .. } => assert!(force),
            other => panic!("expected a removal, not {other:?}"),
        }
        // The prune selector keeps the flag's own text, which the daemon
        // parses with the same grammar.
        match command(&[
            "worktree",
            "prune",
            "--older-than",
            "12h",
            "--yes",
            "--force",
        ]) {
            crate::worktree_command::Command::Prune {
                older_than,
                keep,
                yes,
                force,
            } => {
                assert_eq!(older_than.as_deref(), Some("12h"));
                assert_eq!(keep, None);
                assert!(yes);
                assert!(force);
            }
            other => panic!("expected a prune, not {other:?}"),
        }
        match command(&["worktree", "prune", "--keep", "3"]) {
            crate::worktree_command::Command::Prune {
                older_than, keep, ..
            } => {
                assert_eq!(older_than, None);
                assert_eq!(keep, Some(3));
            }
            other => panic!("expected a prune, not {other:?}"),
        }
        assert_eq!(
            parse(&["worktree", "prune", "--older-than", "soon"])
                .err()
                .unwrap()
                .to_string(),
            "--older-than takes a number and a unit d, h or m, such as 30d"
        );
        // A word the namespace does not have, and the namespace alone, are
        // the same usage error.
        for arguments in [vec!["worktree"], vec!["worktree", "show"]] {
            assert_eq!(
                parse(&arguments).err().unwrap().to_string(),
                crate::worktree_command::USAGE,
                "{arguments:?}"
            );
        }
        // `--force` and `--yes` belong to worktree commands alone.
        assert_eq!(
            parse(&["session", "cleanup", "--keep", "1", "--force"])
                .err()
                .unwrap()
                .to_string(),
            "--force requires worktree remove or worktree prune"
        );
        assert_eq!(
            parse(&["worktree", "list", "--yes"])
                .err()
                .unwrap()
                .to_string(),
            "--yes requires session cleanup, worktree prune or mcp import"
        );
    }

    /// `worktree prune` takes exactly one selector, as `session cleanup`
    /// does: neither is a list, and both are refused.
    #[test]
    fn prune_takes_exactly_one_selector() {
        for arguments in [
            vec!["worktree", "prune"],
            vec!["worktree", "prune", "--older-than", "30d", "--keep", "2"],
        ] {
            assert_eq!(
                parse(&arguments).err().unwrap().to_string(),
                crate::worktree_command::SELECTOR_USAGE,
                "{arguments:?}"
            );
        }
        assert_eq!(
            parse(&[
                "worktree",
                "prune",
                "--older-than",
                "30d",
                "--older-than",
                "12h"
            ])
            .err()
            .unwrap()
            .to_string(),
            "Supply --older-than only once"
        );
        assert!(parse(&["worktree", "prune", "--older-than", "30d"]).is_ok());
        assert!(parse(&["worktree", "prune", "--keep", "0"]).is_ok());
    }

    /// `--json` prints a worktree command's reply, and stays a chat or
    /// command flag everywhere else.
    #[test]
    fn json_belongs_to_the_worktree_commands() {
        assert!(parse(&["worktree", "list", "--json"]).unwrap().json);
        assert!(
            parse(&["worktree", "remove", "4f2abc", "--json"])
                .unwrap()
                .json
        );
        assert_eq!(
            parse(&["skills", "list", "--json"])
                .err()
                .unwrap()
                .to_string(),
            "--json requires exec, migration, a modality command or a worktree command"
        );
    }

    /// `--worktree`, `--worktree-base REF` and `--worktree-branch NAME` ask
    /// for a managed worktree at creation, and no other command takes them.
    #[test]
    fn creation_flags_ask_for_a_managed_worktree() {
        let options = parse(&["tui", "--worktree"]).unwrap();
        assert!(options.worktree);
        assert_eq!(options.creation_worktree(), Some(serde_json::json!({})));
        // The bare chat entry is where the tui options live too.
        let options = parse(&["--worktree"]).unwrap();
        assert_eq!(options.creation_worktree(), Some(serde_json::json!({})));
        let options = parse(&[
            "exec",
            "-p",
            "hello",
            "--worktree",
            "--worktree-base",
            "main",
            "--worktree-branch",
            "dim/fix",
        ])
        .unwrap();
        assert_eq!(
            options.creation_worktree(),
            Some(serde_json::json!({"base": "main", "branch": "dim/fix"}))
        );
        // A creation that asks for no worktree sends none.
        assert_eq!(
            parse(&["exec", "-p", "hello"]).unwrap().creation_worktree(),
            None
        );
        assert_eq!(
            parse(&["--worktree-base", "main"])
                .err()
                .unwrap()
                .to_string(),
            "--worktree-base and --worktree-branch require --worktree"
        );
        assert_eq!(
            parse(&["tui", "--worktree", "--resume", "session"])
                .err()
                .unwrap()
                .to_string(),
            "--worktree creates a new Session and cannot be combined with --resume"
        );
        assert_eq!(
            parse(&["skills", "list", "--worktree"])
                .err()
                .unwrap()
                .to_string(),
            "Commands cannot be combined with chat options"
        );
        assert_eq!(
            parse(&[
                "tui",
                "--worktree",
                "--worktree-base",
                "a",
                "--worktree-base",
                "b"
            ])
            .err()
            .unwrap()
            .to_string(),
            "Supply --worktree-base only once"
        );
    }
}
