//! The hook document of one source (Hooks A.1) and the effective catalog of
//! one hook point (A.2, A.5).
use super::sources::{HookSourceId, HookSourceState, HookSourcesSnapshot};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

/// The document events this build runs. Every other event name is
/// `unsupported` and never runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    Notification,
}
impl HookEvent {
    pub const ALL: [Self; 8] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PermissionRequest,
        Self::PostToolUse,
        Self::PostToolUseFailure,
        Self::Stop,
        Self::Notification,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::Notification => "Notification",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.name() == name)
    }
    /// What a non-tool matcher is tested against; `None` for a tool event and
    /// for the events whose matcher is ignored.
    pub fn matcher_input(self) -> Option<&'static str> {
        match self {
            Self::SessionStart => Some("startup"),
            Self::Notification => Some("permission_prompt"),
            _ => None,
        }
    }
    /// The handler timeout when the document names none.
    pub fn default_timeout(self) -> Duration {
        Duration::from_secs(if self == Self::UserPromptSubmit {
            30
        } else {
            600
        })
    }
    /// Whether the event ignores its matcher.
    pub fn matcher_ignored(self) -> bool {
        matches!(self, Self::UserPromptSubmit | Self::Stop)
    }
}

/// What a handler does when its command fails, times out or is cancelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    #[default]
    Continue,
    Block,
}
impl OnFailure {
    pub fn name(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Block => "block",
        }
    }
}

/// The state `hooks.catalog` reports for one handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HookState {
    Ok,
    Unsupported,
    Invalid,
}

/// The user's document (Settings source `hooks`) or a project's
/// `.dimcode/hooks.json`, in the Claude Code and Codex shape.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HooksDocument {
    pub hooks: BTreeMap<String, Vec<HookGroup>>,
    pub disable_all_hooks: bool,
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HookGroup {
    pub matcher: Option<String>,
    pub hooks: Vec<HookHandlerSetting>,
}
/// One `hooks[].hooks[]` entry. Unknown fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HookHandlerSetting {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub command: Option<String>,
    pub command_windows: Option<String>,
    pub timeout: Option<u64>,
    pub on_failure: Option<OnFailure>,
    pub r#async: bool,
    pub args: Option<serde_json::Value>,
    #[serde(rename = "if")]
    pub condition: Option<serde_json::Value>,
}
impl HookHandlerSetting {
    fn state(&self, event_supported: bool) -> HookState {
        if !event_supported || self.args.is_some() || self.condition.is_some() {
            return HookState::Unsupported;
        }
        if self.kind.as_deref() != Some("command") {
            return if self.kind.is_none() {
                HookState::Invalid
            } else {
                HookState::Unsupported
            };
        }
        if self
            .command
            .as_deref()
            .is_none_or(|command| command.trim().is_empty())
            || self.timeout == Some(0)
        {
            return HookState::Invalid;
        }
        HookState::Ok
    }
}

/// How a matcher selects handlers (Hooks A.1).
enum Matcher {
    All,
    Names(Vec<String>),
    Pattern(regex::Regex),
}
impl Matcher {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        let value = value.unwrap_or("").trim();
        if value.is_empty() || value == "*" {
            return Ok(Self::All);
        }
        let exact = value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|'));
        if exact {
            return Ok(Self::Names(
                value
                    .split(['|', ','])
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ));
        }
        regex::Regex::new(value)
            .map(Self::Pattern)
            .map_err(|error| error.to_string())
    }
    fn matches(&self, name: &str, alias: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Names(names) => names.iter().any(|candidate| {
                candidate == name || alias.is_some_and(|alias| candidate == alias)
            }),
            Self::Pattern(pattern) => {
                pattern.is_match(name) || alias.is_some_and(|alias| pattern.is_match(alias))
            }
        }
    }
}

/// One handler of a hook point, in catalog order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookHandler {
    pub event: HookEvent,
    pub source: HookSourceId,
    /// The directory of the plugin whose document carries this handler, which
    /// its commands read as `DIM_PLUGIN_ROOT`; `None` for the user and project
    /// sources (A.3).
    pub plugin_root: Option<std::path::PathBuf>,
    /// The document command; with the plugin directory it is the handler's
    /// de-duplication key at one point (A.2).
    pub command: String,
    /// The Windows replacement, used on a Windows node.
    pub command_windows: Option<String>,
    pub timeout: Duration,
    pub on_failure: OnFailure,
    pub asynchronous: bool,
}
impl HookHandler {
    pub fn command(&self, windows: bool) -> &str {
        match (windows, &self.command_windows) {
            (true, Some(command)) => command,
            _ => &self.command,
        }
    }
}

