# Context projection

Scope: branch. Issue: #2025.

The owner folds its own branch's facts into the context a model request carries, renders that context to messages, and encodes or decodes the recovery snapshot that lets a later fold start after a compaction. Today the same need is the `historyRenderer` value: a table of Rust functions at Session scope, whose methods take a caller-supplied `&Timeline` or `&Session`, plus a Session-level `context_in` that folds another branch's history by id. This document replaces the value with branch-scoped operations.

## Use and scope: responsibility split

The owner answers:

- **The context at a cutoff.** Which events place context, how a compaction's replacement and baseline apply, and the positions the fold covered.
- **Rendered entries.** The context and its rendered `Entry` list at one cutoff, for callers that need both.
- **A request-local projection.** The rendered entries and the compaction in force at a cutoff, folded page by page; the fold releases each raw event page and the reply carries no event list, so the events are not held whole.
- **Recovery snapshots.** Encode the projection at a completed compaction's cut, select the saved snapshot a fold resumes from, validate a saved snapshot against its anchor, and decode the checkpoint it carries.

Callers keep:

- Which cutoff a request uses: the `through` its source carries.
- Whether a prepared request's estimate may be reused: ContextPreparation's `validate`.
- When a snapshot is saved: ContextPreparation's compaction flow.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| ContextPreparation builds a request's context at a fixed cutoff | No: which events carry context, how compaction replaces a range and how the baseline folds are the owner's knowledge; composing them from `timeline.query` puts a second projection in the caller | `renderedStream` |
| ContextPreparation's manual compaction reads the context and its rendered entries at a cutoff | No: the same fold; a caller that renders entries itself keeps a second projection | `renderedContext` |
| ContextPreparation checks the budget at its own cutoff and compacts | Yes: the entries, the configuration and the compaction in force the fold returns are the check's input | Business composition |
| ContextPreparation saves a completed compaction's snapshot after the fact commits | No: encoding, the anchor check and decoding are one rule, and the caller holds either the full context or only a checkpoint | `snapshot`, `streamCheckpoint`, `snapshotFromCheckpoint` |
| ContextPreparation resumes from a saved snapshot | No: the owner selects the snapshot its folds resume from and ignores one that does not apply | `renderedStream`, `renderedContext`, `streamCheckpoint` |
| A test or an example inspects a folded context | Yes: `renderedContext` answers the context and the entries | Business |
| A process-scope reader folds a named Session's branch | No: the fold is branch state; the reader resolves the branch by id through the mechanism and calls `renderedContext` there ([ADR 027](../../adr/027-cross-scope-access-by-id.md), #2127) | `renderedContext` on the branch the id resolves |

## Caller example

```rust
// ContextPreparation: the context the request and its budget check read, at the source's cutoff.
let streamed = match projection
    .rendered_stream(RenderedStreamRequest { through: source.through, batch: 256 })
    .await
{
    Ok(streamed) => streamed,
    Err(ProjectionError::Closed { .. }) => return Ok(()), // the branch is closing
    Err(error) => return Err(error),
};
let messages: Vec<std::sync::Arc<Message>> = {
    // The compaction in force comes first: its replacement, then the keyed baseline.
    let mut messages: Vec<std::sync::Arc<Message>> = Vec::new();
    if let Some(dim_agent_types::AgentEvent {
        kind: EventKind::ContextCompaction(data),
        ..
    }) = &streamed.compaction
    {
        if let ContextCompactionResult::Completed {
            replacement,
            context_baseline,
            ..
        } = &data.outcome
        {
            messages.extend(replacement.iter().cloned().map(std::sync::Arc::new));
            messages.extend(context_baseline.iter().map(|entry| entry.message.clone()));
        }
    }
    messages.extend(
        streamed
            .history
            .iter()
            .map(|entry| std::sync::Arc::new(entry.message.clone())),
    );
    messages
};
```

```rust
// ContextPreparation: the context and the rendered entries the manual compaction starts from.
let RenderedContextReply { context, history } = match projection
    .rendered_context(RenderedContextRequest { through })
    .await
{
    Ok(reply) => reply,
    // `future_sequence`, `storage`, `fault` or `closed`: the compaction returns the error, and
    // the command behind it answers the client with that failure instead of waiting for a fact.
    Err(error) => return Err(error),
};
```

