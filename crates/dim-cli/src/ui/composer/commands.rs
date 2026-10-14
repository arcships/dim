//! The scrollback command registry (generated from the reference build's
//! `listScrollbackCommands()`; edit deliberately).

#[derive(Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    pub requires_session: bool,
}

/// Registry order is the palette order. `tui` is not listed: the native
/// interface has no fullscreen form to switch to.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "remote",
        description: "Share this workspace with your phone",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "new",
        description: "Start a new session",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "resume",
        description: "Resume a previous session",
        aliases: &["sessions"],
        requires_session: false,
    },
    Command {
        name: "skills",
        description: "List loaded skills",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "reload-skills",
        description: "Rescan skills from disk",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "reload-plugins",
        description: "Rescan installed plugins",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "connect",
        description: "Connect a provider",
        aliases: &["provider"],
        requires_session: false,
    },
    Command {
        name: "models",
        description: "Select a model",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "fast",
        description: "Toggle fast mode",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "debug",
        description: "Debug diagnostics",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "settings",
        description: "Open settings",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "approvals",
        description: "Configure approvals",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "mcp",
        description: "MCP server status",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "agent",
        description: "Switch to agent mode",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "external-agents",
        description: "Manage external agents",
        aliases: &["agents"],
        requires_session: false,
    },
    Command {
        name: "plugins",
        description: "Plugin settings",
        aliases: &["plugin"],
        requires_session: false,
    },
    Command {
        name: "theme",
        description: "Switch theme",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "language",
        description: "Switch language",
        aliases: &["lang", "locale"],
        requires_session: false,
    },
    Command {
        name: "queue",
        description: "Manage queued messages",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "timeline",
        description: "Jump to a message",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "usage",
        description: "Show context and plan usage",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "stats",
        description: "Show session stats and activity",
        aliases: &["state"],
        requires_session: false,
    },
    Command {
        name: "export",
        description: "Export the provider request JSON",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "compact",
        description: "Trigger context compaction",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "goal",
        description: "Switch to goal mode",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "workflow",
        description: "Run or inspect workflows",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "workflow:save",
        description: "Save an inline workflow",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "exit",
        description: "Exit the app",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "tools",
        description: "Toggle tool details (collapsed or expanded)",
        aliases: &[],
        requires_session: false,
    },
    Command {
        name: "history",
        description: "Append the previous page of full message history",
        aliases: &[],
        requires_session: true,
    },
    Command {
        name: "keys",
        description: "Show application and terminal key bindings",
        aliases: &[],
        requires_session: false,
    },
];
