# PromptGuide

Scope: session. Issue: #2026.

PromptGuide owns the Session's input-guide table: the entries parts register for the markers they write, and the composition that turns the entries in force for one line into the System a request carries. The current implementation is described in the [Prompt](../../architecture/rust-agent/plugins/Prompt.md) page; the branch's adoption of the composition is #2027's.

## Use and scope: responsibility split

PromptGuide owns:

- **Registrations.** One entry per registering row under its scope and first marker; an entry ends when its row unloads.
- **Uniqueness per composition.** A marker has at most one entry in force for a line; a session-scoped entry meets every line, and two branch-scoped entries meet when they name the same line.
- **The frame.** The head and tail of the guide, and the two entries for the markers Prompt itself writes.
- **The composition.** Entries ordered by their own order and first marker, joined with the frame into one System.

Callers keep:

- The text they write and the markers they emit; the entry explains the marker and is the writer's.
- When to compose and what base to place: Prompt's adoption and ContextPreparation/ Loop place the System.
- Which line a request belongs to: the caller passes the line id.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part explains the markers it writes, for its row's lifetime | No: the marker uniqueness and the row's lifetime are the owner's | `register` |
| A part composes the guide for one line over a base | No: the entries in force and the frame are the owner's state | `compose` |
| A part re-composes when an entry appears or withdraws | No: a poll loop in the caller misses withdrawals and order changes | `observe`, then `compose` |
| Change an entry's text | Yes: register the new entry from the writer's next load | Business |
| Decide where the System goes | Yes: Prompt and ContextPreparation place it | Business |

An operation or parameter exists only where the row says No.

## Caller example

A writer registers from its `apply`; the entry ends with the row:

```rust
guide.register(
    ctx,
    GuideScope::Session,
    GuideEntry { markers: vec!["dim-exec".into()], order: 90, text: EXEC_ENTRY.into() },
).await?;
```

Prompt composes for its line over the adopted base, and observes changes:

```rust
let system = guide.compose(line.clone(), Some(base))?;
let _ = guide.observe(ctx, move |version| {
    let guide = guide.clone();
    let line = line.clone();
    Box::pin(async move { adopt_composed(guide.compose(line, None)) .await })
}).await?;
```

## Own contract

### Dependencies

None.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish an entry for the calling row's lifetime | `{scope, entry}` | `{}` | None | `invalid_entry`, `no_caller_row`, `duplicate_marker`, `too_large`, `closed` |
| `compose` | The System for one line over a base | `{line, base?}` | `{system}` | None; a pure read | `invalid_entry` (a malformed line id or base) |
| `observe` | The composition version now and on each change until the row unloads | `{listener}` | `{}` | The row's unload ends the subscription | `closed` |

