# ImageUnderstanding

Status: Draft.

Scope: Session.

Issue: [#798](https://github.com/arcships/dim/issues/798).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

Images a Session receives — attachments ([Attachments](Attachments.md) A.1) and `read`
([WorkspaceTools § read results and images](../../architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images))
— reach the conversational model only as references, and the model asks about them through the
`image_understand` tool. Which model answers is Modality's `modality.image.understand` target
([Modality](Modality.md) A.1), resolved by its owner and executed through the existing
`auxiliaryModel` host. The conversational model never receives image pixels, and the image's
stored content never changes.

## 1. Scope and non-goals

Owns:

- the `image_understand` tool: its declaration, its reference and question parameters, its
  reference resolution against the calling branch, its limits, errors and reuse;
- the request it hands to the `auxiliaryModel` host: the image, the question and the
  instruction, and nothing else.

Does not own:

- how an image block becomes a reference in a request — ModelProvider, stated in
  [Attachments](Attachments.md) A.1;
- which model answers — Modality's `modality.image.understand` resolution;
- executing a provider call — Provider's connection and the `auxiliaryModel` host;
- the stored image and its authorization — the Timeline and the branch facade.

## 2. Own contract

Tool `image_understand`, registered with `toolRegistry` by this Session part
([ToolRegistry](../../architecture/rust-agent/plugins/ToolRegistry.md)). Parameters:

| Field | Rule |
| --- | --- |
| `image` | The reference copied verbatim from the request (§3) |
| `question` | What to ask about that image; non-empty, at most 4 KiB of UTF-8. A byte limit, not a token count |

The result is text: the answer, and nothing derived from the conversation. The tool keeps no
state of its own and changes no image; it reads the calling branch's history (§3).

Errors, each with an actionable reason:

```text
image_understand {image, question}
  |-- params of the wrong shape ------------------------------> invalid_params
  |-- question empty or over 4 KiB ---------------------------> image_question_invalid
  |-- reference not an image visible in this branch ----------> image_reference_unknown
  v
Modality: the modality.image.understand target
  |-- configured default unavailable -------------------------> image_model_unavailable (owner's reason)
  |-- execution through auxiliaryModel -> answer -------------> result text
  '-- provider, account or connection failure ----------------> image_understand_failed
```

A configured default that is no longer selectable fails the call with its reason; the tool never
substitutes another model. Cancellation is not a result code: the call returns the host's
cancellation after it releases the request, as any other tool call
([ToolRegistry](../../architecture/rust-agent/plugins/ToolRegistry.md)).

An answer whose `stop_reason` is `MaxTokens` is returned with the text it produced and an
explicit incomplete marker; it is not reused.

**Admission.** The tool declares risk `session`, so it runs under every preset without a
per-call approval ([ADR 003](../../adr/003-approval-scope.md)). Consent comes from the
configuration, and that is a weaker statement for automatic resolution than for a persisted
default, so both are named here. With a persisted `modality.image.understand` default the user
named the model and the image goes where that model is served. With no default, Modality
resolves automatically (Provider's global selection when it serves the operation, otherwise the
first selectable vision model) and no one chose that provider for images; the snapshot reports
the effective model, but not why automatic resolution picked it. The image goes to whichever
provider serves the effective target either way, and this design adds no second place to choose
it.

That holds for both paths into the tool, and the two are not equal. An attached image enters by
the user's own action; an image the model `read` from the workspace enters because the model
called `read`, under the read permission the Session already grants, and the tool then sends it
to the effective understanding target. With no persisted default, resolution is automatic and
may pick a provider and account the user never named for images; clearing a default restores
that, which the issue requires. Declaring `network` instead would deny the call under read-only
and ask on every call under workspace-write, treating a configured setting as an ad-hoc decision
on each image. A deployment that must not send `read` images to an unchosen provider restricts
the tool through Permission rather than through this design.

## 3. References

A reference is `img:` followed by 16 hexadecimal characters of the SHA-256 digest of the decoded
image bytes — not of the base64 text — so the same image submitted as an attachment and returned
by `read` carries one reference. It names content, not a location: the same bytes give the same
reference, a copy gives the same reference, and reordering, compaction or a cut does not change
it. The rule that puts it in a request in place of the image block is ModelProvider's, stated by
[Attachments](Attachments.md) A.1; this design only consumes the string it produces.

Resolution reads the calling branch: the tool looks for an image block carrying that digest in
the branch's own Timeline, including the ancestry it inherited before a cut. A reference that no
visible event carries is `image_reference_unknown`. Visibility is the only authorization: a
reference confers no reach beyond the history the branch already has, and no path, id or grant
is added for it. Compaction does not invalidate a reference, because resolution reads the
Timeline rather than the model's window; a cut removes the branches it cut off.

The digest index the tool keeps is derived state: built on demand from committed events, never
persisted, and discarded per generation. It holds the digest and the location, not the bytes;
each call reads the block again.

An image block that carries no decodable bytes — a URL source, undecodable data, a format this
build cannot decode — has no reference. ModelProvider keeps a text placeholder for it, as
Attachments A.1 already decides, and no `image_understand` call can reach it.

A model that accepts image input receives references too. Sending it the pixels would answer the
image without Modality's default, which is the defect this design removes, so the request has
one rule for every model and the cost is one tool round where a vision model used to see the
image directly. The alternative — references only when the request's model cannot accept
images — was rejected for that reason and remains available only as a change of this design.

## 4. Model choice and execution

Each call resolves `modality.image.understand` through Modality's typed read (§5, M1) and uses
the resulting target for that call and its retries. Explicit, configured default and automatic
precedence, and the reason an unavailable target reports, are Modality's rule; this design
consumes the answer.

Execution uses the `auxiliaryModel` host's named-binding form: the resolved model, an
instruction fixed by this design (the request's `system`, not the Agent System), one user
message carrying the image block and the question, and the output limit. The request carries no
conversation, no tool definitions and no Session identity. The answer is text.

The instruction states: answer only from the image, answer the question asked, say when the
image does not support an answer. Its text is versioned; a change to it invalidates reuse (§6).

Limits: at most 4 KiB of question, and `max_output_tokens` 2048 for the answer unless the
resolved model declares less. Changing either is a change to this design.

## 5. Cross-module requirements

In the format of [design rules §4 rule 8](../../architecture/rust-agent/principles.md). Each
owner changes its own document.

**M1 Modality: the operation target read.**

- **Problem.** A Session part must learn which model `modality.image.understand` resolves to now. Modality's process surfaces are the task methods (`modalityTasks`) and the adapter copies (`modalityAdapter`); none of them answers "which model would this operation use now", and `modality.capabilities` is a management method served to clients, which a part is not.
- **Existing mechanisms, and why they fail.** `modality.capabilities` is a management method, and a part is not a client. `modalityTasks` is the task surface and refuses `understand` by design (Modality decision 6). Provider's `selectableModels` enumerates what execution accepts for chat, and `selectableForModality` widens that to models execution rejects for chat; neither applies Modality's persisted default or its explicit → default → automatic precedence, so a consumer using them would decide with a second rule.
- **Needed capability.** One process service, `modalityTargets`: for `<category>.<operation>`, the target it resolves to now, or the reason it is unavailable, with the same precedence and reason vocabulary as the snapshot. Read per call; no task, no credential, no gate, no reload.
- **Ordering impact.** Per tool call. Modality stays Active when an adapter or model is absent.
- **Owner and issue.** Modality, [#798](https://github.com/arcships/dim/issues/798) §9.4.

**M2 Attachments and WorkspaceTools: the reference in place of pixels.**

- **Problem.** Images reach the conversational model as image blocks, so a model cannot be chosen for understanding at all: the request's own model sees the pixels.
- **Existing mechanisms, and why they fail.** ModelProvider's per-request rule ([Attachments](Attachments.md) A.1) sends an image when the request's model accepts images and a text placeholder otherwise. Neither branch can carry a resolved understanding target, and `read` returns image blocks the same way.
- **Needed capability.** Every image block in a request becomes the reference text (§3): user content from any writer, and tool results. History, the Timeline and the adopted input stay unchanged, so the decision remains per request. Attachments A.1's vision/placeholder table becomes this one rule, and the `read` section states the same outcome for its results.
- **Ordering impact.** Per request; no event; no reload.
- **Owner and issue.** The request rule is ModelProvider's, implemented in `model_provider/images.rs`; Attachments owns A.1's statement of it and WorkspaceTools owns the `read` statement. [#798](https://github.com/arcships/dim/issues/798).

**M3 Prompt: the entry.**

- **Problem.** The model receives a reference string with nothing telling it what it is or what to do with it.
- **Existing mechanisms, and why they fail.** The input guide carries entries contributed by the capability that writes a marker ([Prompt](Prompt.md) §4); none describes an image reference.
- **Needed capability.** One entry: images appear as references, `image_understand` answers questions about them, and a reference that is no longer in the branch cannot be resolved.
- **Ordering impact.** None: composed per request with the other entries.
- **Owner and issue.** Prompt, [#798](https://github.com/arcships/dim/issues/798).

**No requirement raised to Provider or the assembly.** The `auxiliaryModel` host already takes the model for one call ([AuxiliaryModel](../appserver/auxiliary-model.md)); the product assembly only installs this part next to the other Session parts.

**M4 TimelineTree: published branch-chain queries for Session parts.**

- **Problem.** A Session tool needs the calling branch's inherited, published history, not just its own line (§3).
- **Existing mechanisms, and why they fail.** `treeCommands.query` reads only one line; `Branch::query` supports inheritance but obtaining a branch through the Rust-only `get` method bypasses the data face. Reassembling ancestry in the consumer would duplicate the owner's range and continuation rules.
- **Needed capability.** Extend the existing `treeCommands.query` operation with published chain scope, using the shared property-query shape. TimelineTree owns the range, ancestor cuts, ended-line reads and continuation, as specified on its [owner page](../../architecture/rust-agent/plugins/TimelineTree.md). The tool consumes this operation, not a branch handle or storage internals.
- **Ordering impact.** Read-only, fixed published cutoff; no change to sequences, batches, interruption, reload or event formats. Own-line queries retain their existing behavior.
- **Owner and issue.** TimelineTree, [#1624](https://github.com/arcships/dim/issues/1624).

## 6. Reuse

Reuse is limited to the calling branch. Key: the reference, the question bytes, and the
instruction version. A candidate is reused only when it is a completed, non-truncated answer
whose resolved target is still the model this call resolves to.

The index is derived from committed tool facts of that branch — the calls and their results —
and is rebuilt lazily, never persisted and never shared across Sessions. Failed, cancelled or
incomplete calls are not reused. Identical calls running at the same time in one branch share
one provider call; it lives until the last of them leaves, so one caller cancelling or dropping
its call does not cancel work another caller still waits for.

There is no exactly-once guarantee: a crash or a lost reply before the result commits can cost
another paid call.

## 7. Events and state

No new event type. A call is an ordinary `step/tool-call` and its result, written by Loop like
any other tool, so the answer enters context through the existing tool-result boundary
([principles §4 rule 6](../../architecture/rust-agent/principles.md)). Nothing else is
written, and the part keeps no state that is not rebuildable from those facts.

## 8. Rule check

Rules: [principles §1 and §4](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md) D1–D5.

| Rule | How this design complies |
| --- | --- |
| §4.1 Command events for others' state | The tool returns a result; it changes no other owner's state. Model selection is read from Modality. |
| §4.2 No recall after handoff | References come from the request the model received; the tool resolves them inside the branch and touches no input. |
| §4.3 Only before callbacks, never block | No before callback. The call registers its own task and returns; on cancellation it releases the request and returns the host's cancellation. |
| §4.4 No business meaning in core | No core change: the tool is registered through `toolRegistry`, and the reference rule stays in ModelProvider as Attachments already owns it. |
| §4.5 Atomic batches | The design writes no event itself; Loop's ordinary tool-call batch covers the call and its result. |
| §4.6 Context only from handed-off events | The answer reaches the model only as a tool result; no injection during request preparation. |
| §4.7 Public events and services only | The part uses `toolRegistry`, `treeCommands.query` with published chain scope (§5 M4) to resolve a reference, the declared `auxiliaryModel` host, `sessionFiles` to read stored image bytes, `guideEntries` for its input-guide entry, and Modality's M1 read. |
| §4.8 Requirements to mechanism owners | §5, M1–M4. The existing TimelineTree query gains published chain scope; no new runtime or rutis mechanism. |
| §4.9 Only facts later decisions use enter the Timeline | Only the ordinary tool facts enter; the digest index is derived and discarded. |
| §1 Capability and App Server boundary | A Session Capability in its own crate; the App Server only installs it. |
| ADR 002 D1 Scope | Session: the tool table is the Session's, and a call acts in the calling branch. |
| ADR 002 D2 No wiring modules | `toolRegistry` by name; nothing names this part. |
| ADR 002 D3 Scope dependencies | Declares Modality's read and the auxiliary host; Modality does not depend on it, so there is no reload edge. |
| ADR 002 D4 Configuration | No configuration: limits and instruction are fixed here; the model is Modality's setting. |
| ADR 002 D5 Language-neutral contracts | The tool declaration, its parameters, its errors and the reference string are neutral; a Rust binding is this crate. |
| ADR 003 Approval scope | Admission is the declaration's risk class, not a per-call decision on the configured understanding model (§2). |

## 9. Acceptance

- A fixture flow: an image attached in `session.submit` and one returned by `read` reach the
  request as references; `image_understand` answers from the resolved target.
- Changing the configured `modality.image.understand` model changes the model the fixture's
  provider sees; clearing the default restores automatic resolution; an unavailable default
  fails the call with its reason and no fallback.
- A reference from another Session, an unknown digest, an oversized question and a truncated
  answer each produce their stated outcome.
- Compaction does not break a reference that is still in the branch's history; a cut does for
  the branch it removed from.
- Text-only Sessions are unchanged: no tool call, no reference, no extra request.
