//! Pure static repository checks with no dependency on compiling business
//! runtime crates (ADR 026 D2, D4).

pub mod boundaries;
pub mod comment_links;
pub mod i18n;
pub mod literal_main;
pub mod modules;
pub mod npm;
pub mod plugin_docs;
pub mod types;

use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Workspace root resolved from this crate's manifest directory (`xtask/check`).
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Runs every pure static repository check against `root`.
pub fn check_all(root: &Path) -> Result<()> {
    boundaries::check(root)?;
    types::check(root)?;
    npm::check(root)?;
    comment_links::check(root)?;
    i18n::check(root)?;
    literal_main::check(root)?;
    Ok(())
}
