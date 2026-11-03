//! Terminal UI: screen, input, panels and rendering.
//!
//! Depends on the view model in `model` and the session port in
//! `model::session`; it never reads wire types, CLI options or the
//! App Server implementation.

use std::path::PathBuf;

/// What the composition root tells the UI about its surroundings.
#[derive(Clone)]
pub struct Env {
    /// Product home for saved settings and input history.
    pub home: PathBuf,
    /// The workspace the session works in, as every request names it.
    pub workspace: String,
    /// Start in the fullscreen interface instead of scrollback.
    pub fullscreen: bool,
}

/// A path with the user's home directory written `~`.
pub(crate) fn tilde(path: &str) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let Some(home) = home else {
        return path.to_owned();
    };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return path.to_owned();
    }
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
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
pub mod render;
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
