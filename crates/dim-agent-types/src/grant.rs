use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    pub path: PathBuf,
    pub kind: GrantKind,
    pub mode: GrantMode,
}

impl SessionGrant {
    /// Whether this grant permits `target` for `mode`. Paths are canonical;
    /// a file grant matches only its exact path and a directory grant matches
    /// the path and its children by component. A write grant also permits
    /// reads.
    pub fn matches(&self, target: &Path, mode: GrantMode) -> bool {
        if mode == GrantMode::Write && self.mode != GrantMode::Write {
            return false;
        }
        match self.kind {
            GrantKind::File => target == self.path,
            GrantKind::Dir => target.starts_with(&self.path),
        }
    }
}
