# Context projection

Scope: branch. Issue: #2025.

The owner folds its own branch's facts into the context a model request carries, renders that context to messages, and encodes or decodes the recovery snapshot that lets a later fold start after a compaction. Today the same need is the `historyRenderer` value: a table of Rust functions at Session scope, whose methods take a caller-supplied `&Timeline` or `&Session`, plus a Session-level `context_in` that folds another branch's history by id. This document replaces the value with branch-scoped operations.

## Use and scope: responsibility split

The owner answers:

- **The context at a cutoff.** Which events place context, how a compaction's replacement and baseline apply, and the positions the fold covered.
- **Rendered entries.** The context and its `HistoryEntry` list at one cutoff, for callers that need both.
- **A request-local projection.** The rendered entries and the compaction in force at a cutoff, folded page by page; the fold releases each page and the reply carries no event list, so a long history is not held whole.
- **Recovery snapshots.** Encode the projection at a completed compaction's cut, validate a saved snapshot against its anchor, and decode the checkpoint it carries.

Callers keep:

- Which cutoff a request uses: Loop's committed position and its fixed `through`.
- Whether a saved count or projection may be reused: ContextPreparation's validation rules.
- When a snapshot is saved and which interpreter is current: ContextPreparation's compaction flow.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Loop builds a request's context at a fixed cutoff | No: which events carry context, how compaction replaces a range and how the baseline folds are the owner's knowledge; composing them from `timeline.query` puts a second projection in the caller | `renderedStream`, `renderedContext` |
| ContextPreparation checks the budget at its own cutoff and compacts | Yes: the same reads, and the projection Loop hands over | Business composition |
| A compaction saves its snapshot after the fact commits | No: encoding and the anchor check are one rule with decoding | `snapshot`, `streamCheckpoint` |
| A later preparation resumes from a saved snapshot | No: the owner validates the snapshot against its anchor before decoding | `snapshotFromCheckpoint`, `snapshot` |
| A test or an example inspects a folded context | Yes: `renderedContext` answers the context and the entries | Business |

## Caller example

```rust
// Loop: the request's context at the committed cutoff.
let streamed = projection
    .rendered_stream(RenderedStreamRequest { through, batch: 256 })
    .await?;
let request = settings.request(streamed.history.iter().map(|entry| entry.message.clone()).collect());
```

```rust
// ContextPreparation: the snapshot of a completed compaction.
let snapshot = projection
    .snapshot(SnapshotRequest { context, retain_from: cut, anchor: fact.clone() })
    .await?;
```

```rust
// A later preparation: the checkpoint a saved snapshot carries, if the anchor matches.
let Some(checkpoint) = projection
    .snapshot_from_checkpoint(SnapshotFromCheckpointRequest { checkpoint, anchor })
    .await?
else {
    return Ok(None); // the snapshot does not resume here; the fold starts earlier
};
```

## Own contract

### Dependencies

`B:Timeline` — the owner folds the branch's own facts through the branch's Timeline operations; it never asks another branch or the Session for events. The projection policy is factory configuration: the assembly installs the owner with the policy in force, and a host that needs another policy installs another part.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `renderedStream` | The rendered entries and the compaction in force at a cutoff, folded page by page, without retaining events | `{through, batch}` | `{history, configuration?, prompt?, ancestry, compaction?, branches, compactionCut?}` | The caller drops the call; the fold stops at its last page | `future_sequence`, `invalid`, `storage`, `closed` |
| `renderedContext` | The context and rendered entries at a cutoff, with the events the projection kept | `{through}` | `{context, history}` | The caller drops the call | `future_sequence`, `invalid`, `storage`, `closed` |
| `snapshot` | Encode the projection at a completed compaction's cut | `{context, retainFrom, anchor}` | `{snapshot}` | None; the owner completes the read | `invalid`, `storage`, `closed` |
| `streamCheckpoint` | The checkpoint of the fold that starts after a compaction's cut | `{through, retainFrom, batch}` | `{checkpoint}` | The caller drops the call | `future_sequence`, `invalid`, `storage`, `closed` |
| `snapshotFromCheckpoint` | A snapshot that carries an already-computed checkpoint | `{checkpoint, anchor}` | `{snapshot}` or none when the anchor does not match | None | `invalid`, `storage`, `closed` |

