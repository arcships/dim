# SessionTitle

Scope: Session. Issue: #2033 (this contract). The generation policy, the operator's text rules and the acceptance list are restated below and in [Verification](#verification); this contract states the owner's shape and the calls it makes.

SessionTitle owns a Session's title policy: when a title is generated, what text is acceptable, when the operator's text replaces it, and the title a Session's creation requests. The title value itself belongs to SessionStore, which serializes the writes and announces each commit. The current shape is the one #2033's scan report describes. The generation policy, the instruction text and the 2000-code-point excerpt are unchanged from the pre-contract document and are restated here: one instruction, one user message carrying the first adopted input's text, the first line normalized (quotes and trailing punctuation removed, whitespace collapsed), cut to `maxLength`, then the excerpt fallback.

## Use and scope: responsibility split

SessionTitle owns:

- **The moment of generation.** The first adopted input on the Session's conversation line — an operator input with user content, or Goal's applied objective — triggers one attempt; a Session that already has a title never generates one.
- **The text rules.** The instruction sent to the model, the 2000-code-point excerpt, the output normalization (one line, no quoting, collapsed whitespace) and the length bound from the `session-title` source (`maxLength` 10..=200, default 80).
- **The routing.** An explicit model from the source, otherwise the account owner's title operation when the adopted provider belongs to it, otherwise the adopted model, otherwise the excerpt fallback; one 30 s budget across the paths; three failed attempts end the attempts for this generation. Acceptance (unchanged from the pre-contract document): the first adopted input titles while its turn runs; an interrupt or failed turn still titles; member and non-interactive Sessions get none; a rename during generation wins; a titled Session is never regenerated; a cleared title regenerates from the first adopted input; reload before the store handover writes nothing; missing or invalid settings yield the defaults or fail without rewriting; a first input withdrawn before adoption never titles; a Goal-first Session titles from the objective alone; an absent or withdrawn account owner leaves generation and rename available.
- **The manual rename.** `session.rename`'s parameter rules and the text it accepts or clears.
- **The requested initial title.** The title a launch carried is written once, as the first manual title, after the Session is published.

Callers keep as business:

- The title UI and when the operator renames.
- Which Session is shown; SessionTitle never names another Session's row.
- The creation configuration and whether the Session is interactive or a leader: SessionScope owns the record, SessionTitle reads it.

| Requirement (caller) | Can callers compose it from the owner's operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The operator sets or clears a title | No: the accepted text, the length bound and the precedence over a generated title are the owner's rules | `session.rename` |
| The title appears when the first input is adopted | No: the observation, the routing and the fallback are the owner's decisions | the owner's own task; no caller |
| A generated title never overwrites a title the operator set | No: the decision must run while the store holds the title, or a write in between is lost | `store.setTitle` with a `change` callback that refuses when a title exists |
| The title a launch requested is written once | No: the condition must be decided under the store's exclusion, and the text rule is the owner's | `store.setTitle` with a `change` that refuses unless the title is untitled |
| The title of a Session that was never titled is shown as absent | Yes: `store.title` answers the revision and the value, with its source | business composes |
| Which input the title came from | Yes: the line's facts are readable; the owner's choice is one input by id | business reads the line, the owner composes nothing new |

An operation or parameter exists only where its row says No.

## Caller example

`session.rename` checks the text, then writes it through the store's `change`; the store holds the title across the decision, so no concurrent writer can be lost.

```rust
let text = title_text(params.title.as_deref(), &document)?;   // owner rules: trim, no control characters, length
// The store's ChangeFn: the current title and revision in, the next title (none clears) or its own refusal out.
let change: ChangeFn = Arc::new(move |current: TitleState| {
    let text = text.clone();
    Box::pin(async move {
        // A manual rename always wins: the operator's text replaces whatever is stored.
        Ok(text.map(|text| Title { text, source: TitleSource::Manual }))
    })
});
let written = store
    .set_title(caller, SetTitle { id: session }, change)
    .await
    .map_err(|error| match error {
        StoreError::Rejected { .. } => invalid_params("title refused"),   // the change's refusal
        StoreError::NotFound => session_not_found(),
        other => storage_failed(other),
    })?;
```

A generation attempt: the adopted input's text, the model, and one write whose `change` refuses once any title exists.

