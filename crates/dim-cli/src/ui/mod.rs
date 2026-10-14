//! Terminal UI: screen, input, panels and rendering.
//!
//! Depends on the view model in `model` and the session port in
//! `model::session`; it never reads wire types, CLI options or the
//! App Server implementation.

use std::path::PathBuf;

/// What the composition root tells the UI about its surroundings.
pub struct Env {
    /// Product home for saved settings and input history.
    pub home: PathBuf,
    /// The workspace the session works in.
    pub workspace: PathBuf,
}

pub mod approvals_panel;
pub mod attachments;
pub mod composer;
pub mod config;
pub mod list_panel;
pub mod models_panel;
pub mod oauth_view;
pub mod prompts;
pub mod providers_panel;
pub mod render;
pub mod sessions_panel;
pub mod terminal;
pub mod theme;
pub mod theme_panel;
pub mod transcript;
pub mod tui;
