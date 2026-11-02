//! The command palette registry: the actions the native interface runs,
//! sorted by category and name when listed. A slash command is also typed as
//! `/name`; the other entries are actions reached only by their keys.

#[derive(Debug)]
pub struct Command {
    /// The id `run_command` dispatches on; the typed name of a slash command.
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    /// Typed as `/name`; otherwise reached only by its key.
    pub slash: bool,
    /// The key that runs it, as the palette shows it.
    pub key: Option<&'static str>,
    /// The argument a slash command takes inline (`<prompt>`): choosing it
    /// fills the composer instead of running it.
    pub argument: Option<&'static str>,
    /// Dispatch requires the exact name or alias; other matches only complete.
    pub requires_exact_name: bool,
    pub needs: Needs,
}

/// When an action can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Nothing,
    Session,
    /// A run in progress.
    Run,
    /// A queued message.
    Queue,
    /// The fullscreen interface; unavailable elsewhere.
    Fullscreen,
}

/// What the interface is doing, as the palette weighs availability.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub session: bool,
    pub running: bool,
    pub queued: bool,
    /// Whether fullscreen actions are available to the command dispatcher.
    pub fullscreen: bool,
    /// The Dim account is signed in: `/login` opens the account rather
    /// than a sign-in.
    pub signed_in: bool,
    /// The session's model advertises priority/default service tiers:
    /// `/fast` is listed.
    pub service_tiers: bool,
}

impl Command {
    /// What the row says the command does, in the current state.
    pub fn describe(&self, context: Context) -> &'static str {
        if self.name == "login" && context.signed_in {
            "DimAgent account · usage · sign out"
        } else {
            self.description
        }
    }

    /// Whether the menu offers this command in the current context.
    pub fn listed(&self, context: Context) -> bool {
        if !self.slash {
            return false;
        }
        if self.name == "fast" {
            return context.service_tiers;
        }
        true
    }

    pub fn available(&self, context: Context) -> bool {
        match self.needs {
            Needs::Nothing => true,
            Needs::Session => context.session,
            Needs::Run => context.running,
            Needs::Queue => context.queued,
            Needs::Fullscreen => context.fullscreen,
        }
    }
}

const fn slash(
    name: &'static str,
    category: &'static str,
    description: &'static str,
    aliases: &'static [&'static str],
    needs: Needs,
) -> Command {
    Command {
        name,
        category,
        description,
        aliases,
        slash: true,
        key: None,
        argument: None,
        requires_exact_name: false,
        needs,
    }
}

const fn action(
    name: &'static str,
    category: &'static str,
    description: &'static str,
    key: &'static str,
    needs: Needs,
) -> Command {
    Command {
        name,
        category,
        description,
        aliases: &[],
        slash: false,
        key: Some(key),
        argument: None,
        requires_exact_name: false,
        needs,
    }
}

