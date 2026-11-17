//! The codes EventHub answers, and the translation of the `sessions` codes
//! it passes through (design A.1–A.3, Appendix C).
use dim_app_server::plugin_files::PluginFileError;

/// A `sessions` call whose outcome is unknown: the call failed or its reply
/// could not be read, so whether the write happened is not known (design
/// §2.3, Appendix C).
pub const TRANSPORT: &str = "transport";

/// A refused EventHub call: the code of Appendix C and the message its caller
/// reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EventHubError {
    pub code: &'static str,
    pub message: String,
}

impl EventHubError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new("invalid_params", message)
    }

    pub fn invalid_event(message: impl Into<String>) -> Self {
        Self::new("invalid_event", message)
    }

    pub fn unknown_type(message: impl Into<String>) -> Self {
        Self::new("unknown_type", message)
    }

    pub fn unknown_source(message: impl Into<String>) -> Self {
        Self::new("unknown_source", message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new("storage", message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(TRANSPORT, message)
    }
}

impl From<PluginFileError> for EventHubError {
    fn from(error: PluginFileError) -> Self {
        // The change did not happen and the in-memory set is unchanged
        // (Appendix C's `storage`).
        Self::storage(error.to_string())
    }
}

/// A `sessions` error code as EventHub answers it. A code the design does not
/// name is `failed`, which is what every other refused input is.
pub fn sessions_code(code: &str) -> &'static str {
    match code {
        "session_closed" => "session_closed",
        "branch_missing" => "branch_missing",
        "branch_ended" => "branch_ended",
        "reloading" => "reloading",
        "rejected" => "rejected",
        "ambiguous" => "ambiguous",
        "publication_unknown" => "publication_unknown",
        _ => "failed",
    }
}

/// The message text of a `sessions` error envelope.
pub struct SessionsFailure {
    pub code: String,
    pub message: String,
}

impl SessionsFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
