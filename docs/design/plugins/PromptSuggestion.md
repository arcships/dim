# PromptSuggestion

Scope: session. Issue: #2026.

PromptSuggestion owns the client's reply suggestion: the offer after a turn ends, the model call that produces it, and the client's accept or dismiss. The current implementation is in `crates/dim-agent-capabilities/src/prompt_suggestion.rs`; the plugin page is [PromptSuggestion](../../architecture/rust-agent/plugins/PromptSuggestion.md).

## Use and scope: responsibility split

PromptSuggestion owns:

- **The offer.** One pending suggestion per Session, keyed by the turn that produced it; a new turn's offer replaces the pending one.
- **The decision.** Whether a suggestion is due: the Session's settings, the turn's end, and the appended compaction request.
- **The model call.** One `Once` request through the provider owner, with a streamed reply the client watches.
- **The client's answer.** Accept or dismiss the offer.

Callers keep:

- What a suggestion says: the model produces it; the client decides.
- The turn's facts: the line's Timeline answers them.
- The context usage that decides a compaction: the line's ContextUsage publishes it.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client asks for a suggestion after a turn | No: the pending offer and the due decision are the owner's | `session.suggestion.request` |
| A client accepts or dismisses the offer | No: the offer's identity and its single use are the owner's | `session.suggestion.respond` |
| The model call is made with the right provider and budget | No: the selection and the once-request are the provider owner's | `providerService.invoke` |
| A compaction changes the context | No: the line's usage is the branch owner's | the line's ContextUsage by id |

An operation or parameter exists only where the row says No.

## Caller example

The client asks and answers; the offer is identified by its turn:

```rust
let RequestReply { offer_id, text } = suggestion.request(RequestSuggestion { turn_id }, cancelled.clone()).await?;
suggestion.respond(RespondSuggestion { offer_id, accept: true }).await?;
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
| The line's ContextUsage | `read`, the usage value that decides an appended compaction request, reached by line id (#2027). |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `session.suggestion.request` | Ask for a suggestion for one turn | `{sessionId, turnId}` | `{offerId, text}` | The caller's token stops the model call; the offer is dropped | `invalid`, `cancelled`, `closed` |
| `session.suggestion.respond` | Accept or dismiss one offer | `{sessionId, offerId, accept}` | `{}` | None | `invalid`, `closed` |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The turn id is not the last turn, the offer is not pending, or the parameters are malformed |
| `cancelled` | The model call stopped; no offer remains |
| `closed` | The Session is closing; no suggestion is made |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `request` | Check the settings and the turn, resolve the model, run one `Once` request, stream to the client, answer the offer | The model call stops; the offer is dropped | The call ends; the part's tasks stop | New requests are refused; a pending offer is dropped | PromptSuggestion for the offer; the provider for the request |
| `respond` | Take the offer, answer `{}` | Nothing | The offer stays until the part unloads | A pending offer is dropped at close | PromptSuggestion |

## Exceptions with reasons

- The suggestion is one `Once` request, not the dialogue chain: a suggestion is a single paid request and must not retry or substitute a model behind the client's back.
- The offer is keyed by the turn: a new turn's offer replaces the pending one, so a client cannot accept a suggestion for a turn that has moved on.

## Verification

- A request after the last turn ends produces one offer; a second request replaces it; a request for an older turn is `invalid`.
- The model call uses the selected provider and model with `Once`; a cancelled request leaves no offer; a disabled setting makes no call.
- `respond` accepts or dismisses the pending offer once; a second answer is `invalid`.
- The part's unload drops the pending offer and stops its tasks.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact. |
| §2 scopes | Session: the offer serves one Session; the line's state is read by id. |
| §3 orthogonality | No other owner's state is read; the provider and the line's usage are reached by their operations. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact. |
| §4 rule 6 | The suggestion reaches the client, not the model context. |
| §4 rule 7 | Clients use the two methods; no caller reads the offer table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The turn id, offer id and text are data; the model reply streams through the provider's callback; no provider object crosses. |
| §7 no proxy | The provider answers the model call; the line answers its usage; this owner answers the offer. |
| §7 one interface | One name, `session.suggestion`, on the method table. |
| §7 cross-language | The methods are data. |
| §7 one change | The implementation task migrates the model call to `providerService` and the usage read to the line access in one change. |
| ADR 002 D1 | Session scope for the offer; the line's usage is the branch's. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The part depends on the provider and the Session scope; absent settings or provider only disable suggestions. |
| ADR 002 D4 | The enable flag and size bound are read at use, not at reload. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

1. [#2019](https://github.com/arcships/dim/issues/2019): `providerService.resolve` and `invoke {request, mode: "once"}` with the streamed consumer the suggestion needs.
2. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): the line access that lets this Session part read the line's ContextUsage by `(session, line)` id, and [#2027](https://github.com/arcships/dim/issues/2027)'s `read` operation for it.

## Binding appendix

The `session.suggestion` namespace is registered with the method table; the handlers are callbacks.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSuggestion { pub session_id: String, pub turn_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestReply { pub offer_id: String, pub text: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondSuggestion { pub session_id: String, pub offer_id: String, pub accept: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionError {
    Invalid { message: String },
    Cancelled,
    Closed,
}
```
