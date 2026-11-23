# Prompt adoption

Scope: branch. Issue: #2027.

Prompt owns the configuration a line runs with: the prompt entries in force, the adopted model and tool set, and the System the line's requests carry. The input guide's frame, order and built-in entries are PromptGuide's design in [Prompt](Prompt.md); this document is the branch part's target contract. The current implementation is described in the [Prompt plugin page](../../architecture/rust-agent/plugins/Prompt.md).

## Use and scope: responsibility split

Prompt owns:

- **The adopted configuration.** The entries in force per source and scope, the adopted model settings and tool set, the revisions and the System text, held under the branch's one projection.
- **The publication.** One atomic batch: the prompt changes, the delivery inputs and `context/configured` with the System composed over the entries in force. No other owner writes those facts.
- **Adoption order.** A tracked worker serializes loads and adoptions; an open or resume adopts the sources' current content before the line takes input; a superseded pending adoption answers without writing.
- **The tool part.** The adopted configuration's tool declarations and tool-source versions, merged from ToolBinding's current set.
- **The guide adoption.** When a guide entry loads or unloads, every open line whose composition moved publishes one configuration with its settings unchanged.
- **Recovery.** The latest effective references and revisions, then at most one committed adoption after them; no history replay for a missing legacy reference.

Callers keep as business:

- Which sources a client, a profile or a plugin loads, and their text: PromptHost and the contributing owners.
- The model a request runs: Provider and the Session's model methods.
- Which tools exist: ToolRegistry and ToolBinding.
- The System's frame and entries: PromptGuide and each entry's writer.
- What a publication failure means for a request: Loop ends the turn.

| Requirement (caller) | Can callers compose it from the operations below without breaking Prompt's invariants? | Handling |
| --- | --- | --- |
| A Session language or Agent configuration call adopts sources and a model on one line | No: the entries, the System and the configuration commit in one batch, and only the owner holds the adopted projection across the decision | `configure` |
| PromptSources' branch part adopts changed source values after polling | Yes: the same `configure` with the loaded values | Business composition |
| ToolBinding adopts the registry's current tool set | Yes: the same `configure` with the tool set as data | Business composition |
| Skills adopts its list entry when the skill list changes | Yes: the same `configure` with source values | Business composition |
| A Session method, Report or a client reads the adopted configuration | No: the adopted settings and their revision are the owner's projection | `adoptedConfiguration` |
| PromptSources' branch part compares its checked sources with what was adopted | No: the effective entries per target are the owner's projection; reading a whole snapshot duplicates it | `adoptedEntries` |
| A caller waits until a publication is visible | Yes: the operation answers after the batch publishes; a publication that cannot be confirmed is an error kind | Business with the operation's reply |

An operation exists only where the row says No.

## Caller example

The Session language update adopts the language source on the conversation line and on every loaded line:

```rust
let outcome = prompt
    .configure(
        ConfigureRequest {
            sources: Some(vec![(target, Some(entry))]),
            model: None,
            tools: None,
        },
        cancel.clone(),
    )
    .await?;
// Unchanged | Adopted { sequence } | Superseded
```

ToolBinding adopts the registry's current set on its own line:

```rust
prompt
    .configure(
        ConfigureRequest { sources: None, model: None, tools: Some(tool_set) },
        cancel.clone(),
    )
    .await?;
```

A Session method reads the adopted configuration:

```rust
let Adopted { settings } = prompt.adoptedConfiguration().await?;
```

The source poller compares only what it checked:

