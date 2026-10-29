//! The preview rule SessionStore's catalog projection shares (#556): the text
//! of a Session's first user input, as `session.summaries` lists it.
use dim_agent_types::{ContentBlock, UserInput};

/// Characters the preview keeps before it ends in `…`.
const PREVIEW_CHARS: usize = 120;

/// The preview a user input gives `session.summaries`: its text blocks joined
/// with newlines, cut to 120 characters, `None` when the text is empty (an
/// image-only message, for example).
pub(crate) fn preview(user: &UserInput) -> Option<String> {
    let text = user
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| truncate(&text, PREVIEW_CHARS))
}

fn truncate(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}
