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
    sources.register(RegisterSource { name: name.into(), load: sources.clone() }).await?;
}
```

A branch adopts at open and reloads one source when its periodic check moves it:

```rust
let Some(update) = sources.initial().await? else { return Ok(()) };
prompt.configure(ConfigureRequest::Initial(update), cancelled.clone()).await?;

if let Some(entry) = sources.load(LoadSource { source: "dim/instructions/project".into(), scope }, cancelled.clone()).await? {
    if adopted.version != entry.version {
        prompt.configure(ConfigureRequest::Loaded { sources: vec![entry] }, cancelled.clone()).await?;
    }
}
```

The language update entry composes the record, the value and each loaded branch:

```rust
store.set_session_language(&session_id, language).await?; // #2018's record
let Changed { previous, version } = language.set(SetLanguage { language }).await?;
for line in tree.directory(None)?.lines {
    line_owner::<Prompt>(line.line)?.adopt(LanguageUpdate { version }, cancelled.clone()).await?;
}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Home, environment and shell/platform context for the environment source. |
| `S:sessionConfig` | Working directory, Session context and the language seed. |
| `P:basePromptSettingsHost` | The user's replacement of `dim/base`, read once at apply; absent keeps the factory text. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish a source for the calling row's lifetime | `{name, load}` | `{}` | None | `duplicate`, `invalid`, `closed` |
| `load` | One source's entry for a target | `{source, scope}` | `{entry?}` | The caller's token stops the load; a withdrawn source only stops later lookups | `unknown`, `cancelled`, `failed` |
| `initial` | What a new branch adopts | `{}` | `{configuration?}`: source targets and model | None | None |
| `get` | The reply language and its version | `{}` | `{language?, version}` | None | None |
| `set` | Change the reply language | `{language?}` | `{previous?, version}` | None; the owner completes it | None |

`load` is a callback by reference on the owner's side: the contributor's load runs outside the owner's locks and outside the Timeline's commit lock; an accepted load settles even after its source withdraws. `get` and `set` are values and a transition; no lock, version or wait crosses.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate` | The source name is registered already; the row must not register twice |
| `invalid` | The source name is empty or outside the source grammar |
| `unknown` | No source of that name is registered now; drop the target or re-read `initial` |
| `cancelled` | The load stopped before the entry was produced; the branch adopts nothing |
| `failed` | The source failed while producing the entry; the branch keeps its adopted text and retries on its next check |
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

1. [#2027](https://github.com/arcships/dim/issues/2027): Prompt's branch owner needs an operation reached by line id that adopts a language update (the value's version) before the update replies. The line mechanism is [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025)'s `(session, line)` access; until it exists the update cannot keep the before-reply promise and this contract records the gap.
2. [#2018](https://github.com/arcships/dim/issues/2018): the Session record's `setSessionLanguage` operation the update entry calls before `set`.

## Binding appendix

The services are marked on their owner types under `promptSources` and `sessionLanguage`. `PromptHost`, `PromptRegistry` as a second name and `InitialPromptConfiguration` leave; the shared types below are the wire shapes. `caller: &Ctx` is the rutis caller parameter for `register`; `CancelledFn` is the cancellation argument and is not a request field.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::{ConfigureUpdate, PromptEntry, PromptScope, PromptSourceTarget, ReplyLanguage};
use rutis::host::BoxFuture;
use rutis::Ctx;
use std::sync::Arc;

pub type LoadFn =
    Arc<dyn Fn(PromptSourceTarget, CancelledFn) -> BoxFuture<'static, Result<Option<PromptEntry>, String>> + Send + Sync>;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterSource { pub name: String, pub load: LoadFn }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSource { pub source: String, pub scope: PromptScope }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadReply { pub entry: Option<PromptEntry> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialReply { pub configuration: Option<ConfigureUpdate> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageValue { pub language: Option<ReplyLanguage>, pub version: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLanguage { pub language: Option<ReplyLanguage> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed { pub previous: Option<ReplyLanguage>, pub version: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceError {
    Duplicate { message: String },
    Invalid { message: String },
    Unknown { message: String },
    Cancelled,
    Failed { message: String },
    Closed,
}
```


