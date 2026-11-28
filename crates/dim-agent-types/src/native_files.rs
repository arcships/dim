use crate::ImageSource;
use serde::{Deserialize, Serialize};

pub const WORKTREES: &str = "workspace.files.worktrees";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Worktrees {
    pub cwd: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Complete {
    pub cwd: String,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Deserialize, Serialize)]
pub struct Completions {
    pub candidates: Vec<FileCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCandidate {
    pub insert_text: String,
    pub absolute_path: String,
    pub dir: bool,
    pub image: bool,
}

pub const UPLOAD: &str = "native.uploadSessionFile";
pub const READY: &str = "native.uploadSessionFile.ready";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Header {
    pub session_id: String,
    pub name: String,
    pub byte_length: u64,
    pub image: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedFile {
    pub path: String,
    pub image: Option<ImageSource>,
}
