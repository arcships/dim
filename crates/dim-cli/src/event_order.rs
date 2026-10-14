//! Member order of product events as the reference TypeScript client prints
//! them: it parses each event with a zod schema, which re-emits members in
//! schema order (declared members first, catch-all members after, in the
//! order received).

use crate::json_ord::J;

const STEP: &[&str] = &["turn_id", "request_id"];

fn with_step(extra: &[&'static str]) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = STEP.to_vec();
    keys.extend_from_slice(extra);
    keys
}

/// Reorder `members` by `keys`; members not listed keep their relative order
/// after the listed ones.
fn reorder(members: &[(String, J)], keys: &[&str], child: &dyn Fn(&str, &J) -> J) -> J {
    let mut out: Vec<(String, J)> = Vec::with_capacity(members.len());
    for key in keys {
        if let Some((k, v)) = members.iter().find(|(k, _)| k == key) {
            out.push((k.clone(), child(k, v)));
        }
    }
    for (k, v) in members {
        if !keys.contains(&k.as_str()) {
            out.push((k.clone(), v.clone()));
        }
    }
    J::Obj(out)
}

fn plain(_: &str, v: &J) -> J {
    v.clone()
}

fn object(value: &J, keys: &[&str], child: &dyn Fn(&str, &J) -> J) -> J {
    match value {
        J::Obj(members) => reorder(members, keys, child),
        other => other.clone(),
    }
}

fn each(value: &J, f: &dyn Fn(&J) -> J) -> J {
    match value {
        J::Arr(items) => J::Arr(items.iter().map(f).collect()),
        other => other.clone(),
    }
}

fn content_block(value: &J) -> J {
    object(value, &["type", "text", "source"], &|k, v| {
        if k == "source" {
            object(v, &["type", "mediaType", "data", "url"], &plain)
        } else {
            v.clone()
        }
    })
}

fn tool_call(value: &J) -> J {
    object(value, &["id", "name", "input"], &plain)
}

fn tool_result(value: &J) -> J {
    object(
        value,
        &[
            "content",
            "isError",
            "structuredContent",
            "blobRefs",
            "backgroundTask",
            "contextMessages",
            "prompts",
        ],
        &|k, v| {
            if k == "content" {
                each(v, &content_block)
            } else {
                v.clone()
            }
        },
    )
}

fn branch_source(value: &J) -> J {
    object(
        value,
        &["kind", "turn_id", "request_id", "call_id", "operation_id"],
        &plain,
    )
}

fn data(kind: &str, name: &str, value: &J) -> J {
    let keys: Vec<&str> = match (kind, name) {
        ("exec", "started") => with_step(&["call_id", "task_id", "description", "output_path"]),
        ("exec", "ended") => vec!["task_id", "status", "result"],
        ("approval", "request") => vec!["approval_id", "source", "call", "reason"],
        ("approval", "respond") => vec!["approval_id", "decision", "reason"],
        ("approval", "cancelled") => vec!["approval_id", "reason"],
        ("approval", "review_started") => with_step(&["approval_id", "call_id", "action_hash"]),
        ("input", "message") => vec![
            "user",
            "blocks",
            "mode",
            "submission_id",
            "user_message_id",
            "attachments",
            "work_origin",
        ],
        ("input", "applied") => vec!["input_id", "expected_turn_id", "mode"],
        ("input", "control") => vec!["inputId", "action"],
        ("input", "control-result") => vec!["inputId", "action", "controlId", "outcome"],
        ("input", "consumed") => vec!["input_id"],
        ("turn", "start") => vec!["turn_id", "input_id"],
        ("turn", "end") => vec!["turn_id", "status", "reason", "error"],
        ("stop", "interrupt") => vec!["reason", "scope"],
        ("step", "model-start") => with_step(&[]),
        ("model", "call") => with_step(&["provider_id", "model_id"]),
        ("step", "text") | ("step", "thinking") => with_step(&["delta"]),
        ("step", "tool-use-start") => with_step(&["call_id", "name"]),
        ("step", "tool-use-args-delta") => with_step(&["call_id", "name", "delta"]),
        ("step", "tool-use") => with_step(&["call"]),
        ("step", "tool-call") => with_step(&["calls"]),
        ("step", "tool-start") => with_step(&["call_event_id", "call_id"]),
        ("step", "tool-completed") => with_step(&["call_event_id", "call_id", "result"]),
        ("step", "tool-result") => with_step(&["call_event_id", "results"]),
        ("step", "model-output-end") => with_step(&[
            "status",
            "finish_reason",
            "error",
            "budget",
            "presentation",
            "reason",
        ]),
        ("model", "usage") => with_step(&["usage"]),
        ("context", "configured") => vec!["provider_id", "model_id"],
        ("context", "prepare") => with_step(&["through", "manual"]),
        ("collaboration", "request") => with_step(&["call_id", "prompt", "display", "target"]),
        _ => Vec::new(),
    };
    let key_refs: Vec<&str> = keys.iter().copied().collect();
    object(value, &key_refs, &|k, v| match (kind, name, k) {
        ("exec", "ended", "result") => tool_result(v),
        ("approval", "request", "source") => branch_source(v),
        ("approval", "request", "call") => object(v, &["name", "input"], &plain),
        ("input", "message", "user") => object(v, &["content"], &|_, c| {
            each(c, &|b| object(b, &["type", "text"], &plain))
        }),
        ("input", "message", "blocks") => each(v, &|b| {
            object(b, &["type", "text", "key", "retire"], &plain)
        }),
        ("step", "tool-use", "call") => tool_call(v),
        ("step", "tool-call", "calls") => each(v, &tool_call),
        ("step", "tool-completed", "result") => tool_result(v),
        ("step", "tool-result", "results") => each(v, &|r| {
            object(r, &["call_id", "result"], &|k, v| {
                if k == "result" {
                    tool_result(v)
                } else {
                    v.clone()
                }
            })
        }),
        ("model", "usage", "usage") => object(
            v,
            &[
                "promptTokens",
                "completionTokens",
                "totalTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
                "cost",
            ],
            &plain,
        ),
        ("turn", "end", _) | ("step", "model-output-end", _) | _ => v.clone(),
    })
}

/// One event in the reference client's member order.
pub fn canonical(event: &J) -> J {
    let kind = event.get("type").and_then(str_of).unwrap_or("");
    let name = event.get("name").and_then(str_of).unwrap_or("");
    object(
        event,
        &[
            "id",
            "sequence",
            "create_at",
            "tags",
            "type",
            "name",
            "data",
        ],
        &|k, v| {
            if k == "data" {
                data(kind, name, v)
            } else {
                v.clone()
            }
        },
    )
}

fn str_of(value: &J) -> Option<&str> {
    match value {
        J::Str(s) => Some(s),
        _ => None,
    }
}
