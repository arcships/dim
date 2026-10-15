//! The contract between the terminal UI and the session worker.
//!
//! The UI sends [`Command`]s and drains [`Update`]s through [`SessionPort`];
//! the App Server behind it (`session_host`) is the only implementation.

use crate::model::view::{Answers, ViewSnapshot};

pub enum Command {
    Submit(String),
    /// Context for the model's next request that does not start a turn.
    Append(String),
    Stop,
    /// Answer an approval request.
    Approval {
        request_id: String,
        branch_id: String,
        allow: bool,
        remember: bool,
    },
    AskAnswers {
        request_id: String,
        branch_id: String,
        answers: Answers,
    },
    AskCancel {
        request_id: String,
        branch_id: String,
        reason: String,
    },
    /// Steer or withdraw a queued input.
    InputControl {
        input_id: String,
        action: &'static str,
    },
    /// Start a fresh session in the same directory.
    NewSession,
    /// Switch to a stored session.
    Resume(String),
    /// List stored sessions (newest first).
    ListSessions,
    /// Drop the connection and establish it again.
    Reconnect,
    /// List the models that can be chosen.
    ListModels,
    /// Produce the `/usage` report.
    Usage,
    /// Read the default tool-approval mode.
    GetApprovals,
    /// Set the default tool-approval mode.
    SetApprovals(String),
    /// List the providers of the `/connect` panel.
    ListProviders,
    /// Change one provider.
    Provider(ProviderAction),
    /// Begin a browser or device sign-in; `attempt` tags its updates.
    OauthStart(OauthStart),
    /// Abandon the sign-in in progress.
    OauthCancel,
    /// A callback URL the user pasted.
    OauthPaste(String),
    /// Make `(provider, model)` the default and the active session's model.
    SelectModel {
        provider_id: String,
        model_id: String,
    },
}

/// What a sign-in needs to begin.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthStart {
    pub attempt: u64,
    pub provider_id: String,
    pub device: bool,
    pub email: Option<String>,
}

/// How a sign-in is going.
#[derive(Debug, Clone, PartialEq)]
pub enum OauthEvent {
    /// Browser mode: open `url`; `opened` says whether the browser launched.
    Url {
        url: String,
        opened: bool,
    },
    Device {
        verification_uri: String,
        verification_uri_complete: Option<String>,
        user_code: String,
    },
    Exchanging,
    SlowDown(u64),
    /// Signed in, or why not (`None` error text = cancelled).
    Done(Result<String, Option<String>>),
}

/// One change to a provider.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderAction {
    Configure {
        provider_id: String,
        api_key: Option<String>,
        base_url: Option<String>,
        enabled: Option<bool>,
    },
    Disconnect(String),
}

/// One provider of the `/connect` list.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub status: String,
    pub is_current: bool,
    pub enabled: bool,
    pub custom: bool,
    pub base_url: Option<String>,
    pub api_key_preview: Option<String>,
}

/// One model the picker offers.
#[derive(Debug, Clone)]
pub struct ModelItem {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub display_name: String,
}

/// The models and which one is in effect.
#[derive(Debug, Clone, Default)]
pub struct ModelList {
    pub models: Vec<ModelItem>,
    pub current: Option<(String, String)>,
}

/// One stored session as the picker shows it.
#[derive(Debug, Clone)]
pub struct SessionItem {
    pub id: String,
    pub title: Option<String>,
    pub preview: Option<String>,
    pub cwd: String,
    pub updated_at: u64,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKind {
    New,
    Resumed,
}

pub enum Update {
    /// The session exists; `provider` / `model` label the status line.
    Ready {
        provider: String,
        model: String,
    },
    Snapshot(Box<ViewSnapshot>),
    /// How many files the run that just ended changed.
    FilesChanged(usize),
    /// The active session changed; snapshots that follow belong to it.
    Switched {
        session_id: String,
        kind: SwitchKind,
    },
    /// The stored sessions, or why they could not be listed.
    Sessions(Result<Vec<SessionItem>, String>),
    /// The connection was lost; `attempt` counts tries (1-based).
    Reconnecting {
        attempt: u32,
    },
    /// The connection is back and the missed events were applied.
    Reconnected,
    /// A command failed without ending the session.
    Notice(String),
    Models(Result<ModelList, String>),
    /// A transient status-line message.
    Toast(String),
    /// The default tool-approval mode (wire name).
    Approvals(Result<String, String>),
    Providers(Result<Vec<ProviderEntry>, String>),
    /// What a provider change came to.
    ProviderResult(Result<String, String>),
    /// Progress of the sign-in tagged `attempt`.
    Oauth {
        attempt: u64,
        event: OauthEvent,
    },
    /// A report to commit as a status block; `warning` colours it.
    Report {
        text: String,
        warning: bool,
    },
    /// The session cannot continue.
    Failed(String),
}

/// The seam between the interactive UI and the session worker.
///
/// The UI sends [`Command`]s and drains [`Update`]s through this and nothing
/// else, so the worker behind it can be replaced or faked without touching a
/// widget.
pub trait SessionPort {
    /// Queues a command for the worker.
    fn send(&self, command: Command);
    /// Everything the worker published since the last call.
    fn drain(&self) -> Vec<Update>;
    /// Closes the connection and gives the worker a moment to wind down.
    fn shutdown(self: Box<Self>);
    fn submit(&self, text: String) {
        self.send(Command::Submit(text));
    }
    fn stop(&self) {
        self.send(Command::Stop);
    }
}