```rust
let line = tree.directory()?.default;                    // the conversation line, as Goal resolves it
let input = tree.read(ReadLines {
    line,
    scope: None,
    from: None,
    through: Some(adopted_at),
    fields: vec!["id".into(), "data.user".into(), "data.blocks".into()],
    limit: None,
}).await?;
let text: String = eligible_text(&input).chars().take(2000).collect();
let title = match route(&document, &adopted_model) {
    Route::Account(account) => match account.title(TitleRequest { provider, text }, cancel.clone()).await {
        Ok(reply) => reply.title,
        Err(TitleError::UnsupportedProvider | TitleError::Unavailable { .. }) => generate_with_model(&provider, &text).await?,
        Err(error) => return Err(error.into()),
    },
    Route::Model(model) => generate_with_model(&model, &text).await?,
    Route::Excerpt => excerpt_title(&text, document.max_length),
};
let title = title.clone();
let change: ChangeFn = Arc::new(move |current: TitleState| {
    let title = title.clone();
    Box::pin(async move {
        if current.title.is_some() {
            // The operator's title wins; the attempt is discarded, not an error.
            return Err(StoreRejected { code: "titled".into(), message: "the Session already has a title".into() });
        }
        Ok(Some(Title { text: title, source: TitleSource::Auto }))
    })
});
store.set_title(caller, SetTitle { id }, change).await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| SessionScope ([#2025](https://github.com/arcships/dim/issues/2025), target, pending [#2093](https://github.com/arcships/dim/pull/2093)) | `config`, `identity`, `activated`, `closing` | The creation record (`initialTitle`, `interactive`, `collaborationRole`), the Session id, publication and close |
| SessionStore ([#2018](https://github.com/arcships/dim/issues/2018), target, pending [#2091](https://github.com/arcships/dim/pull/2091)) | `title`, `setTitle` with a `change` | `title` answers `{revision, title?: {text, source}}`; `setTitle(caller, SetTitle, ChangeFn)` runs the change under the owner's serialization and answers `{revision}` |
| TimelineTree ([#2024](https://github.com/arcships/dim/issues/2024), target, pending [#2092](https://github.com/arcships/dim/pull/2092)) | `directory`, `read` | The conversation line; the first adopted input's text and the adopted model at a fixed cutoff |
| Provider ([#2019](https://github.com/arcships/dim/issues/2019), target, pending [#2096](https://github.com/arcships/dim/pull/2096)) | `invoke` with `Once` | One title request on an adopted model |
| The Dim account owner ([#2019](https://github.com/arcships/dim/issues/2019), target, pending [#2096](https://github.com/arcships/dim/pull/2096); optional) | `title` | The relay's title endpoint when the adopted provider belongs to it |
| Settings ([#2017](https://github.com/arcships/dim/issues/2017), target, pending [#2094](https://github.com/arcships/dim/pull/2094)) | `register`, `read` | The `session-title` source: `enabled`, `maxLength` (10..=200, default 80), `model` |
| Session registries ([#2026](https://github.com/arcships/dim/issues/2026)) | `sessionMethods.register` | Publishing `session.rename` |

The account owner is read by its face name when generation needs it, so a profile without that owner generates through the model or the excerpt; this document does not require the account owner to exist.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `session.rename` | Set or clear the manual title | `{sessionId, title: string \| null}` | `{title, titleSource, revision}` | Before the write is admitted; after admission the store settles it | `invalid_params`, `session_not_found`, `storage_failed`, `service_reloading`, `operation_interrupted` |
| generation (internal) | Title the Session from its first adopted input | — | — | The 30 s budget or the Session's close; before the write is admitted nothing is written | none is returned to a caller; failures reach the fallback or end the attempt |

`invalid_params` is the owner's own rule: an empty title, a control character or a line separator, a title longer than `maxLength`, a parameter that is neither a string nor `null`. `session_not_found` and `storage_failed` are SessionStore's answers; `service_reloading` is the part unloading; `operation_interrupted` is the mechanism's report. Every code is one the client already receives. The store's `change` runs under its exclusion with the current title, so a lost update cannot happen and the caller keeps no retry: a manual rename returns its text, an automatic or requested title returns `Rejected` when a title exists, and the owner's serialization decides the order.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `session.rename` | Check the text, call `set_title` with a `change` that returns the manual text, answer the new revision | Before admission the call ends without a write; an admitted write is not undone | Nothing to release | `service_reloading` once the part is unloading; an admitted write settles in the store | SessionStore for the write and the exclusion; SessionTitle for nothing |
| Generation | Observe adoption, read the input and the model at a fixed cutoff, generate within the budget, fall back to the excerpt, write with a `change` that refuses when a title exists | Cancelled before admission: nothing is written; the in-flight model call is cancelled and joined | The task is cancelled; nothing is written | Cancelled before the store handover; an admitted write settles in the store | SessionTitle for the model call; SessionStore for the write and the exclusion |
| Initial title | After publication, call `setTitle` with a `change` that returns the requested text only while the title is untitled | Before the write nothing is written | Cancelled with the part | A later rename or clear is never undone by a reload: the `change` sees the stored title and refuses | SessionStore |

The generation task takes no Session activity hold: an idle release cancels it like any other close. A deleted or withdrawn input never triggers generation; the first adopted eligible input decides.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_params` | Fix the title: its length, a control character, a line break, the parameter type |
| `session_not_found` | The Session row does not exist; the client drops the rename |
| `storage_failed` | The store could not commit; retry later or report the failure |
| `service_reloading` | SessionTitle is unloading; the next generation serves the next call |
| `operation_interrupted` | The mechanism interrupted the call on unload; an admitted write settles in the store |

