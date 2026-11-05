//! The command palette registry: the actions the native interface runs,
//! listed in a curated order. A slash command is also typed as
//! `/name`; the other entries are actions reached only by their keys.

#[derive(Debug)]
pub struct Command {
    /// The id `run_command` dispatches on; the typed name of a slash command.
    pub name: &'static str,
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
    description: &'static str,
    aliases: &'static [&'static str],
    needs: Needs,
) -> Command {
    Command {
        name,
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
    description: &'static str,
    key: &'static str,
    needs: Needs,
) -> Command {
    Command {
        name,
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
    slash("new", "start a new session", &[], Needs::Nothing),
    slash(
        "resume",
        "resume a previous session",
        &["sessions"],
        Needs::Nothing,
    ),
    slash("models", "select a model", &[], Needs::Nothing),
    slash(
        "compact",
        "compact the context of the shown line",
        &[],
        Needs::Session,
    ),
    slash(
        "usage",
        "show the context window usage",
        &[],
        Needs::Session,
    ),
    slash(
        "effort",
        "show or set the reasoning effort",
        &["reasoning"],
        Needs::Nothing,
    ),
    slash(
        "fast",
        "show or set the priority service tier (on|off)",
        &[],
        Needs::Nothing,
    ),
    slash(
        "agent",
        "list agents or switch this session's agent",
        &[],
        Needs::Nothing,
    ),
    slash(
        "permissions",
        "choose the permission preset and manage command rules",
        &["approvals", "approval"],
        Needs::Nothing,
    ),
    slash("settings", "open settings", &[], Needs::Nothing),
    Command {
        argument: Some("<prompt>"),
        ..slash(
            "branch",
            "open a line from the end of the shown line",
            &[],
            Needs::Session,
        )
    },
    slash(
        "fork",
        "fork the session at its last finished turn",
        &[],
        Needs::Session,
    ),
    slash(
        "lines",
        "follow, talk to and end this session's lines",
        &[],
        Needs::Session,
    ),
    slash(
        "timeline",
        "jump to a message of the shown line",
        &[],
        Needs::Session,
    ),
    slash(
        "goal",
        "follow or change the shown line's goal",
        &[],
        Needs::Session,
    ),
    slash(
        "tasks",
        "view command output or interrupt a member",
        &["background"],
        Needs::Session,
    ),
    Command {
        argument: Some("<title>"),
        ..slash("rename", "rename this session", &[], Needs::Session)
    },
    slash(
        "web",
        "turn the web interface on or off",
        &[],
        Needs::Nothing,
    ),
    slash(
        "connect",
        "connect a provider",
        &["provider"],
        Needs::Nothing,
    ),
    slash("login", "sign in to DimAgent", &[], Needs::Nothing),
    slash(
        "skills",
        "list Skills, enable or disable one",
        &[],
        Needs::Nothing,
    ),
    slash(
        "reload-skills",
        "rescan the Skills of this workspace",
        &[],
        Needs::Nothing,
    ),
    slash(
        "mcp",
        "manage MCP servers, sign in or out (/mcp login ID)",
        &[],
        Needs::Nothing,
    ),
    slash(
        "plugins",
        "list plugins, turn them on or off, reload (install PATH)",
        &["plugin"],
        Needs::Nothing,
    ),
    slash(
        "runtime",
        "show, install or configure the plugin runtimes",
        &[],
        Needs::Nothing,
    ),
    slash(
        "hooks",
        "list hooks, turn a source on or off, edit the user document",
        &[],
        Needs::Nothing,
    ),
    slash(
        "scheduled",
        "list scheduled jobs, pause, resume, run or remove one",
        &["schedule"],
        Needs::Nothing,
    ),
    slash(
        "workflow",
        "list workflow definitions, start one or save a definition",
        &[],
        Needs::Nothing,
    ),
    slash(
        "workflow:save",
        "save a workflow definition for reuse",
        &[],
        Needs::Nothing,
    ),
    slash(
        "tools",
        "cycle tool details and compaction summaries",
        &[],
        Needs::Nothing,
    ),
    slash(
        "tui",
        "switch between fullscreen and scrollback",
        &[],
        Needs::Nothing,
    ),
    slash("theme", "switch theme", &[], Needs::Nothing),
    slash(
        "language",
        "switch language",
        &["lang", "locale"],
        Needs::Nothing,
    ),
    slash("keys", "show the key bindings", &[], Needs::Nothing),
    Command {
        key: Some("Ctrl+C Ctrl+C"),
        requires_exact_name: true,
        ..slash("exit", "exit", &["quit"], Needs::Nothing)
    },
    action("interrupt", "interrupt the run", "Esc", Needs::Run),
    action(
        "steer",
        "steer with the next queued message",
        "Ctrl+S",
        Needs::Queue,
    ),
    action("scroll-top", "scroll to the top", "Home", Needs::Fullscreen),
    action(
        "scroll-bottom",
        "scroll to the bottom",
        "End",
        Needs::Fullscreen,
    ),
    action(
        "pick-message",
        "pick an earlier message to edit or quote",
        "Tab",
        Needs::Fullscreen,
    ),
];
