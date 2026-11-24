# FileReads

Scope: branch. Issue: #2028.

FileReads owns one line's text bindings: which files this line's own operations have seen, the raw byte revision and the text format each was read in, and whether the operation that established a binding is still in the model's context. A line forked from another starts empty: the inherited history belongs to the parent line, and a parent's read before the fork is not a binding here. This document is the owner's target contract; the current implementation is described in the [WorkspaceTools plugin page](../../architecture/rust-agent/plugins/WorkspaceTools.md#filereads).

## Use and scope: responsibility split

FileReads owns:

- **The bindings.** One binding per canonical path, from the operations run on this line itself: the SHA-256 of the raw bytes, the resolved encoding, its source, the byte-order mark and the line endings, with the call event's sequence and the call's ordinal. `read`, `write` and `edit` hold the per-path lock from before their checks or reads until after they record, so same-path work is serialized, and the owner keeps the binding with the greater (`established`, `ordinal`): the order is the order of the calls' events, not of their execution. A top-level call that changes the file and runs after a Code Mode child of its batch is outranked by that child's binding, and the next `write` or `edit` answers `file_modified_since_read`, so the model re-reads.
- **The context rule.** A binding whose establishing operation a completed compaction cut out of the model's context cannot justify replacing the file; the model reads the file again or uses `edit`.
- **The rebuild.** On load, the owner folds this line's own committed file-tool results back into bindings: each result carries the binding its operation installed, so Load takes it as it stands and infers nothing from another operation's read. A forked line rebuilds nothing from its parent.
- **The edit's rule.** `edit` checks the model's `oldText` against the file itself, so its binding survives a compaction that cut the read out.

FileReads does not open, read or write files: the file tools own path resolution and the bytes. FileReads does not decide whether a call is admitted: Permission and Approval do.

Targets: the line access is [ADR 027](../../adr/027-cross-scope-access-by-id.md) (proposed) with the mechanism obligation [#2127](https://github.com/arcships/dim/issues/2127); `fileReads` becomes a data face so a line is reachable by id; the file-tool codec's mapping to the DTOs below is a requirement on WorkspaceTools ([#2026](https://github.com/arcships/dim/issues/2026)).

Callers keep as business:

- Which file a tool touches, its encoding override, and the tool's own result text.
- What "read the file again" means for the turn.
- The write's effect declarations and the mutation display.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Decide whether a `write` may replace an existing file | No: the binding's freshness against compaction is the owner's rule, and a caller holding the map would duplicate it | `binding` with `write` |
| Create a file that does not exist | Yes: there is no binding to consult; the tool's own resolution, its existence check under the per-path lock and the default binding cover it | — |
| Decide whether an `edit` may proceed | No: the same map, with a different rule | `binding` with `edit` |
| Record that an operation bound a file | No: the record and the operation's start sequence are one write; a caller that forgot the record would leave later operations without a file revision they can trust | `record` |

An operation exists only where its row says No.

## Caller example

The file tool resolves the path, takes the per-path lock, runs its own checks, then asks and records with that canonical path. The lock is held from before the checks and the binding or the read until after `record`, and `binding` is consulted only after the tool's own checks pass, so `file_not_found` for `edit`, `is_directory`, `not_regular`, the edit size limit and their published refusals keep their order. A missing path takes no `binding`: the tool writes the file as UTF-8 without a BOM, detects the line endings it wrote and records that binding, so a later `write` can replace the file. The examples take `context` (the call's `ToolExecutionContext`), `new_binding` and `result` as the tool's own values. A call dispatched by ToolExecutor carries both fields; a direct `toolHost.execute` caller may omit them, and then the operation records nothing: the bytes stand and the next write needs a read, which the examples show.

```rust
let pair = context.sequence.zip(context.ordinal); // a direct caller may omit the pair
let canonical = resolve(request.path)?; // the tool's own resolution
let _lock = locks.lock(&canonical, &cancel).await?; // held from before the checks until after `record`
if let Some(refusal) = target_checks(&canonical) { // directory, non-regular file, stat error
    return Ok(refusal);
}
if exists(&canonical) { // the tool's own check
    let binding = reads.binding(BindingRequest {
        path: canonical.clone(),
        purpose: Purpose::Write,
    }).await?; // Closed, Failed
    match binding {
        BindingReply::Bound { binding } => { /* preserve encoding and line endings; check the revision */ }
        BindingReply::Unavailable { reason } => {
            return Ok(text_binding_required(reason)); // the model re-reads or edits
        }
    }
}
// ... write the bytes; the tool's own result and the binding the write installed ...
if let Some((established, ordinal)) = pair {
    if let Err(RecordError::Closed) = reads.record(RecordRequest {
        path: canonical,
        binding: new_binding,
        established,
        ordinal,
    }) {
        // the bytes are written: the result stands, and the next operation needs a read unless the next generation rebuilds it
    }
}
return Ok(result);
```

