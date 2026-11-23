# FileReads

Scope: branch. Issue: #2028.

FileReads owns one line's text bindings: which files the model has seen on this line, the raw byte revision and the text format each was read in, and whether the operation that established a binding is still in the model's context. This document is the owner's target contract; the current implementation is described in the [WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md#filereads).

## Use and scope: responsibility split

FileReads owns:

- **The bindings.** One binding per canonical path: the SHA-256 of the raw bytes, the resolved encoding, its source, the byte-order mark and the line endings, with the sequence at which the operation that established it ran.
- **The context rule.** A binding whose establishing operation a completed compaction cut out of the model's context cannot justify replacing the file; the model reads the file again or uses `edit`.
- **The rebuild.** On load, the owner folds the line's committed file-tool results back into bindings, so a reload does not force a re-read of every file whose encoding is in the facts.
- **The edit's rule.** `edit` checks the model's `oldText` against the file itself, so its binding survives a compaction that cut the read out.

FileReads does not open, read or write files: the file tools own path resolution and the bytes. FileReads does not decide whether a call is admitted: Permission and Approval do.

Callers keep as business:

- Which file a tool touches, its encoding override, and the tool's own result text.
- What "read the file again" means for the turn.
- The write's effect declarations and the mutation display.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Decide whether a `write` may replace an existing file | No: the binding's freshness against compaction is the owner's rule, and a caller holding the map would duplicate it | `binding` with `write` |
| Decide whether an `edit` may proceed | No: the same map, with a different rule | `binding` with `edit` |
| Record that an operation bound a file | No: the record and the operation's start sequence are one write; a caller that forgot the record would silently allow a later write | `record` |

An operation exists only where its row says No.

## Caller example

The `write` tool asks before replacing an existing file, then records the operation it performed:

```rust
let binding = reads.binding(BindingRequest {
    path: display.clone(),
    purpose: Purpose::Write,
}).await?; // Closed; the value says whether a binding is usable
match binding.binding {
    Some(binding) => { /* preserve encoding and line endings; check the revision */ }
    None => {
        let reason = binding.unavailable.expect("one of binding or unavailable");
        return Ok(text_binding_required(reason)); // the model re-reads or edits
    }
}
// ... write the bytes ...
reads.record(RecordRequest {
    path: canonical.clone(),
    binding: new_binding,
    established,
}).await?; // Closed
```

The `edit` tool asks with the edit purpose, which ignores compaction:

```rust
let binding = reads.binding(BindingRequest { path: display, purpose: Purpose::Edit }).await?;
let Some(binding) = binding.binding else {
    return Ok(text_binding_required(binding.unavailable.expect("read first")));
};
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Timeline` | The line's committed file-tool results, from which bindings are rebuilt, and the completed `context/compaction` facts that decide the context rule. |