```rust
// ContextPreparation: the snapshot of the completed compaction. Saving it is optional:
// any failure, including the save, skips the snapshot and the committed fact stands.
let saved = match projection
    .snapshot(SnapshotRequest { context, retain_from: cut, anchor: fact.clone() })
    .await
{
    Ok(SnapshotReply { snapshot }) => Some(snapshot),
    // `invalid`, `fault` or `closed`: every failure skips the snapshot.
    Err(_) => None,
};
if let Some(snapshot) = saved {
    // The branch Timeline's save waits for the anchor to publish; a storage or closed
    // failure leaves the snapshot unsaved.
    let _ = timeline.save_snapshot(snapshot).await;
}
```

```rust
// ContextPreparation: the same snapshot when the caller holds only a checkpoint. Saving it is
// optional: a fold, encode or save failure skips the snapshot and the committed fact stands.
let saved = match projection
    .stream_checkpoint(StreamCheckpointRequest { through, retain_from: cut, batch: 256 })
    .await
{
    Ok(CheckpointReply { checkpoint }) => match projection
        .snapshot_from_checkpoint(SnapshotFromCheckpointRequest { checkpoint, anchor: fact })
        .await
    {
        Ok(Some(SnapshotReply { snapshot })) => Some(snapshot),
        Ok(None) | Err(_) => None,
    },
    Err(
        ProjectionError::FutureSequence { .. }
        | ProjectionError::Invalid { .. }
        | ProjectionError::Storage { .. }
        | ProjectionError::Fault { .. }
        | ProjectionError::Closed { .. },
    ) => None,
};
if let Some(snapshot) = saved {
    let _ = timeline.save_snapshot(snapshot).await;
}
```

## Own contract

### Dependencies

`B:Timeline` — the owner reads the branch through `timeline.query` over the published facts at or below the cutoff, `timeline.sequence` for the published head, `timeline.checkpoint(through, interpreter, decode)` with this owner's interpreter and decode callback, and `timeline.segments` with `timeline.ancestors` for the chain the fold walks. The caller, not the owner, saves a snapshot through `timeline.save_snapshot`. The owner never asks another branch or the Session for events or snapshots. The projection policy is factory configuration: the assembly installs the owner with the policy in force, a host that needs another policy installs another part, and a test that needs one builds the owner through the same factory. The policy is not a public parameter of any operation.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `renderedStream` | The rendered entries and the compaction in force at a cutoff, folded page by page from the snapshot the owner selects, without retaining events | `{through, batch}` | `{history, configuration?, prompt?, ancestry, compaction?, branches, compactionCut?}` | The caller drops the call; the fold stops at its last page | `future_sequence`, `invalid`, `storage`, `fault`, `closed` |
| `renderedContext` | The context and rendered entries at a cutoff, with the events the fold read from the checkpoint resume to the cutoff | `{through}` | `{context, history}` | The caller drops the call | `future_sequence`, `storage`, `fault`, `closed` |
| `snapshot` | Encode the projection at a completed compaction's cut | `{context, retainFrom, anchor}` | `{snapshot}` | No cancellation check; a dropped call discards the reply and writes nothing | `invalid`, `fault`, `closed` |
| `streamCheckpoint` | The checkpoint of the fold that starts at the saved snapshot the owner selects, or at the chain start, and stops at a compaction's cut | `{through, retainFrom, batch}` | `{checkpoint}` | The caller drops the call | `future_sequence`, `invalid`, `storage`, `fault`, `closed` |
| `snapshotFromCheckpoint` | A snapshot that carries an already-computed checkpoint, anchored at the compaction that produced it | `{checkpoint, anchor}` | `{snapshot}` or none when the anchor is not that compaction | No cancellation check; a dropped call discards the reply and writes nothing | `closed` |

`through` is a sequence at or below the branch's published head; the fold reads the published facts up to it, and a higher cutoff is `future_sequence`. `batch` is the page size of the fold; `retainFrom` is the id of the compaction's cut event; `anchor` is the completed compaction fact a snapshot anchors to.