## Exceptions with reasons

- The account owner is an optional name, not a required dependency: generation falls back to the model and then to the excerpt, so a profile without an account owner still titles Sessions and still renames.
- A generation attempt reads the adopted input at a fixed cutoff instead of subscribing to later facts: the title is decided once from the first eligible input, and later inputs never retitle.
- The model call is one `invoke` with `Once`: a title is a single request with no retry and no model substitution, which is the mode Provider's contract names for one paid request.
- The requested initial title is written only while the title is untitled, decided inside the store's `change`: the configuration keeps the requested text, so a reload would otherwise resurrect a title the operator cleared.
- The `change` only computes: waiting on another owner or on I/O inside it holds the title's exclusion for that wait, the same rule FileStorage's `update` states.

## Verification

Flow tests through `session.create`, `session.submit`, `session.rename`, `session.summaries` and `settings.*`:

- The first adopted input starts one attempt; the title appears while the turn is still running; a second input does not retitle.
- A manual rename during generation wins; the automatic write's `change` sees the title and refuses, so the attempt is discarded.
- An image-only first input yields no title and does not rescan later inputs; a withdrawn input never triggers.
- A Goal-first Session titles from the goal's published value, not from the model text; a later operator turn does not retitle.
- No account owner: generation uses the model; no model: the excerpt; a failing relay falls back without consuming an attempt.
- Reload before the store handover writes nothing; an admitted write settles in the store and is not repeated.
- A launch's requested title is written once while untitled; a later rename or clear survives a reload because the `change` refuses.

## Supersedes