FileReads injects no other service. The Session's file tools reach this owner through the Session access mechanism by line id ([TimelineTree's route](TimelineTree.md#session-parts-reaching-a-lines-owners)); FileReads returns values and stores records, and no file handle crosses.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `binding` | Whether a file may be replaced or edited, and on which binding | `{path, purpose}` | `{binding?}` or `{unavailable: never_read\|compacted}` | None; the owner completes the read | `closed` |
| `record` | Note the binding an operation established, at the sequence the operation started | `{path, binding, established}` | `{}` | None | `closed` |

`purpose` is `write` or `edit`; it selects the owner's rule, and a caller never evaluates compaction itself. `binding` is the value the file tool preserves: revision, encoding, encoding source, byte-order mark and line endings. `established` is the line's sequence when the operation started, taken by the caller after the call's fact committed; the owner compares it with the completed compaction's cut.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `closed` | `binding`, `record` | The line is closing or unloading; the tool call ends; the replacement generation rebuilds the bindings from the facts. |

"Never read" and "compacted" are values, not errors: the file tool turns either into the model-facing `text_binding_required` result, and the model's next step is to re-read or edit.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `binding` | Read the map; for `write`, check the latest completed compaction after the binding's sequence and whether the cut event still lies after it | None; the read is bounded | Nothing to release | Refused with `closed` | FileReads |
| `record` | Replace the path's binding with the new one and its sequence | None | Nothing to release | The new generation rebuilds from the facts; a record not yet committed is lost with the generation | FileReads |
| Load | Read the line's committed file-tool results at one cutoff, take each result's binding and its event sequence, and keep the latest per path | — | — | — | FileReads |

The rebuild uses the result event's sequence: no legal compaction cut separates a call from its result, so the result's position is equivalent to the operation's start for the context rule.

## Exceptions with reasons

- `record` takes the operation's start sequence from the caller: the window belongs to the caller's operation, and an owner that sampled the sequence at record time would sample after the operation's own writes.

## Verification

Flow tests on the owner:

- A read binds a file; a `write` after a compaction that cut the read out is refused with `compacted`; a compaction that kept the read leaves the write admitted; an `edit` proceeds after either.
- A write's binding replaces the read's for later operations; a failed offload does not bind.
- After a reload, the bindings are rebuilt from the line's facts and an existing file can be written without a new read; a file whose read a compaction cut out before the reload stays unavailable for `write`.
- The face and the Rust call answer the same binding or reason.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | FileReads is one branch part and a Capability: it injects `timeline` and folds the line's file-tool results on load. |
| §2 scopes | Branch: bindings are one line's; the file tools are Session parts that reach this owner by line id. |
| §3 orthogonality | No other owner's state is read; the facts are the line's Timeline. |
| §4 rule 1 | No command event: the owner writes nothing. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | No batch. |
| §4 rule 6 | No model-context input: the bindings are not context. |
| §4 rule 7 | The tool results are public facts; the owner folds them. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact of its own; the bindings are a projection of public facts, and the projection is this owner's state. |
| §7 resources and objects | No resource; the owner stores values and returns values. |
| §7 no proxy | The owner answers its own bindings; it returns no file's bytes. |
| §7 one surface | `binding` and `record`; `get` and `write_binding` are removed. |
| §7 cross-language | Every parameter and reply is data; the caller is a Session part reached by line id. |
| §7 responsibility split | The caller keeps the file, its bytes and the result text; the owner keeps the bindings and the context rule. |
| ADR 002 D1 | Branch scope: a binding is one line's having seen a file. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | No dependency on callers; a composition without the file tools has no bindings. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **Session access (#2025).** A Session part resolves a line's owner by line id: the mechanism named in [TimelineTree's route](TimelineTree.md#session-parts-reaching-a-lines-owners). The file tools use it to reach `binding` and `record`; FileReads adds no forwarding operation.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `fileReads`.

```rust
use dim_agent_runtime::PublicService;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Write,
    Edit,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingRequest {
    pub path: String,
    pub purpose: Purpose,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFileBinding {
    pub revision: String, // SHA-256 of the raw bytes, lowercase hex
    pub encoding: TextEncoding,
    pub encoding_source: EncodingSource,
    pub bom: Bom,
    pub eol: Eol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unavailable {
    NeverRead,
    Compacted,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<TextFileBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Unavailable>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRequest {
    pub path: String,
    pub binding: TextFileBinding,
    pub established: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum FileReadsError {
    #[error("the line is closing")]
    Closed,
}

impl PublicService for FileReads {}

#[rutis::service(name = "fileReads")]
impl FileReads {
    pub async fn binding(&self, request: BindingRequest) -> Result<BindingReply, FileReadsError> { /* ... */ }
    pub fn record(&self, request: RecordRequest) -> Result<(), FileReadsError> { /* ... */ }
}
```

`TextEncoding`, `EncodingSource`, `Bom` and `Eol` are the existing value types of the file-tool codec; they cross as data and stay defined beside the codec, not here.
