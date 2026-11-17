use crate::{InputMode, UserInput};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingInput {
    pub input_id: String,
    #[serde(with = "crate::safe_integer")]
    pub sequence: u64,
    pub mode: InputMode,
    pub user: Option<UserInput>,
    pub applied: bool,
    /// Present when the read requested block presence; no raw content is returned.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub has_raw_blocks: Option<bool>,
}

/// One piece of work a submit check did not do, attributed to the check that
/// raised it (Hooks A.6.2). A caller shows it; the model never reads it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SubmitWarning {
    /// The name the raising check was registered under.
    pub check: String,
    /// What it did not do, as one sentence.
    pub message: String,
}
