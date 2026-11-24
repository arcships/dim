# PromptSuggestion

Scope: session. Issue: #2026.

PromptSuggestion owns the client's reply suggestion: the offer after a turn ends, the model call that produces it, and the client's accept or dismiss. The current implementation is in `crates/dim-agent-capabilities/src/prompt_suggestion.rs`; the plugin page is [PromptSuggestion](../../architecture/rust-agent/plugins/PromptSuggestion.md).

## Use and scope: responsibility split

PromptSuggestion owns:

- **The offer.** One pending suggestion per Session, keyed by the turn that produced it; a new turn's offer replaces the pending one.
- **The decision.** Whether a suggestion is due: the Session's settings, the turn's end, and the appended compaction request.
- **The model call.** One `Once` request through the provider owner, with a streamed reply the client watches.
- **The client's answer.** Accept or dismiss the offer, by turn id.

Callers keep:

- What a suggestion says: the model produces it; the client decides.
- The turn's facts: the line's Timeline answers them.
- The context usage that decides a compaction: the line's ContextUsage publishes it.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client asks for a suggestion after a turn | No: the pending offer, the throttle and the due decision are the owner's | `session.suggestion.request` |
| A client accepts or dismisses the offer | No: the offer's identity and its single use are the owner's | `session.suggestion.respond` |
| The model call is made with the right provider and budget | No: the selection and the once-request are the provider owner's | `providerService.resolve`, `invoke` |
| A compaction changes the context | No: the line's usage is the branch owner's | `line_owner::<ContextUsage>(line)?.read(...)` |

An operation or parameter exists only where the row says No.

## Caller example

The client asks and answers; the offer arrives as the Session notice `offer {turnId, text}`:

```rust
let accepted = suggestion.request(RequestParams { session_id: session.clone(), turn_id: turn.clone() }, cancelled.clone()).await?;
// accepted is {accepted: true} or {accepted: false, reason}; the text arrives as the offer notice.
suggestion.respond(RespondParams { session_id: session.clone(), turn_id: turn, action: Action::Accept })?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:sessionScope` | `config` for `interactive` and the role (only a leader's interactive Session offers suggestions), `identity` for the Session id, and `activated`/`closing`: the part works only while the Session runs. |
| `S:sessionMethods` | Register `session.suggestion` until this part unloads. |
| `P:providerService` | `resolve` and `invoke` with `Once` for the suggestion request (#2019). |
| `P:promptSuggestionSettingsHost` | The user's enable flag and size bound, read per decision. |
| `S:timelineTree` | The line's directory record and the turn's end fact. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `session.suggestion.request` | Ask for a suggestion for one completed turn | `{sessionId, turnId}` | `{accepted: bool, reason?}` | The caller's token stops the preflight and the model call; the offer is dropped | `invalid`, `cancelled`, `closed` |
| `session.suggestion.respond` | Accept or dismiss one offer | `{sessionId, turnId, action: "accept" \| "dismiss"}` | `{}` | None | `invalid`, `closed` |

The reply stays the client's: `accepted` means the model call started, and `reason` is one of the stable codes (`disabled`, `not_root`, `branch_changed`, `not_latest_turn`, `turn_not_completed`, `no_text`, `throttled`, `compaction_due`, `new_input`, `turn_started`, `dismissed`, `accepted`, `session_closed`, `timeout`, `failed`, `invalid`). The offer and its clearing arrive as the `PromptSuggestion` notices `offer {turnId, text}` and `cleared {turnId, reason}`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The parameters are malformed; the client fixes the request |
| `cancelled` | The preflight or the model call stopped; no offer remains |
| `closed` | The Session is closing; no suggestion is made |

A policy refusal is the `{accepted: false, reason}` reply, not an error: the client acts on the reason.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `request` | Check the settings and the turn, read the line's usage, resolve the model, run one `Once` request, emit the offer notice, answer `accepted` | The model call stops; the offer is dropped; a later answer changes nothing | The call ends; the part's tasks stop | New requests are refused; a pending offer is dropped | PromptSuggestion for the offer; the provider for the request |
| `respond` | Take the offer, emit `cleared` with `accepted` or `dismissed`, answer `{}` | Nothing | The offer stays until the part unloads | A pending offer is dropped at close | PromptSuggestion |

## Exceptions with reasons

- The suggestion is one `Once` request, not the dialogue chain: a suggestion is a single paid request and must not retry or substitute a model behind the client's back.
- The offer is keyed by the turn: a new turn's offer replaces the pending one, so a client cannot accept a suggestion for a turn that has moved on.
- A policy refusal is a reply, not a Timeline fact: the client shows the reason and the model never sees the attempt.

## Verification

- A request after the last turn ends produces one offer notice; a second request replaces it; a request for an older turn answers `{accepted: false, reason: "not_latest_turn"}`.
- The model call uses the selected provider and model with `Once`; a cancelled request leaves no offer; a disabled setting answers `{accepted: false, reason: "disabled"}`.
- `respond` accepts or dismisses the pending offer once and emits `cleared`; a second answer changes nothing and answers `{}`.
- The client's `{accepted, reason}` reply and the `offer`/`cleared` notices are byte-identical to today's for the same facts.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact. |
| §2 scopes | Session: the offer serves one Session; the line's state is read by id. |
| §3 orthogonality | No other owner's state is read; the provider and the line's usage are reached by their operations. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact. |
| §4 rule 6 | The suggestion reaches the client as a notice, not the model context. |
| §4 rule 7 | Clients use the two methods; no caller reads the offer table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The turn id and the text are data; the model reply streams through the provider's callback; no provider object crosses. |
| §7 no proxy | The provider answers the model call; the line answers its usage; this owner answers the offer. |
| §7 one interface | One name, `session.suggestion`, on the method table. |
| §7 cross-language | The methods are data; the notices are data. |
| §7 one change | The implementation task migrates the model call to `providerService` and the usage read to `line_owner` in one change. |
| ADR 002 D1 | Session scope for the offer; the line's usage is the branch's. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The part depends on the provider and the Session scope; absent settings or provider only disable suggestions. |
| ADR 002 D4 | The enable flag and size bound are read at use, not at reload. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

1. [#2019](https://github.com/arcships/dim/issues/2019): `providerService.resolve` and `invoke {request, mode: "once"}` with the streamed consumer the suggestion needs.
2. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): `line_owner::<T>(line)` gives this Session part the line's ContextUsage data face by id; [#2027](https://github.com/arcships/dim/issues/2027)'s `read` operation for it.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `session.suggestion.request`/`respond` shapes stay; the part's usage read changes from `TreeCommands`/`ContextPreparation` to `line_owner::<ContextUsage>(line)?.read(...)`.

## Binding appendix

The `session.suggestion` namespace is registered with `sessionMethods.register`; the handlers are callbacks. The offer cache and the in-flight generation are private.

```rust
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action { Accept, Dismiss }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestParams { pub session_id: String, pub turn_id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestReply {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondParams { pub session_id: String, pub turn_id: String, pub action: Action }
```

## Previous sections

## 9. Relation to Timeline branches

See [Own contract](#own-contract).

## 15. Acceptance

See [Verification](#verification).