The `edit` tool asks with the edit purpose, which ignores compaction, and records the revision its replacement produced:

```rust
let pair = context.sequence.zip(context.ordinal);
let canonical = resolve(request.path)?; // the tool's own resolution
let _lock = locks.lock(&canonical, &cancel).await?; // held from before the checks until after `record`
if let Some(refusal) = target_checks(&canonical) { // missing target, directory, non-regular file, edit size limit
    return Ok(refusal);
}
let reply = reads.binding(BindingRequest { path: canonical.clone(), purpose: Purpose::Edit }).await?;
let binding = match reply {
    BindingReply::Bound { binding } => binding,
    BindingReply::Unavailable { reason } => return Ok(text_binding_required(reason)),
};
// ... apply the replacement, keeping the binding's encoding, source and BOM; the file's line endings are detected again ...
if let Some((established, ordinal)) = pair {
    if let Err(RecordError::Closed) = reads.record(RecordRequest {
        path: canonical,
        binding: new_binding,
        established,
        ordinal,
    }) {
        // the file is edited: the result stands, and the next operation needs a read unless the next generation rebuilds it
    }
}
return Ok(result);
```

The `read` tool holds the same lock and records the binding it installed; a text read binds, an image read does not, so the record follows the offload and happens only when the read installed a binding:

```rust
let pair = context.sequence.zip(context.ordinal);
let canonical = resolve(request.path)?; // the tool's own resolution
let _lock = locks.lock(&canonical, &cancel).await?; // held from before the read until after `record`
// ... read the bytes, offload the result, and build the binding a text read installed ...
if let (Some(new_binding), Some((established, ordinal))) = (new_binding, pair) {
    if let Err(RecordError::Closed) = reads.record(RecordRequest {
        path: canonical,
        binding: new_binding,
        established,
        ordinal,
    }) {
        // the read stands, and the next operation needs a read unless the next generation rebuilds it
    }
}
return Ok(result);
```