```rust
let AdoptedEntries { entries } = prompt
    .adoptedEntries(AdoptedEntriesRequest { targets: checked.clone() })
    .await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| PromptHost, InitialPromptConfiguration (#2026, Session) | `load`, `get` | The named source's current entry and the Session's initial targets and model. |
| PromptGuide (#2026, Session) | `compose`, `changes` | The System over the entries in force, and the signal that the composition moved. |
| BranchScope | `timeline`, `dispatcher`, `timelineIdentity`, `branchLifecycle` | The line's facts, their order, its identity and its phase. |
| SessionScope (#2025, Session) | `reportFault` | A prompt or recovery failure that stops the line's own work. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `configure` | Adopt source values, a model and a tool set as one configuration | `{sources?, model?, tools?}` | `{outcome}`: `unchanged`, `adopted{sequence}` or `superseded` | The caller cancels before the batch commits; a committed adoption is not revoked and answers `superseded` to a later one | `invalid_scope` (an entry outside the line's ancestry, or a System entry not branch-scoped), `invalid_model` (a model without the snapshot the tools need), `publication_unknown` (committed but publication unconfirmed), `cancelled`, `closed` |
| `adoptedConfiguration` | The adopted settings and their revision | `{}` | `{settings?}` | None | `closed` |
| `adoptedEntries` | The effective entries for the named targets | `{targets?}` | `{entries}` | None | `closed` |

`configure` with no source values, no model and no tools is `unchanged`. A tool-only adoption without a model configuration is `invalid_model`: the tool part is written into the configuration the line already adopted. `adoptedEntries` with no targets answers every effective entry; a target with no entry is absent.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `invalid_scope` | `configure` | Fix the request: a scope outside the line's ancestry, or a System entry that is not branch-scoped |
| `invalid_model` | `configure` | Adopt a model first, then the tool set |
| `publication_unknown` | `configure` | The batch committed but publication was not confirmed; query the line's facts after recovery instead of retrying blindly |
| `cancelled` | `configure` | Nothing was committed by the cancelled call; a committed adoption is never revoked |
| `closed` | all | The line is closing; the next generation serves a reloaded line |

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `configure` | Admit the request into the serial adoption queue, load the sources, compute the changes, compose the System over the entries in force, commit the prompt changes, the delivery inputs and `context/configured` in one batch, then answer after publication | Before the commit: nothing is written. After the commit: the adoption stands | New adoptions are refused; the queue drains or ends cancelled; the adopted projection is republished from the committed facts | Prompt: the queue, the batch and the publication; the source work by the calling row |
| open or resume adoption | Adopt the sources' current content before the line reports open, then queue the existing receipt | A cancelled open abandons the adoption and settles the waiters | The line closes without the adoption; a restored line re-reads its committed configuration | Prompt |
| guide adoption | A composition change publishes one configuration with the settings unchanged, on every open line whose composition moved | The caller's cancellation stops its own wait | A closing line publishes nothing; the listener ends with the line | Prompt |
| `adoptedConfiguration`, `adoptedEntries` | Read the projection or the effective entries | Nothing to release | `closed` | Prompt |

No request rebuilds the System from history: the System a line runs with is the one its last adoption published, and the entries reach the model only through that System or the request Loop assembles.

## Exceptions with reasons

- A guide adoption records the settings unchanged: a marker's writer loading must not make the line adopt a new model or tool set, so the adoption carries the adopted settings forward.
- A closing line publishes no guide change: its writers withdraw because it is shutting down, and a configuration recorded then would carry a System the line never ran with.
- The publication answer can be `publication_unknown`: the batch committed and publication confirmation is a separate read, and pretending either outcome would be wrong.

## Verification

- `configure` commits the entries, the delivery inputs and `context/configured` in one batch; a second adoption that wins first makes the first answer `superseded` without writing.
- A model- or tool-only adoption re-renders the guide over the recorded source text; a prompt change that renders the System composes the new source text.
- A guide entry that loads or unloads publishes one configuration with the settings unchanged only on lines whose composition moved; a closing line publishes none.
- An open adopts the sources' current content before it reports open; the first request carries the composed System.
- `adoptedConfiguration` answers the last committed configuration; `adoptedEntries` answers only the requested targets.
- A publication that cannot be confirmed answers `publication_unknown`; the facts show the committed batch.
- A reload republishes the adopted projection from the committed facts without replaying history.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds prompt facts and writes the configuration facts. |
| §2 scopes | Branch: the adopted configuration belongs to one line; the guide table is the Session's, read through PromptGuide. |
| §3 orthogonality | No other owner's state is read; the sources and the guide arrive through their owners' operations. |
| §4 rule 1 | Configure requests are commands; the owner writes the result. |
| §4 rule 2 | The adoption's handover is the committed `context/configured`; after it the line runs with that configuration. |
| §4 rule 3 | Handlers schedule the adoption worker and return; loads run in that worker. |
| §4 rule 4 | No business meaning: what an entry says stays with its writer. |
| §4 rule 5 | The prompt change, the delivery inputs and the configuration commit in one batch. |
| §4 rule 6 | Entries reach the model only through the adopted System or the request Loop assembles. |
| §4 rule 7 | Callers use the three operations; the source loaders and the guide are other owners. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | The configuration and the prompt change are published after their batch commits. |
| §7 values, ids and objects | Source values, the model and the tool set are data; the configuration is a value; no object or callback crosses. |
| §7 one interface | One name, `prompt`, on the `Prompt` type; `configureControl`, the crate-private `adopt_tools` and the `ConfigurationPublisher` are removed. |
| §7 cross-language | Every operation is data. |
| ADR 002 D1 | Branch: the configuration is per line. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2026 (PromptGuide): `compose` must answer the System for one line and a base System as data, and `changes` must deliver a value a branch part can watch; the current `compose_system` takes Rust values and the watch is Rust-only.
- #2026 (PromptRegistry): `promptHost.load` and `initialPromptConfiguration.get` as the target contracts state; Prompt calls them and adds no second registry.
- #2025 (Session access): the Session language and Agent configuration entries reach `configure` and `adoptedConfiguration` by line id through the Session access surface.

## Binding appendix

The service is marked on the `Prompt` type; the wire name stays `prompt`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::{ModelConfiguration, PromptEntry, PromptSourceTarget};
use rutis::Ctx;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<(PromptSourceTarget, Option<PromptEntry>)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<dim_agent_types::ModelSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSetAdoption>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolSetAdoption {
    pub tools: Vec<dim_agent_types::ToolDefinition>,
    pub tool_sources: Vec<dim_agent_types::ToolSourceVersion>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum ConfigureOutcome {
    Unchanged,
    Adopted { sequence: u64 },
    Superseded,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Adopted {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<ModelConfiguration>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptedEntriesRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<PromptSourceTarget>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedEntries {
    pub entries: Vec<(PromptSourceTarget, PromptEntry)>,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PromptError {
    #[error("invalid scope: {message}")]
    InvalidScope { message: String },
    #[error("invalid model: {message}")]
    InvalidModel { message: String },
    #[error("publication unknown")]
    PublicationUnknown,
    #[error("cancelled")]
    Cancelled,
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl Prompt {
    pub async fn configure(&self, request: ConfigureRequest, cancelled: CancelledFn) -> Result<ConfigureOutcome, PromptError> { /* .. */ }
    pub fn adopted_configuration(&self) -> Result<Adopted, PromptError> { /* .. */ }
    pub fn adopted_entries(&self, request: AdoptedEntriesRequest) -> Result<AdoptedEntries, PromptError> { /* .. */ }
}
```
