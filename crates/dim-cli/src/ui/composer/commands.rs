//! The command palette registry: the actions the native interface runs,
//! grouped by category, in palette order. A slash command is also typed as
//! `/name`; the other entries are actions a key reaches (named in `key`),
//! listed so the palette teaches the key, as Amp's does.

#[derive(Debug)]
pub struct Command {
    /// The id `run_command` dispatches on; the typed name of a slash command.
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    /// Typed as `/name`; otherwise reached by its key or the palette only.
    pub slash: bool,
    /// The key that runs it, as the palette shows it.
    pub key: Option<&'static str>,
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
    /// The fullscreen interface; elsewhere the action is not listed.
    Fullscreen,
}

/// What the interface is doing, as the palette weighs availability.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub session: bool,
    pub running: bool,
    pub queued: bool,
    pub fullscreen: bool,
}

impl Command {
    /// Listed at all: fullscreen-only actions are left out elsewhere.
    pub fn listed(&self, context: Context) -> bool {
        self.needs != Needs::Fullscreen || context.fullscreen
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
        "history",
        "session",
        "load the previous page of messages",
        &[],
        Needs::Session,
    ),
    slash(
        "usage",
        "session",
        "show context and plan usage",
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
        "pick an earlier message to edit",
        "Tab",
        Needs::Fullscreen,
    ),
    slash(
        "tools",
        "view",
        "collapse or expand tool details and thoughts",
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
    slash(
        "approvals",
        "model",
        "configure tool approvals",
        &[],
        Needs::Nothing,
    ),
    slash("settings", "app", "open settings", &[], Needs::Nothing),
    slash("keys", "app", "show the key bindings", &[], Needs::Nothing),
    slash(
        "reconnect",
        "app",
        "reconnect to the App Server",
        &[],
        Needs::Nothing,
    ),
    slash(
        "suspend",
        "app",
        "suspend to the shell",
        &[],
        Needs::Nothing,
    ),
    Command {
        key: Some("Ctrl+C Ctrl+C"),
        ..slash("exit", "app", "exit", &["quit"], Needs::Nothing)
    },
];