The bytes a `write` or `edit` produced are not undone by a `record` that answers `closed`: the tool returns its own result, and the next operation on that file needs a read unless the replacement generation rebuilds the binding from the facts.

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Timeline` | The line's own committed file-tool results, from which bindings are rebuilt, and the completed `context/compaction` facts that decide the context rule; a forked line reads its own Timeline only. |
| `B:BranchLifecycle` | The line's close: a request that arrives after the close started answers `closed`. |

FileReads injects no other service, and no file handle crosses: the Session's file tools reach this owner by line id ([ADR 027](../../adr/027-cross-scope-access-by-id.md)), without a declared dependency on them.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `binding` | Whether a file may be replaced or edited, and on which binding | `{path, purpose}` | `BindingReply` | None; the owner completes the read | `closed`, and `failed` with `write` |
| `record` | Note the binding an operation established, at the sequence and ordinal that identify the operation | `{path, binding, established, ordinal}` | `()` | None | `closed` |

`path` is the canonical path the file tool resolved; the owner keys bindings by it. `purpose` is `write` or `edit`; it selects the owner's rule, and a caller never evaluates compaction itself. `binding` is the value the file tool preserves. `established` is the `step/tool-call` sequence of the call that established the binding — every call in a batch shares its call event's sequence — and `ordinal` is the call's position in its batch's `calls[]`. A Code Mode child has its own call event, so `established` differs between a parent and its child, and the child's single-call batch gives it `ordinal` 0. Both values come from the call's own tool execution context, which ToolExecutor sets for every call. The owner compares `established` with the completed compaction's cut and uses (`established`, `ordinal`) to order two operations on one path. `record` takes both from the caller because the window belongs to the caller's operation; an owner that sampled the sequence at record time would sample after the operation's own writes.

### Errors

| Kind | Operation | Retryable | What the caller does next |
| --- | --- | --- | --- |
| `closed` | `binding` | No, in this generation | The line is closing; the tool call ends and the file is unchanged. |
| `closed` | `record` | No, in this generation | The operation already ran; the tool returns its own result and the binding is not stored. A row reload keeps the branch phase, so the replacement generation rebuilds the binding from the facts; a branch close ends the line and has no replacement, so the next write on the line the model continues on needs a read; a restored branch folds its own facts like any other generation. |
| `failed` | `binding` with `write` | Yes, by a later attempt | The line's facts could not be read; nothing was decided and nothing was written; the error's `reason` carries the read failure and returns to the caller, and the tool call ends. |

`record` has no failure of its own (`RecordError` carries only `closed`): the binding is a value in the branch instance's memory, so only a close refuses it. A `binding` call with the edit purpose reads the map only, so only the write purpose can answer `failed`. The close signal is the branch lifecycle ([plugin contract §7.1](../plugin-contract.md#71-phases)): `binding` awaits `branchLifecycle.closing()` so a close that starts during its fact read ends the call, and `record`, being synchronous, reads the phase that signal sets. The owner does not poll the Timeline for it.

`neverRead` and `compacted` are values, not errors: the file tool turns either into the model-facing `text_binding_required` result, and the model's next step is to re-read or edit. `compacted` is only a `write` answer; with the edit purpose the reply is `bound` or `unavailable {neverRead}`. A caller that matches the reply by hand does not fall through: every reply is one of the two variants. A line the mechanism cannot resolve never reaches this owner; that failure and the caller's next step belong to [#2127](https://github.com/arcships/dim/issues/2127).

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `binding` | Read the map; for `write`, take the latest completed compaction after the binding's sequence: the binding is compacted when that compaction's `retain_from` event lies after the binding's sequence. A compaction inherited from the parent line, and any cut inside the inherited prefix, precedes every operation of this line, so it cannot compact this line's bindings. With the edit purpose the reply is `bound` or `unavailable {neverRead}`; `compacted` is a `write` answer. | None; the read is bounded | Nothing to release | A request after the close started is refused with `closed`; a reloaded generation serves from its rebuilt bindings | FileReads |
| `record` | Keep the binding with the greater (`established`, `ordinal`) for the canonical path, under the caller's per-path lock | None | Nothing to release | The tool returns the operation's result; the new generation rebuilds from the line's facts, and a result fact that had not committed leaves nothing to rebuild from | FileReads |
| Load | Read this line's own `step/tool-result` events at one cutoff; each carries a `call_event_id` and its batch's `results[]`, and each call is joined to its `step/tool-call` event, whose sequence and whose `calls[]` index are the pair. Keep, per path, the result with the greatest (`call event sequence`, `ordinal`) among those that installed a binding and carry a `call_event_id`: a successful `read` of a text file, `write` or `edit`. The result carries that `TextFileBinding` as `structuredContent.binding` and its canonical `path`, so the fold takes it as it stands. An image read, a failed call, a refusal, a parent line's result and any result without a binding or without a `call_event_id` establishes nothing and is skipped | — | — | The reload replaces the generation; `apply` completes the fold before the service is registered, so no call sees a partial map. A fold that fails fails the generation's assembly, so there is never a partial map | FileReads |

The facts order the operations by each call's own event and its index in `calls[]`, the same pair the live `record` stored, so Load and `record` agree on which operation is last for a path, including a parent and a Code Mode child in one batch. Ordering is by the calls' events, not by their execution: a top-level call that changes the file and runs after a child of its batch is outranked by the child's binding, and the next `write` or `edit` answers `file_modified_since_read`, so the model re-reads. The rebuilt binding's `established` is the call event's sequence: no legal compaction cut separates a call from its result ([ContextCompaction §4.3](ContextCompaction.md#43-events)), so the call's position is the operation's start for the context rule. A path whose last such result is a read that a compaction cut out keeps the binding but cannot justify a `write`. A read at one sequence, a compaction that cuts it out, then an `edit`: the edit ignores the cut and records the binding it installed, so `binding` with `write` admits the next replacement after a reload.

A `binding` call that arrives while the generation reloads is answered by the generation still open, or refused with `closed` once the close has started; the replacement answers only after its fold, because its service is registered when `apply` returns. A line that no longer exists is the routing's answer ([#2127](https://github.com/arcships/dim/issues/2127)).

### Client-visible changes

The model-facing results stay as they are: `text_binding_required`, `file_modified_since_read`, their codes and the tool result text do not change, and no client calls `fileReads`. Two metadata changes follow from the rebuild, both in `structuredContent`:

| Change | Reason | Affected clients |
| --- | --- | --- |
| A `read`, `write` or `edit` result's `structuredContent.path` becomes the canonical path instead of the display path | The rebuilt binding is keyed by the canonical path, so the result must carry it | Clients that display `structuredContent.path`, such as a tool-call view showing the resolved file; the model reads only `content` and every refusal code is unchanged |
| All three results gain the `TextFileBinding` they installed as `structuredContent.binding`; a `read` keeps its flat fields beside it | Load folds the installed binding instead of inferring a format from an earlier read, so the result must carry it | Clients that read `structuredContent` see an additive field; the model-visible `content` and the refusals are unchanged |

## Supersedes

- `docs/design/service-faces.md` §3.5's `fileReads` row.
- `docs/design/contracts/runtime-owner-operations.md`'s "File read/edit/write tools → branch FileReads" row.
- The current implementation page (`docs/architecture/rust-agent/plugins/WorkspaceTools.md#filereads`) keeps describing the implementation until this contract lands; that section now links this design, its binding and admission statements — one binding per canonical path, `edit` and `write` requiring one, `write` requiring the operation to be in context — are the implementation of this contract, and its statement at `WorkspaceTools.md:508` that a reload loses the bindings is replaced by the rebuild.
- The current-state statements that a reload does not rebuild bindings describe the implementation until this contract lands; each gains a link to this contract when the delivery rewrites it: the [resume read audit](../appserver/resume-read-audit.md), the [capability-model coverage](../examples/capability-models/README.md), [coverage-fill](../examples/capability-models/coverage-fill.md), [coverage-audit](../examples/capability-models/coverage-audit.md), [timeline-capability-analysis](../examples/timeline-capability-analysis.md) with its empty-map and `get`/`established`/`in_context` statements, [timeline-contract-coverage](../examples/timeline-contract-coverage.md) with its `FileBindingQueries` row, and [FileReads.tla](../examples/capability-models/FileReads.tla) with its `NoRestoredBinding` invariant and [FileBindingQueries.tla](../examples/capability-models/FileBindingQueries.tla) with its binding queries.
- `docs/design/compaction-quality.md` §7.3's statement of the context rule and §8's Rule 8 line about folding the compaction fact, which this contract owns and which now link it.
- `docs/design/resource-faces.md` §4.3's sentences on the binding that travels with `write` and `edit` and on the control side's record, which now link this contract.
- `docs/design/timeline-branches.md`'s statements that `edit` and `write` refuse a file changed after this line last read it, which now say the last operation that bound it and link this contract.

