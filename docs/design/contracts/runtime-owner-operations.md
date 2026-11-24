# Session and branch owner operations

## Retained operations

| Caller → owner | Final API |
| --- | --- |
| Branch management, input and history callers → `timelineTree` | `create/end/info/input/list/page/query` retain their existing signatures on the single Session tree face. Creation returns the branch ID. |
| Language readers → PromptSources | `sessionLanguage.get`. |
| Language updates → App Server | [`session.language.update`](session-access.md) accepts the existing language code or clear action. The owner serializes persistence, live-state replacement and Prompt adoption on main and loaded branches before replying. |
| Prompt → PromptRegistry; guide and tool contributors → their registries | `initialPromptConfiguration.get`, `promptHost.load`, `guideEntries.register`, `toolRegistry.register`, `toolSourcesView.get/observe`. |
| Session and branch parts → their lifecycle; branch fact consumers → Dispatcher | `sessionLifecycle.activated/closing/hold/release`, `branchLifecycle.activated/closing`, `dispatcher.listen/consume/committed`. |

## New rutis APIs on existing owners

### Admission and branch control

| Caller → owner | New API |
| --- | --- |
| Branch directory readers → `timelineTree` | `timelineTree.directory({}) → [{info, head, state, endEvent?}]`, including ending and ended branches. |
| Skills content and MCP publication → Session loader | `sessionLoader.create({group, definitionId, instanceData}) → {instanceId}`; `remove({instanceId}) → void`. The Session loader admits groups whose configured parent is the Session group. Registered Session extension definitions must activate their instance rows. Host-added optional groups retain their optional rows. Skills supplies its content definition; MCP supplies its connection definition ID and retains the resolved transport configuration. |
| Goal → timelineTree | `inputs(id, inputs) → committed events`. One validated atomic batch for the Goal record and trigger. |
| Branch-detail RPC → timelineTree | `display(id) → {entry,purpose?,cutMessages?,end?}`. |
| History RPC and line-read tools → timelineTree | `batchEnd(id, sequence) → void`, `pageEnd(id, after, limit, through) → sequence`, and `page` at the same cutoff. Preserve complete batches and the fixed head of ended lines. |
| Context RPC → target branch | `contextPreparation.compact` and `contextUsage.read`; the owner contracts are [ContextPreparation](../plugins/ContextPreparation.md) and [ContextUsage](../plugins/ContextUsage.md). |
| Goal and Shell admission → timelineTree | `directory({id})` returns the matching directory entry or the unknown-branch error using a single-branch lookup. Goal reads ended history through `query`; writes retain the ended/unknown errors. |
| InputQueue and Collaboration approvals → target branch | `inputQueue.submit`, `inputQueue.withdraw` (the queue runs the submit checks for a Steer), and `approval.pending/decide` on the selected branch; see [InputQueue](../plugins/InputQueue.md). |
| PromptSuggestion → ContextUsage; Report and Agent configuration → Prompt | `contextUsage.read`, `prompt.adoptedConfiguration({}) → adopted configuration or null`. Report also uses `timelineTree.query` at the selected head for evidence. |
| Prompt and ToolBinding → `branchLifecycle` | `registerOpening(completion) → void`. Opening waits for admitted completions under its existing deadline and propagates their errors. |
| Fault producers and Session observers → `sessionLifecycle` | `reportFault(error) → void`, `failure({}) → SessionFault or null`, `observeFault(listener) → id`, `release(id) → boolean`. The listener receives the current failure (initially null) and subsequent reports. Caller unload releases the observation. The first fault wins until a storage failure supersedes it; the first storage failure wins. |
| Check providers and input admission → SubmitChecks | `register(name, check) → void`; `check({entry, branchId, user?, mode}, cancelled) → proceed{blocks, warnings} or block{message}`. Run in name order through the first block. Interrupted checks write nothing. |
| Compaction → `contextPreparation`; stop and activity callers → `agentLoop` | `contextPreparation.compact(..)`; `agentLoop.stopTurn/state/observe`; see [Loop](../plugins/Loop.md) and [ContextPreparation](../plugins/ContextPreparation.md). |

### Calls from a Session to a selected branch