Every fold reads this branch's own facts, keyed by a cutoff and never by a branch id, and every fold selects a saved snapshot by one rule: this owner decodes it against the anchor it carries, and a snapshot whose resume event cannot be read is ignored like one that does not decode. The owner has one interpreter identity: both encode operations write it into the snapshot, and a saved snapshot carrying another interpreter is ignored. `streamCheckpoint` adds one condition — the decoded checkpoint must resume before the cut — and otherwise starts at the chain start. A saved snapshot that fails any of these rules is ignored, not an error: it is optional acceleration. A snapshot that decodes but whose state the current policy cannot fold — another policy under the same interpreter, or a corrupted state — is a cache miss of the same kind: every fold ignores it and replays from the chain start. `fault` is only for the branch's committed facts that do not project. `streamCheckpoint` takes `batch` because a streamed fold's page size is the caller's memory decision and the checkpoint must come from the same fold.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `future_sequence` | `renderedStream`, `renderedContext`, `streamCheckpoint` | Fix the cutoff: it is above the branch's published head. A compaction saving its snapshot skips it and keeps the committed fact |
| `invalid` | `renderedStream`, `streamCheckpoint`, `snapshot` | Fix the request: a batch size of zero; for `streamCheckpoint`, a `retainFrom` outside the branch chain at or before `through`; for `snapshot`, a cut that is neither in the context's events nor the `retain_from` its checkpoint state records, an anchor that is not a completed compaction, or a `retainFrom` that is not that compaction's cut. A compaction saving its snapshot skips it and keeps the committed fact |
| `storage` | `renderedStream`, `renderedContext`, `streamCheckpoint` | A preparation request fails for this attempt and a retry re-reads. A compaction saving its snapshot skips it and keeps the committed fact |
| `fault` | `renderedStream`, `renderedContext`, `streamCheckpoint`, `snapshot` | The branch's committed facts do not project under the current policy. A preparation request reports it and stops: retrying the same cutoff does not help. A compaction saving its snapshot skips it and keeps the committed fact. A saved snapshot that cannot fold is not this kind: it is ignored, and the fold replays from the chain start |
| `closed` | all five | Stop preparing requests. A compaction step that has not written its fact ends, and one saving its snapshot skips it: `snapshot`, `streamCheckpoint` and `snapshotFromCheckpoint` write nothing, and a committed compaction fact stands |

`renderedContext` is the manual compaction's own pre-read. A failure ends the compaction with that error: the command behind it answers the client with the failure instead of waiting for a compaction fact, no snapshot is saved and no completed fact is written.

A saved snapshot is anchored at the compaction in force when it was written, so its anchor is checked against the checkpoint it carries when it is decoded, never against the new cut. `snapshotFromCheckpoint` takes a checkpoint this owner built through `streamCheckpoint` for that compaction. `ProjectionCheckpoint` records the resume position — branch id, sequence and event id — and the state, but no owner, interpreter or policy identity. A checkpoint this owner built has its `resume.branch_id` on that compaction's fixed ancestor chain; that is a precondition of the input, not something this operation checks. The only check the shape supports is the anchor's completed `retain_from`, and a mismatch answers none for every anchor that is not the checkpoint's own completed compaction, including an anchor that is not a compaction at all. Encoding such a checkpoint cannot fail — its state is this owner's own JSON with a digest — so the operation answers `closed` only. On none the caller skips the snapshot and the committed compaction fact stands.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `renderedStream` | The owner selects the saved snapshot its checkpoint validates, folds page by page from its resume position, releases each page, and answers the streamed projection | A dropped call stops the fold at its last page; no state is written | An in-flight fold ends `closed`; the generation answers no further call | The calling row drops the call; the owner holds no resource between pages |
| `renderedContext` | The owner folds the same content without the request-local form | A dropped call ends the read | `closed` | The owner |
| `snapshot` | The owner checks that the anchor's completed compaction names `retainFrom`, builds the checkpoint from the given context at that cut and encodes it with the snapshot's digest; another `retainFrom` is `invalid` | A dropped call discards the reply; nothing is written | `closed`; the snapshot is skipped and the committed fact stands | The owner encodes the snapshot; the caller saves it through the branch Timeline's `save_snapshot`, which writes only the cache after the anchor publishes ([storage](../../architecture/rust-agent/timeline-storage.md)); a `storage` or `closed` failure there leaves it unsaved and the committed fact in force |
| `streamCheckpoint` | The owner finds the cut's position, selects the saved snapshot whose checkpoint resumes before the cut, starts the fold at its resume (or at the chain start) and folds to the cut | A dropped call ends the fold | `closed`; the snapshot is skipped and the committed fact stands | The owner |
| `snapshotFromCheckpoint` | The owner checks the checkpoint's `retain_from` against the anchor's completed compaction and encodes the checkpoint at the anchor; another anchor, including one that is not a compaction, answers none | A dropped call discards the reply; nothing is written | `closed`; the snapshot is skipped and the committed fact stands | The owner encodes the snapshot; the caller saves it through the branch Timeline's `save_snapshot`; a failed save leaves it unsaved and the committed fact in force |

## Verification

