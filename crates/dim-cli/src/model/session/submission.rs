use crate::model::session::{DraftPart, SubmitMode};
use crate::{Result, client::Client};
use dim_agent_types::{ContentBlock, UserInput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionKey {
    pub session_id: String,
    pub branch_id: String,
    pub submission_id: String,
}

impl SubmissionKey {
    pub fn new(session: &str, line: &str) -> Self {
        Self {
            session_id: session.into(),
            branch_id: line.into(),
            submission_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub async fn accepted(&self, client: &mut Client) -> Result<bool> {
        #[derive(Deserialize)]
        #[serde(tag = "status", rename_all = "lowercase")]
        enum Lookup {
            Accepted,
            Unknown,
        }
        let reply: Lookup = client
            .typed("session.input_lookup", serde_json::to_value(self)?)
            .await?;
        Ok(matches!(reply, Lookup::Accepted))
    }

    pub fn params(&self, user: &UserInput, mode: &str) -> Result<serde_json::Value> {
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Body<'a> {
            Text { text: &'a str },
            User { user: &'a UserInput },
        }
        #[derive(Serialize)]
        struct Submit<'a> {
            #[serde(flatten)]
            key: &'a SubmissionKey,
            mode: &'a str,
            #[serde(flatten)]
            body: Body<'a>,
        }
        let body = match user.content.as_slice() {
            [ContentBlock::Text { text }] => Body::Text { text },
            _ => Body::User { user },
        };
        Ok(serde_json::to_value(Submit {
            key: self,
            mode,
            body,
        })?)
    }
}

pub(crate) fn refused(error: &(dyn std::error::Error + 'static)) -> bool {
    error.is::<dim_client::RequestTooLarge>()
        || error
            .downcast_ref::<dim_client::RpcError>()
            .is_some_and(|e| {
                matches!(e.code, -32601 | -32602)
                    || matches!(
                        e.data.as_ref().and_then(|d| d["appError"].as_str()),
                        Some(
                            "invalid_params"
                                | "session_invalid_input"
                                | "session_closed"
                                | "session_build_failed"
                                | "service_reloading"
                                | "unknown_method"
                                | "input_blocked"
                        )
                    )
            })
}

#[derive(Debug, Clone)]
pub struct PreparedDraft {
    pub key: SubmissionKey,
    pub mode: SubmitMode,
    pub parts: Vec<DraftPart>,
}

pub(crate) fn prepared_user(parts: &[DraftPart]) -> UserInput {
    let mut content = Vec::new();
    let mut text = String::new();
    for part in parts {
        match part {
            DraftPart::Text(value) | DraftPart::Block(ContentBlock::Text { text: value }) => {
                text.push_str(value)
            }
            DraftPart::Block(block) => {
                if !text.is_empty() {
                    content.push(ContentBlock::Text {
                        text: std::mem::take(&mut text),
                    });
                }
                content.push(block.clone());
            }
            DraftPart::Paste(_) | DraftPart::File { .. } => unreachable!("prepared draft"),
        }
    }
    if !text.is_empty() {
        content.push(ContentBlock::Text { text });
    }
    UserInput { content }
}

#[derive(Debug)]
pub enum Delivery {
    Accepted,
    Refused(String),
    Unconfirmed(String),
}