Within a branch, inject its owner face directly. For Session callers the existing
`timelineTree` face gains `call({branchId,operation}, cancelled) → operation reply`.
TimelineTree resolves the admitted live branch once using its existing branch lookup and
dispatches on that branch instance. The operation is a tagged union of the calls below;
Direct variants use the owner method's arguments, reply and error. The compact RPC variant below returns the existing settled client result. The Session caller gets data.
Ended and unknown branches retain their original errors. Calls admitted before close stay
with that generation. Tree history, directory and batch input continue to use the tree's
own methods.

| Session caller | Target operation |
| --- | --- |
| Context RPC / PromptSuggestion | `contextUsage.read`; Context RPC's `compactAndWait` is `contextPreparation.compact`, which answers the outcome itself. |
| Session language / Agent configuration / Report | `prompt.configure` or `prompt.adoptedConfiguration` with the existing source, model and outcome records. |
| InputQueue management / Collaboration | `inputQueue.withdraw` with the existing command; the queue runs the submit checks for a Steer. `approval.pending` returns the branch's existing pending requests, and `approval.decide` settles the selected request. Collaboration retains its ended response on closed/ended/unknown detail lookup. |

For a live main branch, Report captures its published head once at draft capture (`directory({id:"main"}).head`)
and stores it as `conversationThrough` on that draft. Activity evidence and later opt-in
conversation extraction call `timelineTree.query` with the same explicit `through`,
existing bounded window and `include_publishing: false`. Zero yields an empty window.
A draft without a captured cutoff does not fetch a conversation. Plan generation never
substitutes a newer head.

### Prompt, configuration and files

| Caller → owner | New API |
| --- | --- |
| ToolBinding, Session language and Agent configuration → `prompt` | `prompt.configure({sources, model?, tools?}) → unchanged, adopted or superseded`; the opening adoption is internal. See [Prompt adoption](../plugins/PromptAdoption.md). |
| Prompt and guide adoption → `prompt` | publication is `prompt.configure`; see [Prompt adoption](../plugins/PromptAdoption.md). |
| File read/edit/write tools → branch FileReads | `binding({path}) → TextFileBinding or null`, `writeBinding({path}) → absent, compacted or bound`, `record({path, binding, established}) → void`. The original read supplies the established sequence; file I/O stays on the file node. |

### Model preparation, history and tools

| Caller → owner | New API |
| --- | --- |
| Loop → RequestSettings | `settings() → settings`; the model profile's defaults before an adoption ([#2093](https://github.com/arcships/dim/pull/2093), target). |
| ContextPreparation → PrepareContextHost | The model profile's request builder stays the host port `prepare` calls; the owner contract is [ContextPreparation](../plugins/ContextPreparation.md). |
| Context accounting and compaction → ContextFacts | `describe`, `count`, `contributions`, `estimate`; ContextFacts is the owner ([#2093](https://github.com/arcships/dim/pull/2093), target). |
| ContextPreparation → SummaryStrategy | `describe(model facts) → {name, replacementSize}`; `compact(input, cancelled) → completed, skipped, failed or cancelled` with StrategyDiagnostics; SummaryStrategy is the owner ([#2093](https://github.com/arcships/dim/pull/2093), target). |
| Loop and ContextPreparation → ContextProjection | `renderedStream`, `renderedContext`, `snapshot`, `streamCheckpoint`, `snapshotFromCheckpoint`; ContextProjection is the owner ([#2093](https://github.com/arcships/dim/pull/2093), target). |
| Loop and ModelProvider → ContextPreparation | `prepare({source, projection}, cancelled) → void`, `validate({request}, cancelled) → void`, `compact({}, cancelled) → outcome`; the owner contract is [ContextPreparation](../plugins/ContextPreparation.md). |
| ContextPreparation and usage readers → ContextUsage | `read({through?}, cancelled) → UsageRead`, `estimate({requestId, through}, cancelled) → Estimate`, `confirm({requestId}) → void`; the owner contract is [ContextUsage](../plugins/ContextUsage.md). |
| Loop and ToolExecutor → `toolBinding` | `bind({requestId, turnId, through, declared, sources})`, `adopted({requestId}) → {adoption, tools}`, `claim({requestId, calls, context})`, `transfer({from, to})`, `release({requestId})`, `retain_only({requestId})`; settlement follows the line's facts and `retain_only` is the Loop's rebuild step ([#2100](https://github.com/arcships/dim/pull/2100), target). |
| Nested tool execution → `toolExecutor` | `open({parentCallId, context}, cancelled) → gateId`, `invoke({gateId, call}) → ToolResult`, `close({gateId}) → ToolEffect[]`. Close joins children in order; interruption releases the gate. |
