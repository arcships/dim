# PromptSources

Scope: session. Issue: #2026.

PromptSources is the Session's prompt sources and the Session's reply language. Two owners share this document because one part holds both states: **PromptSources** owns the registered sources, the load operation and the initial configuration; **SessionLanguage** owns the reply-language value and its version. The current implementation is described in the [PromptSources plugin page](../../architecture/rust-agent/plugins/PromptSources.md). The branch polling and Prompt's adoption are #2027's; this document states what they call.

## Use and scope: responsibility split

PromptSources owns:

- **Source registrations.** One named source per registering row: its load callback and its lifetime; a registration ends when its row unloads.
- **Loading one source for a target.** The target names the source and the branch, turn or task scope; the reply is one entry or none.
- **The initial configuration.** The source targets and model a new branch adopts, read when a branch opens.
- **The built-in sources.** `dim/base`, `dim/session`, project and user instructions, environment, date and language, registered at apply.
- **The language value.** The operator's reply language, its version, and the transition that changes it.

Callers keep:

- What a source says: its text, its version and its delivery; Prompt adopts entries and owns the adopted prompt state.
- Which sources a branch checks and when: the branch part's polling (#2027).
- The Session record write for a language change: the Session store owns the record (#2018); the update entry composes the record write, the value change and each loaded branch's adoption.
- The policy texts' content: the factory configuration supplies them; this owner stores and serves them.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part publishes a named source for its row's lifetime | No: the name's uniqueness and the row's lifetime are the owner's | `register` |
| A branch loads one source for a target | No: the source's callback is the owner's; a caller holding the callback would carry the contributor's object | `load` |
| A branch opens and adopts every source registered then | No: the target set and the model come from the owner's state | `initial` |
| A branch checks a source again later | Yes: `load` with the same target | Business |
| The branch knows whether it has the current text | Yes: the entry carries its version; the branch compares | Business |
| Read the reply language | No: the live value is the owner's | `get` |
| Change the reply language | No: the value, its version and the transition are the owner's | `set` |
| Make a changed language visible on every loaded branch before replying | No: each branch's adoption is its own operation; the update entry calls it per line | Business composition (Requirements 1) |

An operation or parameter exists only where the row says No.

## Caller example

The built-in sources register at apply, and Skills registers its own source the same way:

```rust
for name in SOURCES {
    sources.register(ctx, name.into(), |target| Box::pin(sources.load_one(target))).await?;
}
```

A branch adopts at open and reloads one source when its periodic check moves it:

```rust
// The branch's open reads the targets and model; Prompt adoption loads each target's current content.
let Some(update) = sources.initial().configuration else { return Ok(()) };
let ConfigureUpdate { sources: targets, model } = update;

// A periodic check adopts one changed entry; ConfigureRequest is Prompt adoption's.
if let Some(entry) = sources.load("dim/instructions/project".into(), scope, cancelled.clone()).await? {
    if adopted.version != entry.version {
        prompt.configure(ConfigureRequest {
            sources: Some(vec![(target, Some(entry))]),
            model: None,
            tools: None,
        }, cancelled.clone()).await?;
    }
}
```

The language update entry composes the record, the value and each loaded branch:

```rust
store.set_language(&session_id, language).await?; // #2018's record
let Changed { previous, version } = language.set(language).await?;
for line in tree.directory(None)?.lines {
    line_owner::<Prompt>(line.line)?.configure(ConfigureRequest {
        sources: Some(vec![(language_target(), Some(language_entry(language)))]),
        model: None,
        tools: None,
    }, cancelled.clone()).await?;
}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Home, environment and shell/platform context for the environment source. |
| `S:sessionScope` | The creation record: working directory, Session context and the language seed (`config`). |
| `P:basePromptSettingsHost` | The user's replacement of `dim/base`, read once at apply; absent keeps the factory text. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a source for the calling row's lifetime | `{name, load}` | `{}` | None | `duplicate`, `invalid`, `closed` |
| `load` | One source's entry for a target | `{source, scope}` | `{entry?}` | The caller's token stops the load; a withdrawn source only stops later lookups | `unknown`, `cancelled`, `failed`, `closed` |
| `initial` | What a new branch adopts | `{}` | `{configuration?}`: source targets and model | None | None |
| `get` | The reply language and its version | `{}` | `{language?, version}` | None | None |
| `set` | Change the reply language | `{language?}` | `{previous?, version}` | None; the owner completes it | None |

`load` is a callback by reference on the owner's side: the contributor's load runs outside the owner's locks and outside the Timeline's commit lock; the callback's future is dropped when the caller's token fires; an accepted load settles even after its source withdraws. `get` and `set` are values and a transition; no lock, version or wait crosses.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate` | The source name is registered already; the row must not register twice |
| `invalid` | The source name is empty or outside the source grammar |
| `unknown` | No source of that name is registered now; drop the target or re-read `initial` |
| `cancelled` | The load stopped before the entry was produced; the branch adopts nothing |
| `failed` | The source failed while producing the entry; `cause.code` and `cause.message` are the source's own; the branch keeps its adopted text and retries on its next check |
| `closed` | This Session is closing; a later generation serves the next Session |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | Check the name and record the callback under the row | — | The registration is withdrawn; accepted loads settle | New registrations are refused; the registry reloads with the Session | The registering row |
| `load` | Find the source, call the callback with the target, return the entry | The load ends `cancelled`; the callback's own I/O is the contributor's to stop | The load ends `cancelled`; a load already accepted settles by the source's rule | Pending loads settle or end `closed` | PromptSources for the lookup; the source for its content |
| `initial` | Read the registered names and the model into one value | — | Nothing to release | Pending reads answer the state they observed | PromptSources |
| `get` | Read the value and version | — | Nothing | Answers the state it observed | SessionLanguage |
| `set` | Take the value's gate, replace the value, raise the version | — | Nothing | Refused once the part unloads | SessionLanguage |

## Exceptions with reasons

- The base prompt setting is read while the part applies: a read failure keeps the factory text, and the source serves it for the Session's life; no per-load read means a later user change waits for the next Session.
- A withdrawn source does not cancel a load it already accepted: the source's I/O is its own, and the branch's comparison by version makes a late entry harmless.
- A source's failure keeps its own `code` and `message`: the registry adds no code of its own, so the branch can act on the source's reason.
- The language value is a cell with a version, not a per-branch copy: every branch reads the same value and decides by version whether it adopted it; no branch holds the value's lock.

## Sources and classes

Each source is one name with an owner, a delivery and a class. The delivery decides where a change takes effect: a `System` source is part of the composed system prompt; a `Context` or `State` source is delivered as an Append input block.

| Source | Delivery | Order | Author | Class |
| --- | --- | --- | --- | --- |
| `dim/base` | System | 0 | PromptSources | Policy |
| `dim/session` | System | 50 | The host or the profile | Content |
| `dim/instructions/project` | Context (`<dim-rule>`) | 100 | The project | Content |
| `dim/instructions/user` | Context (`<dim-rule>`) | 110 | The user | Content |
| `dim/environment/context` | State (`<dim-state>`) | 200 | PromptSources | Facts, plus two guidance texts |
| `dim/environment/date` | State (`<dim-state>`) | 210 | PromptSources | Fact |
| `dim/language` | State (`<dim-state>`) | 220 | PromptSources | Fact |

`dim/base` is the identity sentence and the rules; the environment source's shell and file-encoding guidance is policy too. Both are replaceable at the factory configuration (`base`, `shell_guidance`, `file_encoding_guidance`), and the product sets none. The environment source states the working directory, home, shell executable and edition, output encoding, platform and time zone; the date source states the date in the process's local time zone. `dim/session` is the host's or the profile's system prompt, carried and never edited. The user's and project's `AGENTS.md` are read as files, escaped with the context-block escape rule and carried verbatim; a missing or unreadable file produces no entry.

## The language source

Source `dim/language` states the operator's language in one sentence, and no entry when the operator selected none:

```text
<dim-state source="dim/language">The operator set this session's reply language to Simplified Chinese.</dim-state>
```

The value is `SessionConfig.language`, seeded by `session.create {language?}` and changed by `session.language.update`; the daemon never reads the client's interface-locale file. The update writes the record, calls `set`, and has each loaded branch adopt the new version before it replies; a branch loaded later reads the current version at open, and each branch's periodic check remains the fallback.

## Verification

- A registration lives until its row unloads; a duplicate name and an empty name are refused; a load of an unregistered name is `unknown`.
- A load forwards the caller's cancellation and settles normally after its source withdraws; a failing source answers `failed`.
- `initial` lists the sources registered then and the Session's model; a source registered later appears at the next branch open.
- `get` answers the language and version; `set` answers the previous value and a higher version; a branch that already adopted the version does not adopt again.
- The language update writes the record, calls `set`, and each loaded line adopts before the reply; a branch opened later reads the current version at open.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | PromptSources is one Session part holding two owners' state; it writes no Timeline fact and folds none. Branch polling is a separate branch part (#2027). |
| §2 scopes | Session: sources and the language value belong to one Session; a branch reads by target and by version. |
| §3 orthogonality | No other owner's state is read or changed; the base prompt text is read through the host face. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, business meaning in the core, batch, or Timeline fact. |
| §4 rule 6 | Entries reach the model only after Prompt adopts them; this owner adds no context path. |
| §4 rule 7 | Callers use these operations; no caller reads a source callback or the value's lock. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | Targets, entries, the initial configuration and the language value are data; the source load is a callback; the language adoption by line is an id-addressed operation; no source object or lock crosses. |
| §7 no proxy | The registry answers its own sources; the base text is the host's; the language value is its own. |
| §7 one interface | `promptSources` and `sessionLanguage` name the two owners' own operations; `promptHost`, `promptRegistry` and `initialPromptConfiguration` leave the catalog; no wrapper type remains. |
| §7 one change | The implementation task deletes the wrappers, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Session scope: sources and the language value live with one Session. |
| ADR 002 D2 | No wiring plugin: the operations are on the owners' types. |
| ADR 002 D3 | A source follows its part's lifetime; the registry depends on no contributor. |
| ADR 002 D4 | The policy texts are factory configuration; no business setting is read per load. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. [#2027](https://github.com/arcships/dim/issues/2027): Prompt's branch owner needs a `configure` operation reached by line id that adopts a language update (the value's version) before the update replies. The line mechanism is [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025)'s `(session, line)` access; until it exists the update cannot keep the before-reply promise and this contract records the gap.
2. [#2018](https://github.com/arcships/dim/issues/2018): the Session record's `setLanguage` operation the update entry calls before `set`.

## Supersedes

- [model-facing prompts §7](../model-prompts.md#7-language): the `SessionLanguage` cell read by a branch gate; the gate is replaced by the versioned value and `sessionLanguage.get`/`set`.
- [session boundary A.2 and A.3](../contracts/session-boundary.md): the `promptRegistry`, `promptHost` and `initialPromptConfiguration` faces; `promptSources.register`, `load`, `initial` and `sessionLanguage.get`/`set` replace them. The client methods `session.language.*` are unchanged.
- [service faces §4.8](../service-faces.md): the `promptSources` and `sessionLanguage` rows.

## Binding appendix

The services are marked on their owner types under `promptSources` and `sessionLanguage`; `ConfigureRequest` and `ConfigureOutcome` are [Prompt adoption](https://github.com/arcships/dim/pull/2102)'s types, used by the examples. `PromptHost`, `PromptRegistry` as a second name and `InitialPromptConfiguration` leave. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is the cancellation argument and is not a request field. A source's failure arrives as `CallError`; `PromptCallbackError` keeps its own `code` and `message`.

```rust
use crate::host::CancelledFn;
use dim_agent_types::{PromptEntry, PromptScope};
use dim_agent_runtime::{ConfigureUpdate, PromptSourceTarget, ReplyLanguage};
use rutis::host::{BoxFuture, CallError};
use rutis::Ctx;

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SourceError {
    #[error("{message}")]
    Duplicate { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    Unknown { message: String },
    #[error("the load was cancelled")]
    Cancelled,
    #[error("{cause}")]
    Failed { cause: PromptCallbackError },
    #[error("the Session is closing")]
    Closed,
}

#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct PromptCallbackError { pub code: String, pub message: String }

impl From<CallError> for PromptCallbackError {
    fn from(error: CallError) -> Self {
        match error.raw().and_then(|raw| raw.downcast_ref::<rutis_bridge::session::Error>()) {
            Some(rutis_bridge::session::Error::Remote { name, message, .. }) => {
                Self { code: name.clone(), message: message.clone() }
            }
            Some(other) => Self { code: other.code().unwrap_or("failed").into(), message: other.to_string() },
            None => Self { code: "failed".into(), message: error.message().into() },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadReply { pub entry: Option<PromptEntry> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialReply { pub configuration: Option<ConfigureUpdate> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageValue { pub language: Option<ReplyLanguage>, pub version: u64 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed { pub previous: Option<ReplyLanguage>, pub version: u64 }

#[rutis::service(name = "promptSources")]
impl PromptSources {
    pub async fn register(
        &self,
        caller: &Ctx,
        name: String,
        load: impl Fn(PromptSourceTarget) -> BoxFuture<'static, Result<Option<PromptEntry>, PromptCallbackError>> + Send + Sync + 'static,
    ) -> Result<(), SourceError> { /* ... */ }

    pub async fn load(&self, source: String, scope: PromptScope, cancelled: CancelledFn) -> Result<LoadReply, SourceError> { /* ... */ }

    pub fn initial(&self) -> InitialReply { /* ... */ }
}

#[rutis::service(name = "sessionLanguage")]
impl SessionLanguage {
    pub fn get(&self) -> LanguageValue { /* ... */ }
    pub async fn set(&self, language: Option<ReplyLanguage>) -> Changed { /* ... */ }
}
```

## Previous sections

### 1.3 Content

See [Sources and classes](#sources-and-classes).

## 2. Policy at the factory configuration

See [Sources and classes](#sources-and-classes).

## 3. The "Agent prompt language" state source

See [The language source](#the-language-source).