PromptSources provides the Session's named prompt sources and the Session's prompt registry. It owns the words of the sources it authors — the base prompt and the shell guidance — and carries, but does not own, the host's system prompt and the user's and the project's `AGENTS.md`. It states the runtime environment and the date as facts. It contributes no input-guide entry: the markers it feeds are Prompt's, which writes them and explains them ([Prompt §4](Prompt.md#4-the-built-in-entries-and-their-writers)).

## 1. Sources and their classes

Each source is one name with an owner, a delivery and a class ([model-facing prompts §4](../model-prompts.md#4-protocol-and-policy)). The class is per text, not per source, where a source's block mixes them.

| Source | Delivery | Order | Author | Class |
| --- | --- | --- | --- | --- |
| `dim/base` | System | 0 | PromptSources | Policy |
| `dim/session` | System | 50 | The host or the profile | Content |
| `dim/instructions/project` | Context (`<dim-rule>`) | 100 | The project | Content |
| `dim/instructions/user` | Context (`<dim-rule>`) | 110 | The user | Content |
| `dim/environment/context` | State (`<dim-state>`) | 200 | PromptSources | Facts, plus two guidance texts (§1.2) |
| `dim/environment/date` | State (`<dim-state>`) | 210 | PromptSources | Fact |
| `dim/language` | State (`<dim-state>`) | 220 | PromptSources | Fact (designed, §3) |

The delivery decides where a change takes effect. A `System` source is part of the composed system prompt: another line inherits it through the prefix's configuration and applies it where the fact appears. A `Context` or `State` source is delivered as an Append input block, written in the same batch as its `prompt/changed`, and the change names that block's id in `inputs`; a line whose inherited prefix never consumed that input does not count the change as adopted. It adopts the source's current value itself, and the block arrives at that line's own consumption point. The same holds for an `Initialize` change: delivery decides, not the reason the change was written.

### 1.1 Policy

`dim/base` is policy: the identity sentence and the rules — safety, tone, progress updates, objectivity, engineering, git, working method, ending a turn, and the linking conventions. It is guidance about how to work; the runtime behaves the same whatever it says. The shape of a tool's input, and how its entries apply, is stated by the tool's own declaration rather than by `dim/base`.

The shell guidance is policy too. It is the sentence the environment source carries for the operator's shell (`Use POSIX shell syntax. …`, or the PowerShell, cmd, Git Bash and other editions), plus the file-encoding guidance on Windows: how to quote, how to invoke a path with spaces, which legacy encodings to ask the read tool for. None of it states a runtime fact.

Both are replaceable at PromptSources' factory configuration (§2); the product sets neither, so a candidate runs through the shipped assembly ([model-facing prompts §8.1](../model-prompts.md#81-the-rule)).

### 1.2 Facts, and the report on the environment source

The environment source states facts: the working directory, the home directory, the shell's executable and edition, its output encoding, the platform, and the time zone. The date source states the date, in the process's local time zone.

**Two texts in the environment source are guidance, not facts**: `shell_guidance` and `file_encoding_guidance`. They tell the model how to work; the runtime would behave identically if they said something else, and they are false of nothing. They stay where they are — inside the environment block, as the model reads them today — and they become policy members of the factory configuration. Moving them out of the state block would change what every request carries, which is a text change this issue does not make.

### 1.3 Content

`dim/session` is the host's or the profile's system prompt: carried, never edited. The user's and the project's `AGENTS.md` are the instruction bodies: read as files, escaped with the context-block escape rule so their text cannot forge a marker, and carried verbatim — the project's with a one-line header naming its path and that it applies to that directory and below, the user's with a header naming it as `~/AGENTS.md`. Neither is summarized, rewritten or trimmed beyond the surrounding whitespace. A missing or unreadable file produces no entry rather than an empty one.

## 2. Policy at the factory configuration

The factory configuration carries the three policy texts, in the shape [the summary strategy's](../plugins/ContextCompaction.md#134-evaluation) optional system prompt uses: an optional member each, `None` keeping the built-in text, and the product profile setting none of them.

| Member | Replaces |
| --- | --- |
| `base` | `dim/base`'s identity and rules |
| `shell_guidance` | The shell guidance the environment source carries, per shell edition |
| `file_encoding_guidance` | The file-encoding guidance the environment source carries |

A candidate is installed through the shipped assembly — the same plugin, constructed with a configuration — never by patching text at a request boundary ([model-facing prompts §8.1](../model-prompts.md#81-the-rule)).

### 2.1 The product's user override

The product profile sets no factory text, so the App Server may supply the operator's `base` from its user settings ([prompt overrides](../appserver/prompt-overrides.md)). The port is `BasePromptSettingsHost`, optional and read while the Session's sources are assembled: the factory `base` outranks it, a read failure keeps the built-in text, and the value applies to Sessions loaded after the change. The port replaces one field; the shell and file-encoding guidance stay factory-only, because their shape is per shell edition and platform.

## 3. The "Agent prompt language" state source

The source names the language the model writes to the operator in ([model-facing prompts §7](../model-prompts.md#7-language)). It settles the case where the operator's environment language and their messages' language disagree — English logs or code pasted into a Chinese session — by making the answer language an explicit choice instead of an inference from the last message.

**The block.** Source `dim/language`, delivery State, order 220, so it follows the environment and the date. Its body is one sentence, the language named in English:

```text
<dim-state source="dim/language">The operator set this session's reply language to Simplified Chinese.</dim-state>
```

The body states the setting itself, and asks for nothing: a state block is read as the situation, never as an instruction, and it names the language the model writes to the operator in ([model-facing prompts §7](../model-prompts.md#7-language)). The language names are the ones the operator selects — English, Simplified Chinese, Traditional Chinese, Japanese, Korean, Bahasa Indonesia, German, French, Spanish, Portuguese (Brazil), Portuguese (Portugal). The sentence's wording is policy: it is judged against the current text when the source is built ([model-facing prompts §8](../model-prompts.md#8-judging-a-change)), and this design does not build it.

**No selection, no block.** When the operator has chosen no language — the system default — the source produces no entry and the base sentence's rule stands.

**The carrier.** The client carries the operator's choice into the Session: `session.create {language?}` seeds it and `session.language.update` changes it, and the value is `SessionConfig.language`, an optional member of the Session's config record. The daemon never reads the client's interface-locale file. The PromptSources Session part provides the Session's live value as the `SessionLanguage` cell and reads it on every load; the cell's `get` is the `sessionLanguage` data face, which other languages read by name, and the branch part re-exposes that cell on each branch for the Session methods that change it, as the Skill list is re-exposed. The face has no write: an update also writes the Session record and has Prompt adopt the source. A Session created without a choice, including a record written by an older client, has no value and produces no entry. An update adopts the value on `main` and, before it replies, on every loaded branch, so that branch's next request carries it without a wake; a branch loaded later reads the current value when it loads, and each branch's periodic check remains the fallback.

## 4. Dependencies

| Service | Purpose |
| --- | --- |
| `S:PromptRegistry` | Register each source by name, until this part unloads |
| `P:HostPlatform` | Home, environment and the operator's shell |
| `S:SessionConfig` | Working directory and the host's system prompt |

The language source reads the part's own `SessionLanguage` cell rather than an injected service.

## 5. Cross-module requirements

1. **The operator's language for the Session** (§3): the Session's config carries it (`SessionConfig.language`), `session.create {language?}` seeds it and `session.language.get/update` read and change it, and this plugin reads the Session's live value. No configuration file carries it, and no other module decides it.
2. **The plugin bridge** ([Prompt §6.1](Prompt.md#61-the-plugin-bridge)): no further requirement reaches PromptSources; the markers its sources feed are Prompt's, and Prompt's page raises what they need.

## 6. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Plugins and capabilities bounded | The sources, their classes and their texts are PromptSources'; the guide's frame and the entries of the markers it writes are Prompt's |
| §3 Orthogonality | The sources travel on the existing prompt-source contract; this design adds no route of its own |
| §4 rule 1 Apply registers, cleanup releases | The sources register from `apply` and end with the part's generation |
| §4 rule 6 Context from consumed events | Sources keep reaching the model through adoption and Append inputs; this design adds no path |
| §4 rule 7 Public events and services | The source adds no protocol method or event of its own: it is one more name in an existing set. The Session's value is a service the owning part provides at the Session scope and re-exposes on each branch, as the Skill list does (ADR 002 D1, D2) |
| §4 rule 8 Infrastructure unchanged | None is changed. The language fact is raised, not built |
| ADR 002 D1 Scope | The sources live in the Session, where their state lives |
| D4 Configuration | No configuration file is added; the policy members are the plugin's factory configuration, as the summary strategy's system prompt is |
| D5 Language-neutral contracts | The language source is a state block, data on an existing channel |
| ADR 008 D2 No delivery status | The document states the design only |
| ADR 008 D3 Own scope only | The guide's route and the frame stay on [Prompt](Prompt.md)'s page |

## 7. Binding appendix

```rust
/// Factory configuration of the sources' policy texts. The product sets
/// none of them, so the built-in texts are in force.
#[derive(Clone, Default)]
pub struct PromptSourcesConfig {
    /// `dim/base` in place of the built-in identity and rules.
    pub base: Option<Arc<str>>,
    /// The shell guidance the environment source carries, per edition.
    pub shell_guidance: Option<Arc<dyn Fn(ShellEdition) -> String + Send + Sync>>,
    /// The file-encoding guidance the environment source carries.
    pub file_encoding_guidance: Option<Arc<str>>,
}

impl PromptSourcesPlugin {
    /// The plugin with the built-in policy texts in force.
    pub fn new(names: &Names<'_>, today: Today) -> rutis::Result<Self>;
    /// The plugin with a candidate's texts at its factory configuration.
    pub fn with_config(
        names: &Names<'_>,
        today: Today,
        config: PromptSourcesConfig,
    ) -> rutis::Result<Self>;
}

/// Registers PromptSources with the policy texts at `config`.
pub fn install_with_config(
    builder: &mut AgentBuilder,
    today: Today,
    every: Duration,
    config: PromptSourcesConfig,
);

/// Registers PromptSources with the built-in texts: `install` and
/// `install_with_clock` are this entry with the default configuration.
pub fn install(builder: &mut AgentBuilder);
```

The language source's binding is one more arm of the source loader: name `dim/language`, delivery `State`, order 220, body from the operator's setting when one is carried and no entry when none is.

## Reading and adopting current source values

Opening loads the current sources once. The periodic branch checker starts after that initial opening interval; a checker mounted on an already active branch still checks immediately. Each instructions source reads only its own file body. The checker compares each loaded target through the existing state watch and hands changed values to the existing resolved-source adoption; it does not reload their names. Language adoption keeps the Session language gate through its final load and adoption. No file cache or additional persistent event is introduced.
