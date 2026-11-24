# ImageUnderstanding

Owner: [dim-image-understanding](../../crates/dim-image-understanding/src/lib.rs).
The product installs this Session Capability next to Modality. Its contract is
[ImageUnderstanding](../design/plugins/ImageUnderstanding.md): a vision-capable
conversational model, or a request whose model capability is unknown, receives a
sendable image as pixels where the destination takes its format; a model without
image input, or an image that cannot be sent, sees an `img:` reference or
placeholder and can ask `image_understand {image, question}` for a one-shot answer.
A standalone request cannot resolve a reference, so a target that does not take
the image's format fails the call without an answer, and a reused answer follows
the format rule of the route that serves the target now. The tool is risk
`session`; unknown input fields are rejected.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolRegistry` | Register the tool for this generation. |
| `S:TreeCommands` | Query published inherited images and own-line tool facts. |
| `P:SessionFiles` | Materialize immutable image bytes within the Session. |
| `S:SessionIdentity` | Scope file reads to the calling Session. |
| `P:ModalityTargets` | Resolve `image.understand` on every call, including reuse. |
| `P:AuxiliaryModel` | Execute against the resolved target without conversation or tools. |
| `P:ModelSelectionHost` | Read the protocol of the route that serves the resolved target now, before an answer is reused. |
| `S:GuideEntries` | Explain image routing and references through the input guide. |

These are the existing owners' faces, not new services. The tool consumes only
`treeCommands.query` with published chain scope, `sessionFiles.read`,
`modalityTargets.resolve`, `auxiliaryModel.complete`, `modelSelectionHost.resolve`
and the registration routes of ToolRegistry and GuideEntries. See their owner
contracts rather than a separate image-specific model or storage interface. A
destination whose format rule the owner does not report is an unknown one, which
takes the four formats a request may carry.

## Facts, state and lifetime

No subscriptions and no new events. ToolExecutor commits the ordinary tool call and
result. Completed answers carry `imageUnderstanding: {version, target}` in
`structuredContent`; the model-readable answer remains in `content`. Together with
the matching tool call, this is sufficient to reconstruct own-branch answer reuse.
An error or output-limited answer carries no reusable stamp.

An on-demand index keeps references and event/block locations, never image bytes.
It reads the published chain at a fixed cutoff, then incrementally indexes the
calling line's newly published events. The tool rereads the image for each call;
compaction changes neither its identity nor its visibility. A new branch builds
its own inherited image index and cannot reuse another branch's answers.

Concurrent identical calls in one branch share a future held only by its waiters.
One waiter leaving releases only its share; the last waiter cancels and drops the
provider request. Unload cancels this generation's work and discards its indexes.
No result is reused until its ordinary tool result has committed, and not while
the format rule of the route that serves the target now does not take the image's
format. An answer another caller has in flight is refused before this call waits
for it.

## Verification

`cargo test --locked -p dim-image-understanding` exercises the public tool flow
through the real Modality and auxiliary-model owners, with an isolated home and
a fixture HTTP provider. Its compaction case consumes a completed compaction fact,
then rebuilds the reference index after restore; it does not run a summarizer.
Request adaptation and immutable file-backed history are covered
by `dim-agent-capabilities`'s `host_flows`; routing by the conversational model's
vision capability is covered by `image_flow` and `read_image_flow`; a standalone
call whose target cannot take the image's format, and a completed or in-flight
answer whose route stops taking it, are covered by `image_flow`; published
inheritance and ended/restored queries are covered by `branch_flow`.