## Verification

Flow tests on the owner:

- A read binds a file; a `write` after a compaction that cut the read out is refused with `compacted`; a compaction that kept the read leaves the write admitted; an `edit` proceeds after either.
- A write's or an edit's binding replaces the read's for later operations, keyed by the canonical path; a read whose result offload failed does not bind.
- Two reads of one file in one batch with an external change between them: the tie-break is the call's `ordinal`, the later one's binding is kept, and Load after a reload agrees; the case pins the tie-break, not an execution order.
- A read and a write of one file in one batch where the read runs first: both share one call event, the write's `ordinal` is the greater, its binding is the one kept, and Load after a reload agrees.
- A line forked from a line that read a file: the forked line answers `unavailable {neverRead}` for that file, so `write` needs a read on the fork; after that read the write is admitted. A reload rebuilds only the fork's own results.
- A top-level call and a Code Mode child in one batch: each has its own call event, the child's `ordinal` is 0, the later call event keeps the binding, and Load after a reload agrees.
- The reverse: a `code_mode` call and a top-level `write` of one file where the top-level write executes after the child — the child's binding outranks the write's, and the next write answers `file_modified_since_read`, so the model re-reads.
- A close between a successful write or edit and `record`: the tool returns that operation's result, the file keeps its bytes, and the next generation serves the bindings it can rebuild.
- A read of a relative path, then a reload, then a `write` to the same file: the rebuilt binding is the one the write consults.
- A read at one sequence, a compaction that cuts it out, then an `edit`: the edit ignores the cut and records a revision after it, the read supplies the format, and after a reload `binding` with `write` admits the next replacement.
- After a reload, Load folds each path's last result, which carries the binding it installed. A path whose last result is a read, outside any completed compaction's cut, can be written again without a new read; a path whose last result is a read that a compaction cut out stays unavailable for `write`.
- A batch with a top-level `write`, a Code Mode child's `write` of the same file and a non-ASCII `read` of it, then a reload: the fold takes the binding each result installed, the child's binding is the one kept (its call event is later), and the next write after the reload uses the same encoding live and rebuilt.
- An image read, a failed call, a refusal, or a result without a binding or without a `call_event_id` establishes no binding and is skipped by Load, so the path keeps the latest result that installed one before them.
- A `write` to a file this line has not bound answers `unavailable {neverRead}`, and the tool turns it into `text_binding_required`, so the model reads the file.
- A failed fact read answers `failed` for a `write`; a closed line answers `closed` on both operations.
- A fold that fails fails the generation's assembly; no partial map serves a call.
- The face and the Rust call answer the same reply variants and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | FileReads is one branch part and a Capability whose state is a projection: it injects `timeline` and `branchLifecycle`, folds this line's file-tool results when it loads, and at `binding` with `write` reads the latest completed compaction; it starts no work and writes no facts. |
| §2 scopes | Branch: bindings are one line's own and a forked line starts empty; the file tools are Session parts that reach this owner by line id. |
| §3 orthogonality | No other owner's state is read; the facts are the line's Timeline. |
| §4 rule 1 | No command event: the owner writes nothing. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | No batch. |
| §4 rule 6 | No model-context input: the bindings are not context. |
| §4 rule 7 | The tool results are public facts; the owner folds them. |
| §4 rule 8 | No infrastructure change of its own: the two optional `ToolExecutionContext` fields are a field addition on the `toolHost` Session face, set by ToolExecutor; the routing is ADR 027's requirement, not a new port or checkpoint here. |
| §4 rule 9 | No Timeline fact of its own; the bindings are a projection of public facts, and the projection is this owner's state. |
| §7.1 resources | No resource: the owner stores values and returns values, and nothing leaves it to be settled elsewhere. |
| §7.1 lifecycle | The bindings live with the branch instance; a reload rebuilds them from the line's facts, and an operation whose result fact had not committed leaves nothing to rebuild from, so its binding ends with the generation. |
| §7.1 external business | `binding` and `record`, with the cancellation and errors in the tables above. |
| §7.2 objects | No object crosses: every parameter and reply is data, and the owner keys bindings by a path string. |
| §7.2 no proxy | The owner answers its own bindings; it returns no file's bytes and checks no other owner's data. |
| §7.2 one surface | `binding` and `record`; `get`, `write_binding`, `established`, `set` and `WriteBinding` are removed (the delivery issue carries the full deletion list). |
| §7.2 cross-language | Every parameter and reply is data; the caller is a Session part reached by line id. |
| §7.2 responsibility split | The caller keeps the file, its bytes and the result text; the owner keeps the bindings and the context rule. |
| ADR 002 D1 | Branch scope: a binding is one line's having seen a file. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | The dependency direction holds: the file tools are callers that reach a line by id, which ADR 027 D4 makes not a dependency, and FileReads declares only branch services (`timeline`, `branchLifecycle`); a composition without the file tools has no bindings. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Language-neutral operations and data. |
| ADR 027 | The file tools reach a line's FileReads by line id, without a declared dependency; the path and purpose stay in the request. |