`scope` is `"session"` or `{branch: lineId}`; `entry` is `{markers, order, text}`, `markers` at least one and each `[a-z][a-z0-9_-]{0,63}`, `text` at most 4096 bytes. `listener` is a callback by reference; it runs outside the table's lock and outside the Timeline's commit lock.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_entry` | A marker is outside the grammar, the scope's line id is empty or the entry has no marker; fix the entry |
| `no_caller_row` | The registration came from a caller with no row; register from a part's `apply` |
| `duplicate_marker` | A marker the entry names has an entry whose scope meets this one; change the marker or the scope |
| `too_large` | The text is over 4096 bytes; shorten it |
| `closed` | This Session is closing; a later generation serves the next Session |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the scope, markers, size and duplicates, record under the row, raise the version | — | The entry is withdrawn; the composition moves | New registrations are refused; the table reloads with the Session | The registering row |
| `compose` | Select the entries in force for the line, sort, join with the frame | — | Nothing | Answers the state it observed | PromptGuide |
| `observe` | Register the listener under the row and call it with the current version | The row's unload ends it | The subscription ends | The listener stays until the generation unloads | PromptGuide |

## Exceptions with reasons

- An entry whose writer registers while another composes may publish two configurations: the composition before and after the entry. The branch's adoption takes the later one; the intermediate System is the cost of not holding the composition across the writer's load.
- A composition already made does not change when an entry withdraws: Prompt's adoption decides when the System moves, so a branch that is closing records nothing.

## Verification

- A registration lives until its row unloads; an invalid marker, an empty branch id, an oversized text and a meeting duplicate are refused with their own kinds.
- `compose` orders by the entry's order then first marker; a session entry meets every line; two branch entries for different lines never meet.
- `observe` delivers the current version and each later change; the row's unload ends delivery.
- A writer's withdrawal moves the composition only at the next adoption; a closing branch adopts nothing.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact. |
| §2 scopes | Session: contributors live at both scopes and the Session is their nearest common owner; an entry's scope names where it applies. |
| §3 orthogonality | No other owner's state is read; the composition is the owner's own state. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact. |
| §4 rule 6 | The composed System reaches the model through Prompt's adoption; this owner adds no context path. |
| §4 rule 7 | Callers use these operations; no caller reads the entry table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The entry and the System are data; the listener is a callback; the line is an id; no object crosses. |
| §7 no proxy | The table answers its own entries and composition. |
| §7 one interface | One name, `guideEntries`, on the owner type; no wrapper. |
| §7 cross-language | `register`, `compose` and `observe` are data and a callback; no method exists only for Rust. |
| §7 one change | The implementation task deletes the Rust-only composition methods and migrates Prompt, ContextPreparation and Loop in one change. |
| ADR 002 D1 | Session scope. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | An entry follows its writer's lifetime; the table depends on no contributor. |
| ADR 002 D4 | No setting selects a behaviour here. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `guideEntries` registration shape; `register`, `compose` and `observe` replace `compose`/`compose_system`/`changes`.
- [Prompt §4](../plugins/Prompt.md): the entry table and the composition's Rust readers; the entry data and the composed System are unchanged.

## Binding appendix

The service is marked on `GuideEntries` under `guideEntries`. `caller: &Ctx` is the rutis caller parameter for `register` and `observe`; `CancelledFn` is not a request field.

```rust
use dim_agent_types::SystemPrompt;
use rutis::host::BoxFuture;
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideScope { Session, Branch(String) }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideEntry { pub markers: Vec<String>, pub order: i32, pub text: String }

/// A listener's own failure, reported to the table; it does not undo the composition.
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct GuideListenerError { pub code: String, pub message: String }

impl From<rutis::host::CallError> for GuideListenerError {
    fn from(error: rutis::host::CallError) -> Self {
        match error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>()) {
            Some(rutis_bridge::session::Error::Remote { name, message, .. }) => Self { code: name.clone(), message: message.clone() },
            Some(other) => Self { code: other.code().unwrap_or("failed").into(), message: other.to_string() },
            None => Self { code: "failed".into(), message: error.message().into() },
        }
    }
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum GuideError {
    #[error("{message}")]
    InvalidEntry { message: String },
    #[error("the registration has no caller row")]
    NoCallerRow,
    #[error("{message}")]
    DuplicateMarker { message: String },
    #[error("{message}")]
    TooLarge { message: String },
    #[error("the Session is closing")]
    Closed,
}

#[rutis::service(name = "guideEntries")]
impl GuideEntries {
    pub async fn register(&self, caller: &Ctx, scope: GuideScope, entry: GuideEntry) -> Result<(), GuideError> { /* ... */ }

    pub fn compose(&self, line: String, base: Option<SystemPrompt>) -> Result<SystemPrompt, GuideError> { /* ... */ }

    pub async fn observe(
        &self,
        caller: &Ctx,
        listener: impl Fn(u64) -> BoxFuture<'static, Result<(), GuideListenerError>> + Send + Sync + 'static,
    ) -> Result<(), GuideError> { /* ... */ }
}
```
