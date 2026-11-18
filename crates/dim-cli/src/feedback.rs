//! A private intake draft made only from the CLI's focused transcript projection.

use std::{collections::BTreeMap, fmt};

use serde::Serialize;
use uuid::Uuid;

use crate::model::view::{Earlier, RowKind, ViewRow, ViewSnapshot};

const MAX_BODY_BYTES: usize = 64 * 1024;
const CANDIDATE_ROWS: usize = 128;
const MAX_ROWS: usize = 12;
const MAX_TEXT_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 96;
const MAX_ARGUMENT_BYTES: usize = 384;
const MAX_NOTE_BYTES: usize = 2048;
const MAX_INSPECT_BYTES: usize = 16 * 1024;
const MAX_LINES: usize = 16;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sample {
    version: u8,
    client_version: &'static str,
    anchor: Anchor,
    history_complete: bool,
    older_history_not_loaded: bool,
    window_truncated: bool,
    rows: Vec<Row>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Anchor {
    branch: Branch,
    last_position: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
enum Branch {
    Main,
    Line,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    position: usize,
    #[serde(flatten)]
    content: Content,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Content {
    User {
        text: Excerpt,
    },
    Assistant {
        text: Excerpt,
        streaming: bool,
    },
    Tool {
        name: Excerpt,
        arguments: BTreeMap<&'static str, Excerpt>,
        #[serde(rename = "argumentsStatus")]
        arguments_status: Status,
        result: Excerpt,
        pending: bool,
    },
}

#[derive(Clone, Serialize)]
struct Excerpt {
    text: Option<String>,
    status: Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Status {
    Included,
    Truncated,
    Missing,
    Omitted,
    Redacted,
    TooLarge,
    NonText,
    Partial,
}

#[derive(Clone)]
pub(crate) struct Submission {
    pub id: Uuid,
    /// Render these exact bytes for confirmation and send the same body unchanged.
    pub body: String,
}

impl fmt::Debug for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sample")
            .field("row_count", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Submission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Submission")
            .field("id", &self.id)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    schema_version: u8,
    case_id: String,
    allow_eval: bool,
    note: String,
    sample: PreparedSample<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedSample<'a> {
    #[serde(flatten)]
    evidence: &'a Sample,
    note_status: Status,
}

impl Sample {
    /// Freeze the newest eligible rows from the focused line, not the side panel
    /// or the backing Timeline. The last eligible row is the local anchor.
    pub(crate) fn capture(snapshot: &ViewSnapshot) -> Option<Self> {
        let branch = match snapshot.focus.as_str() {
            "" => return None,
            "main" => Branch::Main,
            _ => Branch::Line,
        };
        let mut window_truncated = snapshot.rows.len() > CANDIDATE_ROWS;
        let mut rows = Vec::with_capacity(MAX_ROWS);
        for (position, row) in snapshot.rows.iter().enumerate().rev() {
            let outside_window = snapshot.rows.len() - position > CANDIDATE_ROWS;
            if outside_window {
                break;
            }
            let Some(content) = Content::capture(row) else {
                continue;
            };
            if rows.len() == MAX_ROWS {
                window_truncated = true;
                break;
            }
            rows.push(Row { position, content });
        }
        let last_position = rows.first()?.position;
        rows.reverse();
        Some(Self {
            version: 1,
            client_version: env!("CARGO_PKG_VERSION"),
            anchor: Anchor {
                branch,
                last_position,
            },
            // A loaded projection is not necessarily the whole history, and
            // a bounded sample must not claim otherwise.
            history_complete: snapshot.history_complete
                && snapshot.earlier == Earlier::AtStart
                && !window_truncated
                && rows.len() == snapshot.rows.len(),
            older_history_not_loaded: snapshot.earlier != Earlier::AtStart,
            window_truncated,
            rows,
        })
    }

    /// Construct one immutable preview/wire body. This method never transmits it.
    pub(crate) fn prepare(&self, note: &str, allow_eval: bool) -> Result<Submission, &'static str> {
        let note = Excerpt::clean(Some(note), MAX_NOTE_BYTES);
        let id = Uuid::new_v4();
        let body = serde_json::to_string(&Payload {
            schema_version: 1,
            case_id: id.to_string(),
            allow_eval,
            note: note.text.unwrap_or_default(),
            sample: PreparedSample {
                evidence: self,
                note_status: note.status,
            },
        })
        .map_err(|_| "Could not encode the private intake sample.")?;
        if body.len() > MAX_BODY_BYTES {
            return Err("The private intake sample exceeds 64 KiB.");
        }
        Ok(Submission { id, body })
    }
}

impl Content {
    fn capture(row: &ViewRow) -> Option<Self> {
        if row.line.is_some() || (row.kind == RowKind::UserMessage && row.text.starts_with("!!")) {
            return None;
        }
        match row.kind {
            RowKind::UserMessage => Some(Self::User {
                text: Excerpt::clean(Some(&row.text), MAX_TEXT_BYTES),
            }),
            RowKind::AssistantMessage | RowKind::StreamingAssistant => Some(Self::Assistant {
                text: Excerpt::clean(Some(&row.text), MAX_TEXT_BYTES),
                streaming: row.streaming || row.kind == RowKind::StreamingAssistant,
            }),
            RowKind::ToolCall => {
                let raw_name = row.tool_name.as_deref();
                let name = if raw_name.is_some_and(|name| {
                    !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-:".contains(&b))
                }) {
                    Excerpt::unavailable(Status::Omitted)
                } else {
                    Excerpt::clean(raw_name, MAX_NAME_BYTES)
                };
                let input = row.tool_input.as_ref();
                let result = row.result_text.as_deref();
                let keys: Option<&[&str]> = match raw_name {
                    Some("exec" | "execute" | "bash") => Some(&["command"]),
                    Some("read" | "edit" | "write") => Some(&["path", "file_path"]),
                    Some("grep") => Some(&["pattern", "query"]),
                    Some("glob") => Some(&["pattern"]),
                    _ => None,
                };
                let mut arguments = BTreeMap::new();
                let arguments_status = match (input, keys) {
                    (None, _) => Status::Missing,
                    (Some(serde_json::Value::Object(input)), Some(keys)) => {
                        for &key in keys {
                            if let Some(value) = input.get(key) {
                                let excerpt = match value.as_str() {
                                    Some(text) => Excerpt::clean(Some(text), MAX_ARGUMENT_BYTES),
                                    None => Excerpt::unavailable(Status::NonText),
                                };
                                arguments.insert(key, excerpt);
                            }
                        }
                        if arguments.len() == input.len() {
                            Status::Included
                        } else if arguments.is_empty() {
                            Status::Omitted
                        } else {
                            Status::Partial
                        }
                    }
                    _ => Status::Omitted,
                };
                let result = if result.is_none() || result == Some("") {
                    Excerpt::unavailable(Status::Missing)
                } else if arguments.is_empty()
                    || arguments
                        .values()
                        .any(|field| field.status != Status::Included)
                {
                    Excerpt::unavailable(Status::Omitted)
                } else {
                    Excerpt::clean(result, MAX_TEXT_BYTES)
                };
                Some(Self::Tool {
                    name,
                    arguments,
                    arguments_status,
                    result,
                    pending: row.pending,
                })
            }
            _ => None,
        }
    }
}

impl Excerpt {
    fn unavailable(status: Status) -> Self {
        Self { text: None, status }
    }

    fn clean(text: Option<&str>, limit: usize) -> Self {
        let Some(text) = text else {
            return Self::unavailable(Status::Missing);
        };
        if text.len() > MAX_INSPECT_BYTES {
            return Self::unavailable(Status::TooLarge);
        }
        if text.chars().any(|ch| {
            ch.is_control()
                && !matches!(
                    ch,
                    '\r' | '\n' | '\t' | '\u{1b}' | '\u{7}' | '\u{9b}' | '\u{9c}' | '\u{9d}'
                )
        }) {
            return Self::unavailable(Status::NonText);
        }
        let text = crate::sanitize::safe(&text.replace('\r', "\n"));
        if text.trim().is_empty() {
            return Self::unavailable(Status::Missing);
        }
        if sensitive(&text) {
            return Self::unavailable(Status::Redacted);
        }
        let end = text.floor_char_boundary(text.len().min(limit));
        let end = text[..end]
            .match_indices('\n')
            .nth(MAX_LINES - 1)
            .map_or(end, |(index, _)| index);
        Self {
            status: if end == text.len() {
                Status::Included
            } else {
                Status::Truncated
            },
            text: Some(text[..end].to_owned()),
        }
    }
}

// Withhold the entire field rather than guess where a credential ends. This
// also covers continuation lines and detects labels beyond the excerpt limit.
fn sensitive(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("-----begin")
        || lower.contains("-----end")
        || lower.contains("private key")
        || lower.contains("data:")
        || lower.contains(".env")
        || lower.contains("id_rsa")
        || lower.contains("id_ed25519")
        || lower.contains("<dim-")
        || lower.contains("----- begin")
        || lower.split("://").skip(1).any(|url| {
            let url = url.split_whitespace().next().unwrap_or("");
            url.split(['/', '?', '#'])
                .next()
                .is_some_and(|host| host.contains('@'))
                || url.contains(['?', '%'])
        })
        || lower
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|word| {
                matches!(
                    word,
                    "authorization"
                        | "bearer"
                        | "basic"
                        | "cookie"
                        | "cookies"
                        | "credential"
                        | "credentials"
                        | "key"
                        | "apikey"
                        | "privatekey"
                        | "password"
                        | "passwd"
                        | "pwd"
                        | "proxy"
                        | "secret"
                        | "secrets"
                        | "sessionid"
                        | "token"
                        | "accesstoken"
                        | "refreshtoken"
                        | "clientsecret"
                        | "signature"
                        | "xoxb"
                        | "xoxp"
                        | "xoxa"
                        | "xoxr"
                        | "xoxs"
                        | "ghp"
                        | "gho"
                        | "ghu"
                        | "ghs"
                        | "ghr"
                        | "sk"
                        | "rk"
                        | "glpat"
                ) || word.starts_with("akia") && word.len() >= 16
                    || word.starts_with("asia") && word.len() >= 16
                    || word.starts_with("eyj") && word.len() >= 12
            })
        || text
            .split(|ch: char| !ch.is_ascii_alphanumeric() && !"_+/-=".contains(ch))
            .any(|word| word.len() >= 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn row(kind: RowKind, text: &str) -> Arc<ViewRow> {
        Arc::new(ViewRow {
            row_id: "private-row-id".into(),
            provisional_id: None,
            kind,
            display: Some("private-display".into()),
            text: text.into(),
            streaming: false,
            elapsed_ms: None,
            tone: None,
            call_id: None,
            tool_lookup_done: false,
            tool_name: None,
            tool_input: None,
            batch: None,
            parent_call_id: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            parent_row: None,
            compaction: None,
            failure: None,
            reportable: false,
        })
    }

    fn snapshot(rows: Vec<Arc<ViewRow>>) -> ViewSnapshot {
        ViewSnapshot {
            focus: "main".into(),
            history_complete: true,
            rows,
            ..ViewSnapshot::default()
        }
    }

    fn wire(sample: &Sample) -> Value {
        serde_json::from_str(&sample.prepare("Repro steps", false).unwrap().body).unwrap()
    }

    #[test]
    fn wire_contract_and_frozen_preview() {
        let mut view = snapshot(vec![row(RowKind::UserMessage, "Question")]);
        let sample = Sample::capture(&view).unwrap();
        Arc::make_mut(&mut view.rows[0]).text = "later edit".into();
        let submission = sample.prepare("Repro steps", true).unwrap();
        let value: Value = serde_json::from_str(&submission.body).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["caseId"], submission.id.to_string());
        assert_eq!(value["allowEval"], true);
        assert_eq!(value["note"], "Repro steps");
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert_eq!(value["sample"]["noteStatus"], "included");
        assert_eq!(
            value["sample"]["anchor"],
            json!({"branch": "main", "lastPosition": 0})
        );
        assert_eq!(
            value["sample"]["rows"][0],
            json!({
                "position": 0, "kind": "user", "text": {"text": "Question", "status": "included"}
            })
        );
        assert!(!submission.body.contains("private-row-id"));
        assert!(!submission.body.contains("private-display"));
        assert!(!format!("{sample:?} {submission:?}").contains("Question"));
        assert!(submission.body.len() <= MAX_BODY_BYTES);
    }

    #[test]
    fn excludes_non_transcript_and_other_line_data() {
        let mut other_line = row(RowKind::UserMessage, "other-line-content");
        Arc::make_mut(&mut other_line).line = Some("private-line-id".into());
        let view = snapshot(vec![
            row(RowKind::Thinking, "hidden-thought"),
            row(RowKind::HookContext, "hook-content"),
            row(RowKind::MemberResult, "member-content"),
            row(RowKind::Compaction, "compaction-content"),
            row(RowKind::Shell, "local-shell"),
            row(RowKind::UserMessage, "!!local-only"),
            other_line,
            row(RowKind::AssistantMessage, "Visible answer"),
        ]);
        let value = wire(&Sample::capture(&view).unwrap());
        assert_eq!(value["sample"]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(value["sample"]["rows"][0]["position"], 7);
        assert_eq!(value["sample"]["historyComplete"], false);
        for excluded in [
            "hidden-thought",
            "hook-content",
            "member-content",
            "compaction-content",
            "local-shell",
            "local-only",
            "other-line-content",
        ] {
            assert!(!value.to_string().contains(excluded));
        }
        let mut empty_focus = view;
        empty_focus.focus.clear();
        assert!(Sample::capture(&empty_focus).is_none());
    }

    #[test]
    fn never_searches_beyond_candidate_window() {
        let mut rows = vec![row(RowKind::UserMessage, "Too old")];
        rows.extend((0..CANDIDATE_ROWS).map(|_| row(RowKind::Thinking, "excluded")));
        assert!(Sample::capture(&snapshot(rows)).is_none());
    }

    #[test]
    fn bounds_recent_rows_and_reports_incomplete_history() {
        let mut view = snapshot(
            (0..MAX_ROWS + 1)
                .map(|_| row(RowKind::UserMessage, "Hello"))
                .collect(),
        );
        let value = wire(&Sample::capture(&view).unwrap());
        assert_eq!(value["sample"]["rows"].as_array().unwrap().len(), MAX_ROWS);
        assert_eq!(value["sample"]["rows"][0]["position"], 1);
        assert_eq!(value["sample"]["windowTruncated"], true);
        assert_eq!(value["sample"]["historyComplete"], false);
        view.rows.truncate(1);
        for earlier in [Earlier::Unread, Earlier::Reading] {
            view.earlier = earlier;
            let value = wire(&Sample::capture(&view).unwrap());
            assert_eq!(value["sample"]["historyComplete"], false);
            assert_eq!(value["sample"]["olderHistoryNotLoaded"], true);
        }
        view.earlier = Earlier::AtStart;
        view.history_complete = false;
        assert!(!Sample::capture(&view).unwrap().history_complete);
    }

    #[test]
    fn tool_projection_is_allowlisted_not_serialized_wholesale() {
        let mut tool = row(RowKind::ToolCall, "private-rendered-tool");
        let fields = Arc::make_mut(&mut tool);
        fields.tool_name = Some("exec".into());
        fields.tool_input =
            Some(json!({"command": "printf hello", "env": {"password": "private-input"}}));
        fields.result_text = Some("hello".into());
        fields.diff = Some("private-diff".into());
        let value = wire(&Sample::capture(&snapshot(vec![tool.clone()])).unwrap());
        let content = &value["sample"]["rows"][0];
        assert_eq!(content["argumentsStatus"], "partial");
        assert_eq!(content["arguments"]["command"]["text"], "printf hello");
        assert_eq!(content["result"]["text"], "hello");
        assert!(!value.to_string().contains("private-"));
        Arc::make_mut(&mut tool).tool_name = Some("unknown".into());
        let value = wire(&Sample::capture(&snapshot(vec![tool])).unwrap());
        assert_eq!(value["sample"]["rows"][0]["argumentsStatus"], "omitted");
        assert_eq!(value["sample"]["rows"][0]["result"]["status"], "omitted");
    }

    #[test]
    fn withholds_common_credentials_in_every_excerpt() {
        for credential in [
            "Authorization: Bearer example",
            "api_key=example",
            "password: example",
            "Cookie: value",
            "sk-example",
            "ghp_example",
            "AKIA0123456789ABCDEF",
            "eyJhbGciOiJIUzI1NiJ9.example.signature",
            "https://user:pass@example.test/path",
            "https://example.test/path?code=example",
            "-----BEGIN PRIVATE KEY-----",
            "/abcdefghijklmnop0123456789ABCDEFG",
            "abcdefghijklmnop//0123456789ABCDEFG",
        ] {
            assert_eq!(
                Excerpt::clean(Some(credential), MAX_TEXT_BYTES).status,
                Status::Redacted
            );
            let sample =
                Sample::capture(&snapshot(vec![row(RowKind::UserMessage, credential)])).unwrap();
            let submission = sample.prepare(credential, false).unwrap();
            let value: Value = serde_json::from_str(&submission.body).unwrap();
            assert_eq!(value["note"], "");
            assert_eq!(value["sample"]["noteStatus"], "redacted");
            assert_eq!(value["sample"]["rows"][0]["text"]["status"], "redacted");
            assert!(!submission.body.contains(credential));
        }
        let late_secret = format!("{} password=example", "ordinary words ".repeat(100));
        assert_eq!(
            Excerpt::clean(Some(&late_secret), 10).status,
            Status::Redacted
        );
    }

    #[test]
    fn excerpts_are_bounded_sanitized_and_truthful() {
        assert_eq!(Excerpt::clean(None, 10).status, Status::Missing);
        assert_eq!(Excerpt::clean(Some(" \n"), 10).status, Status::Missing);
        assert_eq!(Excerpt::clean(Some("a\0b"), 10).status, Status::NonText);
        assert_eq!(
            Excerpt::clean(Some(&"x".repeat(MAX_INSPECT_BYTES + 1)), 10).status,
            Status::TooLarge
        );
        let text = Excerpt::clean(Some("你好世界"), 7);
        assert_eq!(text.text.as_deref(), Some("你好"));
        assert_eq!(text.status, Status::Truncated);
        let lines = Excerpt::clean(Some(&"line\n".repeat(MAX_LINES + 1)), MAX_TEXT_BYTES);
        assert_eq!(lines.status, Status::Truncated);
        assert_eq!(lines.text.unwrap().lines().count(), MAX_LINES);
        let ansi = Excerpt::clean(Some("\u{1b}[31mHello\u{1b}[0m"), 100);
        assert_eq!(ansi.text.as_deref(), Some("Hello"));
        assert_eq!(ansi.status, Status::Included);
    }
}