## Requirements raised to mechanism owners

- **Cross-scope access ([ADR 027](../../adr/027-cross-scope-access-by-id.md), [#2127](https://github.com/arcships/dim/issues/2127), target).** A Session part reaches a line's data face by line id. The file tools use it to reach `binding` and `record`; FileReads adds no forwarding operation and declares no dependency on the file tools.
- **File-tool codec and result paths (WorkspaceTools, [#2026](https://github.com/arcships/dim/issues/2026), target).** The binding's wire shape is the existing `TextFileBinding` ([resource-faces §4.3](../resource-faces.md#43-read-write-and-edit)). The codec's `TextEncoding`, `EncodingSource`, `Bom`, `Eol` and `TextFileBinding` are not wire data (`TextEncoding::Legacy` holds an `encoding_rs` reference and `raw_revision` is `[u8; 32]`), so the file tools project them into the DTOs below: the revision as lowercase hex; `TextEncoding` as `{kind: "utf8" | "utf16le" | "utf16be"}` or `{kind: "legacy", name}` with the requested name lowercased; the source as the codec's `bom`, `user` or `utf8-policy` label; and `Bom`/`Eol` as their published names. The DTO's `EncodingSource` has the fourth value `Workspace`: the file tool maps the node's `workspace` source into it and back, and the codec's own `EncodingSource` keeps its three variants, so a node-resolved binding round-trips unchanged. Each of `read`, `write` and `edit` holds the per-path lock from before its checks or read until after `record`. Every `read`, `write` and `edit` result that installs a binding carries it as `structuredContent.binding` and the canonical path as `path`, because Load folds that field and keys it by that path; a `read` keeps its existing flat fields, which hold the same values. A refusal keeps the result shape it publishes today. Deriving serde on the codec types is not a substitute; the projection is the contract. The codec and the result shape belong to WorkspaceTools; this owner changes neither.
- **Tool execution context (ToolExecutor, [#2028](https://github.com/arcships/dim/issues/2028), target).** `ToolExecutionContext` gains `sequence: Option<u64>` (the call's own `step/tool-call` sequence) and `ordinal: Option<u32>` (its position in the batch's `calls[]`), set by ToolExecutor for every call, a Code Mode child included: a child has its own call event and an index of 0. Both are `None` where no call event exists: the batch claim a request registers before `step/tool-call` (`ToolBinding::claim` at `crates/dim-agent-capabilities/src/agent_loop.rs:1705`) and a driver's definitions carry turn, request and branch only. The batch claim context is therefore not a call's context, and the pair is set only on each call's own context. `record` stores the pair; Load takes the sequence from the result's `call_event_id` and the ordinal from the call's index in its batch's `calls[]`, so live and rebuilt orderings agree. The file tool cannot derive the pair from its `call_id`: `call_id` identifies the call within its batch, and neither the call event's sequence nor the batch index is on the tool's input; the executor is the only owner that holds both when it dispatches. A batch's own context carries the batch's `step/tool-call` sequence and no ordinal, and `ToolExecutor::open` admits a Code Mode child by the turn, request and branch its context shares with the batch, not by full equality and not by the pair, because the child has its own call event. The fields belong to the type published on the `toolHost` Session face, whose owner is WorkspaceTools. They are additive on the wire, so TypeScript callers that omit them keep working; every Rust literal that builds a `ToolExecutionContext` adds them, `None` where no call exists, and `cargo run --locked -p xtask -- types` regenerates `npm/branch-services.d.ts`. They change neither Timeline, Dispatcher, assembly nor the Session and branch facades (§4 rule 8) and need no new event or service. FileReads adds no operation for them.

## Binding appendix

The service is marked on the owner type; the name stays `fileReads`. Three types carry the name `TextFileBinding`: the published result type in `crates/dim-agent-types/tests/fixtures/baseline/types/tool.ts` ([resource-faces §4.3](../resource-faces.md#43-read-write-and-edit)), whose values a `read` result also carries in its flat fields beside `structuredContent.binding`; the face's Rust spelling below, with the same fields and wire values; and the file tools' internal codec type (`crates/dim-agent-capabilities/src/workspace_tools/file_reads.rs`), which is not wire data. The request, reply and error types are this face's own, and the file tools project between the face's spelling and the codec type; that projection is the requirement raised to WorkspaceTools above.

```rust
use dim_agent_runtime::PublicService;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Write,
    Edit,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingRequest {
    /// The canonical path the file tool resolved.
    pub path: String,
    pub purpose: Purpose,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFileBinding {
    /// Lowercase hex of the SHA-256 of the raw file bytes.
    pub raw_revision: String,
    pub encoding: TextFileEncoding,
    pub encoding_source: EncodingSource,
    pub bom: Bom,
    pub eol: Eol,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TextFileEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Legacy {
        /// The requested name, lowercased.
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodingSource {
    Bom,
    /// The file node's resolution, passed through from a remote Session.
    Workspace,
    User,
    #[serde(rename = "utf8-policy")]
    Utf8Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bom {
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    None,
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unavailable {
    NeverRead,
    Compacted,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum BindingReply {
    Bound { binding: TextFileBinding },
    Unavailable { reason: Unavailable },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordRequest {
    /// The canonical path the file tool resolved.
    pub path: String,
    pub binding: TextFileBinding,
    /// The call event's `step/tool-call` sequence; a batch's calls share it, a child has its own.
    pub established: u64,
    /// The call's position in its batch's `calls[]`.
    pub ordinal: u32,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FileReadsError {
    #[error("the line is closing")]
    Closed,
    #[error("the binding could not be read: {reason}")]
    Failed { reason: String },
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RecordError {
    #[error("the line is closing")]
    Closed,
}

impl PublicService for FileReads {}

#[rutis::service(name = "fileReads")]
impl FileReads {
    pub async fn binding(&self, request: BindingRequest) -> Result<BindingReply, FileReadsError> { /* ... */ }
    pub fn record(&self, request: RecordRequest) -> Result<(), RecordError> { /* ... */ }
}
```