- A fold at a cutoff returns the events that place context and the compaction in force, with positions that compare across the branch chain.
- The request a streamed projection builds carries the compaction in force first — its replacement, then the keyed baseline — and then the rendered entries after its cut.
- `renderedStream` releases each raw event page and its reply carries no event list: a fold larger than one page keeps only the current projection and its rendered entries. `renderedContext` materializes the events and is the inspection read.
- `streamCheckpoint` resumes the fold from a saved snapshot whose checkpoint starts before the cut, and starts at the chain start when the snapshot cannot be decoded or its checkpoint does not resume before the cut. A snapshot anchored at an earlier compaction is used: its anchor is not compared with the new cut.
- `snapshot` encodes the checkpoint of a completed compaction and answers `invalid` for a cut that is neither in the context's events nor the `retain_from` its checkpoint state records, an anchor that is not a completed compaction, or a `retainFrom` that is not that compaction's cut; it never answers none. `snapshotFromCheckpoint` answers none for every other anchor, including one that is not a compaction.
- A committed compaction or baseline the policy cannot render answers `fault`; the same cutoff does not succeed on a retry.
- A cutoff above the branch's published head — including a sequence already assigned but not yet published — answers `future_sequence`.
- A saved snapshot whose resume event cannot be read is ignored by every fold: it starts at the chain start, and `streamCheckpoint` still folds to the cut.
- Both encode operations write the owner's interpreter identity into the snapshot; a saved snapshot written under another interpreter is ignored by every fold.
- A saved snapshot whose state the current policy cannot fold is ignored like an undecodable one: every fold replays from the chain start and the request or compaction proceeds.
- Every fold is performed by the branch's owner; no Session-level `context_in` remains.
- A call in flight when the branch closes ends `closed`.

## Supersedes

This contract replaces the `historyRenderer` value; the delivery in #2025 carries the migration.