`through` is a committed sequence of this branch; `batch` is the page size of the fold; `retainFrom` is the id of the compaction's cut event; `anchor` is the completed compaction fact a snapshot anchors to.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `future_sequence` | `renderedStream`, `renderedContext`, `streamCheckpoint` | Fix the cutoff: it is above the branch's committed head |
| `invalid` | all | Fix the request: a batch size of zero, a cut outside the branch chain, an anchor that is not a completed compaction, a snapshot whose interpreter is not current |
| `storage` | all | The branch's facts could not be read; the request fails for this attempt; a retry re-reads |
| `closed` | all | The branch is closing; stop preparing requests |

`snapshotFromCheckpoint` answers none, not an error, when the snapshot cannot be used at that anchor: the caller folds from an earlier point.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `renderedStream` | The owner selects the snapshot a checkpoint validates, folds page by page from its resume position, releases each page, and answers the streamed projection | A dropped call stops the fold at its last page; no state is written | An in-flight fold ends `closed`; the generation answers no further call | The calling row drops the call; the owner holds no resource between pages |
| `renderedContext` | The owner folds the same content without the request-local form | A dropped call ends the read | `closed` | The owner |
| `snapshot` | The owner folds to the cut, builds the checkpoint and encodes it with the snapshot's digest | A dropped call answers nothing; nothing is written | `closed` | The owner |
| `streamCheckpoint` | The owner finds the cut's position, resumes from a valid snapshot and folds to the cutoff | A dropped call ends the fold | `closed` | The owner |
| `snapshotFromCheckpoint` | The owner encodes the given checkpoint at the anchor | A dropped call ends the read | `closed` | The owner |

## Exceptions with reasons

- `renderedStream` and `renderedContext` take a cutoff, not a branch id: the owner is a branch part and folds its own branch's facts; a Session-scoped fold by branch id returned another owner's history and is removed.
- The projection policy is factory configuration rather than a Session value: a table of Rust functions cannot cross languages, and a host that substitutes the projection installs another part, so no Rust-only value sits on the public face.
- `snapshotFromCheckpoint` answers none instead of failing when the anchor does not match: a snapshot is optional acceleration, and the caller's next step is the same fold the snapshot would have skipped.
- `streamCheckpoint` takes `batch`: a streamed fold's page size is the caller's memory decision, and the checkpoint must be built from the same fold the caller will use.

## Verification

- A fold at a cutoff returns the events that place context and the compaction in force, with positions that compare across the branch chain.
- `renderedStream` releases each page and its reply carries no event list: a fold larger than one page keeps only the current projection. `renderedContext` materializes the events and is the inspection read.
- `snapshot` encodes the checkpoint of a completed compaction; a snapshot of another interpreter or a mismatched anchor is `invalid` or answers none.
- `context_in` no longer exists; every read is the caller's own branch.
- A call in flight when the branch closes ends `closed`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part. It folds the branch's facts but writes none, so it is not a Capability; the projection policy is its factory configuration. |
| §2 scopes | Branch. The fold reads its own branch's Timeline and nothing across scopes. |
| §3 orthogonality | No other owner's state is read; the projection is a pure function of the branch's public facts. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handover of a business object; the streamed projection is a value. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The projection policy is configuration, not a core special case. |
| §4 rule 5 | The fold reads committed facts; publishing facts is not affected. |
| §4 rule 6 | The projection renders what the branch's adopted inputs and tool results placed; it adds no context path of its own. |
| §4 rule 7 | Callers use these operations; no caller folds from `timeline.query` itself. |
| §4 rule 8 | No infrastructure change: the branch's Timeline already serves the reads. |
| §4 rule 9 | No Timeline fact; a snapshot is optional derived state. |
| §7 values, ids and objects | Every reply is a value; the caller's `&Timeline` and `&Session` no longer cross. |
| §7 no proxy lookup | The owner folds its own branch; the Session-level `context_in` is removed. |
| §7 one interface | One name, `contextProjection`, on the owner type; `historyRenderer` leaves the catalog. |
| §7 cross-language | Requests and replies are data. |
| ADR 002 D1 | Branch: the projection is derived from the branch's facts and shares their lifetime. |
| ADR 002 D2 | No wiring plugin: the part is the owner; the policy is its configuration. |
| ADR 002 D3 | Depends on the branch's Timeline, at its own scope. |
| ADR 002 D5 | Data contract; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None: the owner needs only the branch's `timeline` operations.

