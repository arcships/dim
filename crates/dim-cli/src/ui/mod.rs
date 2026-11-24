//! Terminal UI: screen, input, panels and rendering.
//!
//! Depends on the view model in `model` and the session port in
//! `model::session`; it never reads wire types, CLI options or the
//! App Server implementation.

use std::path::PathBuf;

/// The first message the invocation supplied (`-p`), with the images it
/// attached: text and images are already validated and loaded at the CLI
/// boundary, so the interface only submits them.
#[derive(Clone)]
pub struct Initial {
    pub text: String,
    pub images: Vec<crate::images::Attached>,
}

/// What the composition root tells the UI about its surroundings.
#[derive(Clone)]
pub struct Env {
    /// Product home for saved settings and input history.
    pub home: PathBuf,
    /// The workspace the session works in, as every request names it.
    pub workspace: String,
    /// Start in the fullscreen interface instead of scrollback.
    pub fullscreen: bool,
    /// The Session the invocation asked to resume: only its first message
    /// (`-p`) names it while its first attachment has not loaded, and a key
    /// pressed on the Home screen before that reply is Home's.
    pub resume: Option<String>,
    /// The first message to submit once the first frame is drawn.
    pub initial: Option<Initial>,
}

/// A path label with the user's home directory written `~` and Windows
/// extended-length drive and UNC paths shown in their usual form.
pub(crate) fn tilde(path: &str) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    if let Some(home) = home.as_deref().filter(|home| !home.is_empty())
        && let Some(rest) = path.strip_prefix(home.to_string_lossy().as_ref())
        && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
    {
        return format!("~{rest}");
    }
    if cfg!(windows)
        && let Some(tail) = path.strip_prefix(r"\\?\")
    {
        if let Some(unc) = tail.strip_prefix(r"UNC\") {
            return format!(r"\\{unc}");
        }
        if matches!(
            tail.as_bytes(),
            [b'A'..=b'Z' | b'a'..=b'z', b':', b'\\', ..]
        ) {
            return tail.to_owned();
        }
    }
    path.to_owned()
}

pub mod approvals_panel;
mod atomic;
pub mod commands;
pub mod composer;
pub mod config;
pub mod i18n;
pub mod isolation_panel;
pub mod keys;
pub mod language_panel;
pub mod line_input;
pub mod lines_panel;
pub mod list_panel;
pub mod management_panel;
pub mod models_panel;
pub mod notify;
pub mod oauth_view;
pub mod plugins_panel;
pub mod progress;
pub mod prompts;
pub mod providers_panel;
pub mod queue_panel;
pub mod render;
mod report_panel;
pub mod rows;
pub mod runtime_panel;
pub mod sessions_panel;
pub mod settings_panel;
pub mod shell;
pub mod status;
pub mod tasks_panel;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod theme_panel;
pub mod timeline_panel;
pub mod title;
pub mod transcript;
pub mod tui;
pub mod upgrade_dialog;