- `docs/design/contracts/runtime-owner-operations.md`: the `Loop and ContextPreparation → HistoryRenderer` row of "Model preparation, history and tools".
- `docs/design/contracts/runtime-owner-operations.md`: the `projection?` member of the `Loop and ModelProvider → ContextPreparation` row, which goes with the handover this contract removes; that row's remaining surface is ContextPreparation's own (#2027).
- `docs/design/service-faces.md`: §3.5's `historyRenderer` row.

No client wire protocol or user-visible behavior changes; the streamed reply keeps the fields the callers use, and the model-visible text is unchanged.

## Exceptions with reasons

None: every reply is a value, and the owner hands out no object, callback or resource handle.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part. It is not a Capability: it holds no folded state between calls, and it injects `timeline` only to read the branch's facts, so every call rebuilds the projection from them. The projection policy is its factory configuration. |
| §2 scopes | Branch. Every fold reads its own branch's chain through its own branch Timeline, and nothing across scopes. |
| §3 orthogonality | No other owner's state is read; the projection is a pure function of the branch's public facts. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handover of a business object; the streamed projection is a value. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The projection policy is configuration, not a core special case. |
| §4 rule 5 | The fold reads the published facts at or below the cutoff; publishing facts is not affected. |
| §4 rule 6 | The projection renders what the branch's adopted inputs and tool results placed; it adds no context path of its own. |
| §4 rule 7 | Callers use these operations; no caller folds from `timeline.query` itself. |
| §4 rule 8 | One mechanism requirement: the [ADR 027](../../adr/027-cross-scope-access-by-id.md) D1/D3 by-id route (#2127), so a process-scope reader reaches this branch owner. The branch's Timeline itself needs no change. |
| §4 rule 9 | No Timeline fact; a snapshot is optional derived state. |
| §7.1 design questions | Resources: none; the owner reads the branch's facts page by page and holds nothing between them. Lifecycle: the part lives with the branch, a dropped call ends the fold, and a fold in flight when the branch closes ends `closed`. Operations: one per caller intent, each with its cancellation and errors above. |
| §7 values, ids and objects | Every reply is a value; the caller's `&Timeline` and `&Session` no longer cross. |
| §7 no proxy lookup | The owner folds its own branch; the Session-level `context_in` is removed. |
| §7 one interface | One name, `contextProjection`, on the owner type; `historyRenderer` leaves the catalog. |
| §7 cross-language | Requests and replies are data. |
| §7.2 values and no proxy | Every cross-owner call is one operation of its owner: the `timeline` reads (`query`, `sequence`, `checkpoint`, `segments`, `ancestors`) and the caller's `timeline.save_snapshot`. The owner hands out values only, and nothing asks it for another owner's data — `context_in` is gone. A process-scope reader reaches a named branch by id through the mechanism ([ADR 027](../../adr/027-cross-scope-access-by-id.md)) and calls this owner there; the owner is registered in its branch's group. |
| §7.2 interface, terms and settlement | One interface per owner; requests and replies are data; no caller business concept or owner-internal mechanism sits on the face. The flows state who settles a fold, a dropped call and a close. |
| §7 one change | The implementation removes `historyRenderer` and migrates its callers in one change. |
| ADR 002 D1 | Branch: the projection is derived from the branch's facts and shares their lifetime. |
| ADR 002 D2 | No wiring plugin: the part is the owner; the policy is its configuration. |
| ADR 002 D3 | Depends on the branch's Timeline, at its own scope. |
| ADR 002 D4 | The projection policy is structural configuration: a host that substitutes the projection installs another part, and no business setting selects it. |
| ADR 002 D5 | Data contract; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

One: the by-id route of [ADR 027](../../adr/027-cross-scope-access-by-id.md) D1 and D3 (#2127), so a process-scope reader can fold a named Session's branch.

- **Blocked by:** the `Session` and `Branch` facades reach only the caller's own scope, and a process-scope caller needs both steps of ADR 027 D1 — `sessionId` to the Session group, then `(sessionId, branchId)` to the branch group. Folding another branch by id is the Session-level `context_in` this contract removes ([§7 no proxy lookup](#rule-check)), so it is no fallback.
- **Considered and rejected:** a Session-scoped fold by branch id (a proxy read of another branch); a forwarding operation on another owner carrying the branch id (§7).
- **General capability needed:** resolve `sessionId` to its Session group and then `(sessionId, branchId)` to the branch group, and call the branch owner's own operations there. It carries no business meaning, and this owner gains no forwarding operation, operation-name routing or request/reply envelope. This owner registers in its branch's group, so the route finds it.
- **Sequence, batches, interruption, reload:** the route adds no event, batch or interruption semantics. A reload drops the resolved instance and the reader resolves again; a closed line answers as ADR 027 says.
- **Owner and issue:** the runtime mechanism, #2127.

## Binding appendix

`ContextProjection` is the owner type; its fields are private implementation.

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::{
    AgentEvent, ContextBaselineEntry, Message, ProjectionCheckpoint, RecoverySnapshot,
    TimelinePosition,
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderedStreamRequest {
    pub through: u64,
    pub batch: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamedContext {
    pub history: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<dim_agent_types::ContextConfiguredData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<dim_agent_types::PromptReference>,
    /// Root first: how positions compare across the branch chain.
    pub ancestry: Vec<String>,
    /// The completed compaction in force, with its cut and baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<dim_agent_types::AgentEvent>,
    /// Root first: the branch chain up to the cutoff.
    pub branches: Vec<String>,
    /// The position of the cut in effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_cut: Option<dim_agent_types::TimelinePosition>,
}

/// The wire form of one rendered entry. The owner maps `HistoryEntry` to it:
/// the entry's `Arc<Message>` becomes an owned `Message`, and its
/// `Arc<[ContextBaselineEntry]>` becomes a `Vec` of the same entries, each of
/// which keeps its own `Arc<Message>`. The internal type is unchanged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub message: Message,
    pub tool_context: bool,
    pub baseline: Vec<ContextBaselineEntry>,
    pub event_id: String,
    pub position: TimelinePosition,
}

/// The wire form of a folded context. The owner maps `ContextHistory` to it
/// (`Arc<AgentEvent>` becomes an owned event); the internal type is unchanged.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<ProjectionCheckpoint>,
    pub ancestry: Vec<String>,
    pub events: Vec<AgentEvent>,
    pub positions: Vec<TimelinePosition>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderedContextRequest {
    pub through: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedContextReply {
    pub context: History,
    pub history: Vec<Entry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    pub context: History,
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
    Fault { message: String },
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

`Entry` and `History` are this face's wire forms of `dim_agent_capabilities::history::HistoryEntry` and `dim_agent_runtime::ContextHistory`; `Entry` maps the entry's `Arc<Message>` to an owned `Message` and its `Arc<[ContextBaselineEntry]>` to a `Vec` of the same entries, each keeping its own `Arc<Message>`. `StreamedContext` maps the rendered entries the same way and carries its `compaction` as an owned `AgentEvent` instead of `Arc<AgentEvent>`. The owner maps them in one place, and the internal types stay unchanged. The face's request, reply and error types live in `crate::context_projection`. `ProjectionCheckpoint`, `RecoverySnapshot`, `AgentEvent`, `TimelinePosition`, `Message` and `ContextBaselineEntry` already derive serde, so `Entry` and `History` need no code change elsewhere. `renderedStream` carries no folded event list; `renderedContext` and the snapshot reads carry the events because their callers need them. Errors cross as `{code, message}`, `code` from the variant name.