## Supersedes

This contract replaces the `historyRenderer` value; the delivery in #2025 carries the migration.

- `docs/design/contracts/runtime-owner-operations.md`: the `Loop and ContextPreparation → HistoryRenderer` row of "Model preparation, history and tools".
- `docs/design/service-faces.md`: §3.5's `historyRenderer` row.
- `docs/design/plugins/ContextCompaction.md` §4.2 and the sections that name `HistoryRenderer`: the projection is this owner's, and the history renderer's name leaves the catalog.

Rule D: no client wire protocol or user-visible behavior changes; `StreamedContext` keeps its fields and the model-visible text is unchanged.

## Binding appendix

```rust
use dim_agent_capabilities::history::HistoryEntry;
use dim_agent_runtime::{ContextHistory, PublicService};
use dim_agent_types::{AgentEvent, ProjectionCheckpoint, RecoverySnapshot, TimelinePosition};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderedStreamRequest {
    pub through: u64,
    pub batch: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamedContext {
    pub history: Vec<HistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<dim_agent_types::ContextConfiguredData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<dim_agent_types::PromptReference>,
    /// Root first: how positions compare across the branch chain.
    pub ancestry: Vec<String>,
    /// The completed compaction in force, with its cut and baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<dim_agent_types::AgentEvent>,
    /// Root first: the segments the fold read.
    pub branches: Vec<String>,
    /// The position of the cut in effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_cut: Option<dim_agent_types::TimelinePosition>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderedContextRequest {
    pub through: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedContextReply {
    pub context: ContextHistory,
    pub history: Vec<HistoryEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    pub context: ContextHistory,
    pub retain_from: String,
    pub anchor: AgentEvent,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotReply {
    pub snapshot: RecoverySnapshot,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamCheckpointRequest {
    pub through: u64,
    pub retain_from: String,
    pub batch: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointReply {
    pub checkpoint: ProjectionCheckpoint,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotFromCheckpointRequest {
    pub checkpoint: ProjectionCheckpoint,
    pub anchor: AgentEvent,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ProjectionError {
    #[error("{message}")]
    FutureSequence { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    Storage { message: String },
    #[error("{message}")]
    Closed { message: String },
}

impl PublicService for ContextProjection {}

#[rutis::service(name = "contextProjection")]
impl ContextProjection {
    pub async fn rendered_stream(
        &self,
        request: RenderedStreamRequest,
    ) -> Result<StreamedContext, ProjectionError> { /* ... */ }
    pub async fn rendered_context(
        &self,
        request: RenderedContextRequest,
    ) -> Result<RenderedContextReply, ProjectionError> { /* ... */ }
    pub async fn snapshot(&self, request: SnapshotRequest) -> Result<SnapshotReply, ProjectionError> { /* ... */ }
    pub async fn stream_checkpoint(
        &self,
        request: StreamCheckpointRequest,
    ) -> Result<CheckpointReply, ProjectionError> { /* ... */ }
    pub async fn snapshot_from_checkpoint(
        &self,
        request: SnapshotFromCheckpointRequest,
    ) -> Result<Option<SnapshotReply>, ProjectionError> { /* ... */ }
}
```

`ContextHistory`, `HistoryEntry`, `ProjectionCheckpoint`, `RecoverySnapshot`, `AgentEvent` and `TimelinePosition` are the types those facts already use and gain `Serialize`/`Deserialize` for this face; their fields do not change. `renderedStream` carries no `ContextHistory`, so a streamed fold never holds the event list; `renderedContext` and the snapshot reads carry events or checkpoints because their callers need them. Errors cross as `{code, message}`, `code` from the variant name.