- The store's `title` and `setTitle(caller, SetTitle, ChangeFn)` shapes are SessionStore's ([#2091](https://github.com/arcships/dim/pull/2091)); [session-title-metadata.md](../appserver/session-title-metadata.md) stays that owner's document for the title value, its source and the notice, and this contract adds no title operation of its own.
- [resource-owner-operations.md](../contracts/resource-owner-operations.md): the SessionTitle row (`store.readMetadata`, `setTitle` with an existing title request). SessionTitle calls `store.title` and `store.setTitle` with a `change`.
- [resource-faces.md](../resource-faces.md): the sentence that SessionTitle uses the process repository operations.
- The Chinese mirror `docs/zh/design/plugins/SessionTitle.md` describes the pre-contract shape; its target is this document.
- This document's earlier 522-line design: its generation policy, limits, acceptance and appendices remain in force; its shape sections are replaced by this contract.

## Client-visible changes

None. `session.rename` keeps its parameters, its `{title, titleSource, revision}` reply and every error code it publishes; no conflict retry is visible because the store's `change` runs under its own exclusion. A title written by the operator still always wins, now decided inside that callback.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline event and folds no state, so it is not a Capability. |
| §2 scopes | Session: the policy and the in-flight attempt live with the Session; the title value lives in SessionStore. |
| §3 orthogonality | The part reads the line's facts and the account's answer and returns only its own decisions; the title value is the store's. |
| §4 rule 1 (commands) | The title is written through the store's operation; no other owner's state is written. |
| §4 rule 2 (handover) | The write is the handover; after admission the store owns the outcome. |
| §4 rule 3 (before callbacks) | None registered. |
| §4 rule 4 (no business meaning in core) | The instruction, the excerpt rule and the routing are SessionTitle's. |
| §4 rule 5 (batches) | No batches. |
| §4 rule 6 (context) | No model-visible text is added to the Session's context; the title request is a separate one-shot call. |
| §4 rule 7 (public events and services) | The line's published adoption facts read at a fixed cutoff, the store's operations, Provider's `invoke`, the optional account face and Settings. |
| §4 rule 8 (requirements) | One requirement on SessionStore, listed below. |
| §4 rule 9 (facts) | The title never enters the Timeline; the store announces each commit. |
| §7 values, ids and callbacks | The Session id, the title value and the revision cross; `setTitle` takes the caller's `change` by reference and the model request takes Provider's consumer; no object crosses. |
| §7 no proxy lookup | SessionTitle returns only its own decisions; the title value comes from the store and the account answer from the account owner. |
| §7 one interface | One method namespace; no service of its own; no adapter. |
| §7 cross-language | The rename is data; the internal generation has no cross-owner caller. |
| §7 one change | Deleting the optional generator name, the required auxiliary dependency and the model-text decode happens in the same change that adds the store precondition and the account call. |
| ADR 002 D1 | The policy is Session state; the title value is the store's. |
| ADR 027 D4 | SessionTitle resolves nothing by id: its dependencies are process services and its own Session's line read, both declared at load. |
| ADR 002 D2 | No wiring plugin: the part registers one namespace and calls owners' operations. |
| ADR 002 D3 | Dependencies point from SessionTitle to SessionScope, SessionStore, TimelineTree, Provider, Settings and optionally the account owner. |
| ADR 002 D4 | `enabled`, `maxLength` and `model` are business configuration read at use; enabling the plugin is structural. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

- [SessionStore #2091](https://github.com/arcships/dim/pull/2091) is open: `set_title(caller, SetTitle, ChangeFn)` running the change under the owner's serialization with `rejected` as a kind, and `title` answering `{revision, title?: {text, source}}`. SessionTitle's examples and appendix use that binding; landing order: #2091 before this contract's implementation.


## Compatibility anchors

These headings exist only so the links in other documents and code comments resolve; their content is in the sections above.

### 0.1 Fields read for one title attempt

One attempt reads four fields at a fixed cutoff: the triggering input's id, its `data.user` text or Goal's `<objective>` in `data.blocks`, the first adopted input's id, and the adopted model from `context/configured`; the read shape is in [Own contract](#own-contract).

### 2. Data and state

The title value and its revision are SessionStore's; the policy is in [Own contract](#own-contract).

### 3.2 Manual rename

See `session.rename` in [Own contract](#own-contract).

### 3.3 Generation routing and optional service

Routing is in [Own contract](#own-contract); the account owner is optional.

### 4. Timing and conflicts

A manual title always wins inside the store's change; see [Own contract](#own-contract) and [Client-visible changes](#client-visible-changes).

### 5. Context mapping

The title has no model-visible effect; see [Own contract](#own-contract).

### 9. Requirements raised to mechanism owners

See [Requirements raised to mechanism owners](#requirements-raised-to-mechanism-owners).

### A.1 `session.rename`

See `session.rename` in [Own contract](#own-contract).

### A.2 Depended-on shapes (owners define them)

The store's shapes are [#2091](https://github.com/arcships/dim/pull/2091)'s; see the dependency table in [Own contract](#own-contract).

### A.3 Configuration

`enabled`, `maxLength` (10..=200, default 80) and `model` are read from the `session-title` source; see [Own contract](#own-contract).

### Appendix B. Full conflict table

See [Own contract](#own-contract) and [Exceptions with reasons](#exceptions-with-reasons).

### Appendix C. Errors and recovery

See the error table in [Own contract](#own-contract).

### Appendix D. Acceptance

The acceptance items are in [Own contract](#own-contract)'s routing paragraph.

### Appendix E. Assumptions

See [Use and scope: responsibility split](#use-and-scope-responsibility-split).

## Binding appendix

```rust
use dim_app_server::storage::{MetadataError, SessionMetadata, Title, TitleSource};

/// The Session part reads and writes the store's own types; `set_title` takes the
/// caller's `change`, and the reply carries the committed revision.
async fn rename(&self, request: RenameRequest, cancelled: CancellationToken) -> Result<RenameReply, RenameError>;

pub struct RenameRequest {
    pub session: String,
    pub title: Option<String>,
}

pub struct RenameReply {
    pub title: Option<String>,
    pub title_source: Option<TitleSource>,
    pub revision: u64,
}

/// The method's wire errors are the ones already published; the enum only names them.
pub enum RenameError {
    InvalidParams(String),
    SessionNotFound,
    StorageFailed(String),
    ServiceReloading,
    OperationInterrupted,
}

/// `ChangeFn`, `TitleState`, `Title`, `TitleSource`, `StoreRejected` and
/// `SetTitle` are SessionStore's types ([#2091](https://github.com/arcships/dim/pull/2091));
/// this contract defines none of them. `set_title` takes `caller: &Ctx`, the
/// request and the change, and no cancellation argument.
```

The store's `title` and `setTitle` shapes are SessionStore's contract ([#2091](https://github.com/arcships/dim/pull/2091)); this appendix restates none of them.