pub const COMMANDS: &[Command] = &[
    slash("new", "session", "start a new session", &[], Needs::Nothing),
    slash(
        "resume",
        "session",
        "resume a previous session",
        &["sessions"],
        Needs::Nothing,
    ),
    slash(
        "web",
        "session",
        "open this session in the browser",
        &[],
        Needs::Nothing,
    ),
    slash(
        "tasks",
        "session",
        "view command output or interrupt a member",
        &["background"],
        Needs::Session,
    ),
    slash(
        "timeline",
        "session",
        "jump to a message of the shown line",
        &[],
        Needs::Session,
    ),
    slash(
        "lines",
        "session",
        "follow, talk to and end this session's lines",
        &[],
        Needs::Session,
    ),
    Command {
        argument: Some("<prompt>"),
        ..slash(
            "branch",
            "session",
            "open a line from the end of the shown line",
            &[],
            Needs::Session,
        )
    },
    slash(
        "fork",
        "session",
        "fork the session at its last finished turn",
        &[],
        Needs::Session,
    ),
    Command {
        argument: Some("<title>"),
        ..slash(
            "rename",
            "session",
            "rename this session",
            &[],
            Needs::Session,
        )
    },
    slash(
        "usage",
        "session",
        "show the context window usage",
        &[],
        Needs::Session,
    ),
    slash(
        "compact",
        "session",
        "compact the context of the shown line",
        &[],
        Needs::Session,
    ),
    slash(
        "goal",
        "session",
        "follow or change the shown line's goal",
        &[],
        Needs::Session,
    ),
    action("interrupt", "run", "interrupt the run", "Esc", Needs::Run),
    action(
        "steer",
        "run",
        "steer with the next queued message",
        "Ctrl+S",
        Needs::Queue,
    ),
    action(
        "scroll-top",
        "view",
        "scroll to the top",
        "Home",
        Needs::Fullscreen,
    ),
    action(
        "scroll-bottom",
        "view",
        "scroll to the bottom",
        "End",
        Needs::Fullscreen,
    ),
    action(
        "pick-message",
        "view",
        "pick an earlier message to edit or quote",
        "Tab",
        Needs::Fullscreen,
    ),
    slash(
        "tools",
        "view",
        "collapse or expand tool details, thoughts and compaction summaries",
        &[],
        Needs::Nothing,
    ),
    slash(
        "tui",
        "view",
        "switch between fullscreen and scrollback",
        &[],
        Needs::Nothing,
    ),
    slash("theme", "view", "switch theme", &[], Needs::Nothing),
    slash(
        "language",
        "view",
        "switch language",
        &["lang", "locale"],
        Needs::Nothing,
    ),
    slash("models", "model", "select a model", &[], Needs::Nothing),
    slash(
        "connect",
        "model",
        "connect a provider",
        &["provider"],
        Needs::Nothing,
    ),
    slash("login", "model", "sign in to DimAgent", &[], Needs::Nothing),
    slash(
        "effort",
        "model",
        "show or set the reasoning effort",
        &["reasoning"],
        Needs::Nothing,
    ),
    slash(
        "fast",
        "model",
        "show or set the priority service tier (on|off)",
        &[],
        Needs::Nothing,
    ),
    slash(
        "agent",
        "model",
        "list agents or switch this session's agent",
        &[],
        Needs::Nothing,
    ),
    slash(
        "mcp",
        "model",
        "manage MCP servers, sign in or out (/mcp login ID)",
        &[],
        Needs::Nothing,
    ),
    slash(
        "plugins",
        "model",
        "list plugins, turn them on or off, reload (install PATH)",
        &["plugin"],
        Needs::Nothing,
    ),
    slash(
        "skills",
        "model",
        "list Skills, enable or disable one",
        &[],
        Needs::Nothing,
    ),
    slash(
        "hooks",
        "model",
        "list hooks, turn a source on or off, edit the user document",
        &[],
        Needs::Nothing,
    ),
    slash(
        "scheduled",
        "model",
        "list scheduled jobs, pause, resume, run or remove one",
        &["schedule"],
        Needs::Nothing,
    ),
    slash(
        "reload-skills",
        "model",
        "rescan the Skills of this workspace",
        &[],
        Needs::Nothing,
    ),
    slash(
        "permissions",
        "model",
        "configure file access, tool approval and process isolation, and manage command rules",
        &["approvals", "approval"],
        Needs::Nothing,
    ),
    slash(
        "runtime",
        "app",
        "show, install or configure the plugin runtimes",
        &[],
        Needs::Nothing,
    ),
    slash("settings", "app", "open settings", &[], Needs::Nothing),
    slash("keys", "app", "show the key bindings", &[], Needs::Nothing),
    Command {
        requires_exact_name: true,
        ..slash(
            "suspend",
            "app",
            "suspend to the shell",
            &[],
            Needs::Nothing,
        )
    },
    Command {
        key: Some("Ctrl+C Ctrl+C"),
        requires_exact_name: true,
        ..slash("exit", "app", "exit", &["quit"], Needs::Nothing)
    },
];