/// One handler as `hooks.catalog` lists it, in source and document order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEntry {
    pub source: HookSourceId,
    pub event: String,
    pub matcher: Option<String>,
    pub command: Option<String>,
    pub timeout: u64,
    pub on_failure: &'static str,
    pub r#async: bool,
    pub state: HookState,
}

/// Whether any hook runs from any source: `disableAllHooks` in the user
/// document turns them all off.
fn effective(snapshot: &HookSourcesSnapshot) -> bool {
    !snapshot.sources.iter().any(|source| {
        source.id == HookSourceId::User
            && source
                .document
                .as_ref()
                .is_some_and(|user| user.disable_all_hooks)
    })
}

/// The handlers of `event` that run, in catalog order: source table order,
/// then document order; handlers sharing a command and a plugin directory run
/// once (A.2).
///
/// `name` is the tool name of a tool event and `alias` its migration alias;
/// `None` for an event whose matcher is an input of its own.
pub fn handlers(
    snapshot: &HookSourcesSnapshot,
    event: HookEvent,
    name: Option<&str>,
    alias: Option<&str>,
) -> Vec<HookHandler> {
    if !effective(snapshot) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut handlers = Vec::new();
    for source in &snapshot.sources {
        if !source.enabled || source.state != HookSourceState::Ok {
            continue;
        }
        let Some(document) = &source.document else {
            continue;
        };
        let Some(groups) = document.hooks.get(event.name()) else {
            continue;
        };
        for group in groups {
            let Ok(matcher) = Matcher::parse(group.matcher.as_deref()) else {
                continue;
            };
            let matches = match (event.matcher_ignored(), event.matcher_input()) {
                (true, _) => true,
                (_, Some(input)) => matcher.matches(input, None),
                (_, None) => match name {
                    Some(name) => matcher.matches(name, alias),
                    None => true,
                },
            };
            if !matches {
                continue;
            }
            for setting in &group.hooks {
                if setting.state(true) != HookState::Ok {
                    continue;
                }
                let Some(command) = setting.command.clone() else {
                    continue;
                };
                let plugin_root = match &source.id {
                    HookSourceId::Plugin(_) => source.path.clone(),
                    HookSourceId::User | HookSourceId::Project => None,
                };
                if !seen.insert((command.clone(), plugin_root.clone())) {
                    continue;
                }
                handlers.push(HookHandler {
                    event,
                    source: source.id.clone(),
                    plugin_root,
                    command,
                    command_windows: setting.command_windows.clone(),
                    timeout: setting
                        .timeout
                        .map(Duration::from_secs)
                        .unwrap_or_else(|| event.default_timeout()),
                    on_failure: setting.on_failure.unwrap_or_default(),
                    asynchronous: setting.r#async,
                });
            }
        }
    }
    handlers
}

/// Every handler the sources hold, for `hooks.catalog`, including the ones
/// that never run.
pub fn entries(snapshot: &HookSourcesSnapshot) -> Vec<HookEntry> {
    let mut entries = Vec::new();
    for source in &snapshot.sources {
        let Some(document) = &source.document else {
            continue;
        };
        for (event, groups) in &document.hooks {
            let supported = HookEvent::from_name(event).is_some();
            for group in groups {
                let invalid = Matcher::parse(group.matcher.as_deref()).is_err();
                for setting in &group.hooks {
                    let state = match (invalid, setting.state(supported)) {
                        (true, _) => HookState::Invalid,
                        (false, state) => state,
                    };
                    entries.push(HookEntry {
                        source: source.id.clone(),
                        event: event.clone(),
                        matcher: group.matcher.clone(),
                        command: setting.command.clone(),
                        timeout: setting.timeout.unwrap_or_else(|| {
                            HookEvent::from_name(event)
                                .map(HookEvent::default_timeout)
                                .unwrap_or(Duration::from_secs(600))
                                .as_secs()
                        }),
                        on_failure: setting.on_failure.unwrap_or_default().name(),
                        r#async: setting.r#async,
                        state,
                    });
                }
            }
        }
    }
    entries
}
