use crate::{
    evidence::ConversationExcerpt,
    redaction::{Facts, Replacements, mask_credentials},
};
use dim_agent_runtime::QueryEvent;
use dim_agent_types::{
    ContentBlock, ContentMetadata, EventKind, FinishReason, MessagePart, ModelOutputOutcome,
    markup::runtime_context,
};
use regex::Regex;
use std::{collections::HashMap, path::Path, sync::LazyLock};

const MAX_SOURCE: usize = 64 * 1024;
const EXCERPT_LIMIT: usize = 2048;
const TOTAL_LIMIT: usize = 8 * 1024;
const WITHHELD: &str = "[excerpt withheld]";
static COOKIE_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[^a-z0-9_-]|\\[rn])(?:set-)?cookie(?:\\?["'`])?[ \t]*[:=]"#)
        .expect("static cookie field pattern")
});

#[derive(Default)]
pub(super) struct Conversation {
    redacted: Vec<ConversationExcerpt>,
    unredacted: Vec<ConversationExcerpt>,
}

impl Conversation {
    pub(super) fn excerpts(&self, redact: bool) -> &[ConversationExcerpt] {
        if redact {
            &self.redacted
        } else {
            &self.unredacted
        }
    }
}

fn append(text: &mut Option<String>, value: &str) {
    if let Some(current) = text {
        if value.len() > MAX_SOURCE - current.len() {
            *text = None;
        } else {
            current.push_str(value);
        }
    }
}

fn number(facts: &mut Facts, value: &str, kind: &str) -> Option<()> {
    if value.len() > 1024 {
        return None;
    }
    if !facts.identifiers.iter().any(|(id, _)| id == value) {
        if facts.identifiers.len() >= 64 {
            return None;
        }
        let n = 1 + facts
            .identifiers
            .iter()
            .filter(|(_, label)| label.starts_with(kind))
            .count();
        facts
            .identifiers
            .push((value.to_owned(), format!("{kind}-{n}")));
    }
    Some(())
}

