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
static CREDENTIAL_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i)(?:^|[^a-z0-9_-]|\\[rn])"#,
        r#"(?:[a-z0-9]+[_-])*"#,
        r#"(?:cookie|authorization|api[_-]?key|private[_-]?key|signing[_-]?key"#,
        r#"|password|passwd|credential|secret|token)"#,
        r#"(?:[_-][a-z0-9]+)*(?:\\?["'`])?[ \t]*[:=]"#,
    ))
    .expect("static credential field pattern")
});
static COOKIE_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[^a-z0-9_-]|\\[rn])(?:set-)?cookie(?:\\?["'`])?[ \t]*[:=]"#)
        .expect("static cookie field pattern")
});
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@").expect("static URL userinfo pattern")
});
static BEARER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^a-z0-9_-])bearer[ \t]+[a-z0-9._~+/-]{20,}={0,2}")
        .expect("static bearer token pattern")
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
    let mut credential_continuation = None;
    let mut value_pending = false;
    for line in normalized.lines() {
        let lower = line.to_ascii_lowercase();
        let field = CREDENTIAL_FIELD.find(&lower);
        let indentation = line
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .count();
        let continuation = credential_continuation
            .is_some_and(|base| !line.trim().is_empty() && indentation > base);
        let quoted_continuation = value_pending && line.trim_start().starts_with(['"', '\'', '`']);
        let field_value_is_pending = field.is_some_and(|field| {
            let value = line[field.end()..].trim();
            value.is_empty()
                || matches!(value, "'" | "\"" | "`")
                || value.eq_ignore_ascii_case("bearer")
        });
        let folded_header = COOKIE_FIELD.is_match(&lower);
        if lower.contains("-----begin") && lower.contains("private key") {
            lines.push(WITHHELD);
            break;
        }
        let withheld = field.is_some()
            || continuation
            || quoted_continuation
            || URL_USERINFO.is_match(line)
            || BEARER_TOKEN.is_match(line)
            || lower.contains("-----begin")
            || lower.contains("akia")
            || lower.contains("eyj");
        if withheld {
            lines.push(WITHHELD);
        } else {
            lines.push(line);
        }

        credential_continuation = if field_value_is_pending || folded_header {
            Some(indentation)
        } else if continuation {
            credential_continuation
        } else {
            None
        };
        value_pending = field_value_is_pending;
    }
    lines.join("\n")
}