pub(super) fn select(events: &[QueryEvent], session: &str, home: &Path) -> Conversation {
    let mut facts = Facts {
        homes: home.to_str().map(String::from).into_iter().collect(),
        session: session.into(),
        ..Default::default()
    };
    let mut pending = HashMap::new();
    let mut output = None;
    let mut excerpts = Vec::new();
    let mut ids_complete = true;
    for row in events {
        let event = &row.event;
        if event.parent_call_id.is_some() {
            continue;
        }
        let selected = match &event.kind {
            EventKind::InputMessage(input) => {
                if let Some(user) = &input.user {
                    let mut text = Some(String::new());
                    for (index, block) in user.content.iter().enumerate() {
                        if let ContentBlock::Text { text: value } = block
                            && !runtime_context(index, value)
                        {
                            if text.as_ref().is_some_and(|s| !s.is_empty()) {
                                append(&mut text, "\n");
                            }
                            append(&mut text, value);
                        }
                    }
                    if let Some(text) = text {
                        pending.insert(&event.id, text);
                    }
                }
                None
            }
            EventKind::InputConsumed(data) => {
                pending.remove(&data.input_id).map(|text| ("user", text))
            }
            EventKind::ToolStart(data) => {
                ids_complete &= number(&mut facts, &data.call_id, "call").is_some();
                None
            }
            EventKind::ToolCompleted(data) => {
                ids_complete &= number(&mut facts, &data.call_id, "call").is_some();
                None
            }
            EventKind::ModelStart(data) => {
                output = Some((&data.request_id, Some(String::new())));
                None
            }
            EventKind::Text(data) => {
                if let Some((request, text)) = &mut output
                    && **request == data.request_id
                {
                    append(text, &data.delta);
                }
                None
            }
            EventKind::ModelContent(data) => {
                let mut text = Some(String::new());
                for part in &data.content {
                    let metadata = match part {
                        MessagePart::Text {
                            text: value,
                            provider_metadata,
                            ..
                        } => {
                            append(&mut text, value);
                            provider_metadata
                        }
                        MessagePart::ToolUse {
                            id,
                            name,
                            provider_metadata,
                            ..
                        } => {
                            ids_complete &= number(&mut facts, id, "call").is_some();
                            ids_complete &= number(&mut facts, name, "tool").is_some();
                            provider_metadata
                        }
                        MessagePart::Thinking {
                            provider_metadata, ..
                        } => provider_metadata,
                        _ => continue,
                    };
                    if let Some(
                        ContentMetadata::OpenaiResponses { item_id, .. }
                        | ContentMetadata::XaiResponses { item_id, .. },
                    ) = metadata
                    {
                        ids_complete &= number(&mut facts, item_id, "item").is_some();
                    }
                }
                output = Some((&data.request_id, text));
                None
            }
            EventKind::ModelOutputEnd(data) => output.take().and_then(|(request, text)| {
                (*request == data.request_id
                    && matches!(
                        data.outcome,
                        ModelOutputOutcome::Completed {
                            finish_reason: FinishReason::EndTurn | FinishReason::StopSequence
                        }
                    ))
                .then_some(text)
                .flatten()
                .map(|text| ("assistant", text))
            }),
            _ => None,
        };
        if let Some((role, text)) = selected
            && !text.trim().is_empty()
        {
            excerpts.push((role, event.sequence, text));
        }
    }
    let selected: Vec<_> = excerpts.into_iter().rev().take(6).collect();
    let kept = selected
        .iter()
        .map(|(_, _, text)| text.clone())
        .collect::<Vec<_>>();
    let rules = ids_complete
        .then(|| Replacements::new(&facts, &kept).ok())
        .flatten();
    let mut result = Conversation::default();
    let mut remaining = TOTAL_LIMIT;
    for (role, sequence, text) in selected {
        if remaining == 0 {
            break;
        }
        let mut raw = guarded(&text);
        let mut checked = rules
            .as_ref()
            .map(|rules| {
                let value = rules.text(&raw);
                if rules.has_residue(&value) {
                    WITHHELD.into()
                } else {
                    value
                }
            })
            .unwrap_or_else(|| WITHHELD.into());
        for text in [&mut raw, &mut checked] {
            let cap = text.floor_char_boundary(remaining.min(EXCERPT_LIMIT));
            text.truncate(cap);
        }
        remaining -= raw.len().max(checked.len());
        result.redacted.push(ConversationExcerpt {
            role,
            sequence,
            text: checked,
        });
        result.unredacted.push(ConversationExcerpt {
            role,
            sequence,
            text: raw,
        });
    }
    result.redacted.reverse();
    result.unredacted.reverse();
    result
}

fn guarded(value: &str) -> String {
    let masked = mask_credentials(value);
    let normalized = masked.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    let mut cookie_continuation = false;
    let mut cookie_value_pending = false;
    for line in normalized.lines() {
        let lower = line.to_ascii_lowercase();
        let field = COOKIE_FIELD.find(&lower);
        cookie_continuation = field.is_some()
            || (cookie_continuation && line.starts_with([' ', '\t']))
            || (cookie_value_pending && line.trim_start().starts_with(['"', '\'']));
        cookie_value_pending = field.is_some_and(|m| line[m.end()..].trim().is_empty())
            || (cookie_value_pending && line.trim().is_empty());
        if lower.contains("-----begin") && lower.contains("private key") {
            lines.push(WITHHELD);
            break;
        }
        if [
            "authorization",
            "bearer ",
            "api_key",
            "api-key",
            "apikey",
            "password",
            "passwd",
            "credential",
            "secret",
            "sk-",
            "ghp_",
            "github_pat_",
            "xoxb-",
            "xoxp-",
            "-----begin",
            "akia",
            "eyj",
        ]
        .iter()
        .any(|p| lower.contains(p))
            || cookie_continuation
            || lower.match_indices("token").any(|(at, _)| {
                lower[at + "token".len()..]
                    .trim_start_matches(['\'', '"', ' ', '\t'])
                    .starts_with(['=', ':'])
            })
        {
            lines.push(WITHHELD);
        } else {
            lines.push(line);
        }
    }
    lines.join("\n")
}
