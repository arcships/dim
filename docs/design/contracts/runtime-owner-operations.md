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
| Skills content and MCP publication → Session loader | `sessionLoader.create({group, definitionId, instanceData}) → {instanceId}`; `remove({instanceId}) → void`. The Session anchors each instance. Skills supplies its content definition; MCP supplies its connection definition and resolves driver and credentials locally. |
| Goal → timelineTree | `inputs(id, inputs) → committed events`. One validated atomic batch for the Goal record and trigger. |
| Branch-detail RPC → timelineTree | `display(id) → {entry,purpose?,cutMessages?,end?}`. |
| History RPC and line-read tools → timelineTree | `batchEnd(id, sequence) → void`, `pageEnd(id, after, limit, through) → sequence`, and `page` at the same cutoff. Preserve complete batches and the fixed head of ended lines. |
| Context RPC → target branch | `agentLoop.compact` and `contextUsage.read`. |
| Goal and Shell admission → timelineTree | `directory({id})` returns the matching directory entry or the unknown-branch error using a single-branch lookup. Goal reads ended history through `query`; writes retain the ended/unknown errors. |
| InputQueue and Collaboration approvals → target branch | `timelineTree.input`, `inputQueue.withdraw` after submission checks, and `approval.pending/decide` on the selected branch. |
| PromptSuggestion → ContextUsage; Report and Agent configuration → Prompt | `contextUsage.read`, `prompt.adoptedConfiguration({}) → adopted configuration or null`. Report also uses `timelineTree.query` at the selected head for evidence. |
| Prompt and ToolBinding → `branchLifecycle` | `registerOpening(completion) → void`. Opening waits for admitted completions under its existing deadline and propagates their errors. |
| Fault producers and Session observers → `sessionLifecycle` | `reportFault(error) → void`, `failure({}) → AgentError or null`, `observeFault(listener) → subscription`. First fault wins with existing storage-fault precedence. |
| Check providers and input admission → SubmitChecks | `register(name, check) → void`; `check({entry, branchId, user?, mode}, cancelled) → proceed{blocks, warnings} or block{message}`. Run in name order through the first block. Interrupted checks write nothing. |
| Compaction, stop and activity callers → `agentLoop` | `agentLoop.compact({}) → sequence`, `stopTurn({turnId, reason}) → sequence or null`; `agentLoop.state({}) → {phase, turnId?}`, `observe(listener)` delivers that snapshot. |

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
| Context RPC / PromptSuggestion | `contextUsage.read`. The `compactAndWait` variant runs the existing compact RPC handler: it calls `agentLoop.compact` and waits for the compaction fact matching that command sequence and returns the existing completed/skipped/failed reply. The committed sequence watch and bounded query run inside the selected branch for that wait. |
| Session language / Agent configuration / Report | `prompt.configure` or `prompt.adoptedConfiguration` with the existing source, model and outcome records. |
| InputQueue management / Collaboration | `inputQueue.withdraw` with the existing command; the caller runs the existing submit checks for steer before writing. `approval.pending` returns the branch's existing pending requests, and `approval.decide` settles the selected request. Collaboration retains its ended response on closed/ended/unknown detail lookup. |

For a live main branch, Report captures its published head once at draft capture (`directory({id:"main"}).head`)
and stores it as `conversationThrough` on that draft. Activity evidence and later opt-in
conversation extraction call `timelineTree.query` with the same explicit `through`,
existing bounded window and `include_publishing: false`. Zero yields an empty window.
A draft without a captured cutoff does not fetch a conversation. Plan generation never
substitutes a newer head.

### Prompt, configuration and files

| Caller → owner | New API |
| --- | --- |
| ToolBinding, Session language and Agent configuration → `prompt` | `prompt.initialCompletion({}) → void or opening error`; `prompt.configure({sources, model?}) → unchanged, adopted or superseded`. |
| Prompt and guide adoption → `contextPreparation` | `contextPreparation.publish({adopted, changes, model?, toolAdoption?, inputs}) → sequence or null`; `awaitPublished({sequence}) → sequence`. Publication commits adoption atomically. |
| File read/edit/write tools → branch FileReads | `binding({path}) → TextFileBinding or null`, `writeBinding({path}) → absent, compacted or bound`, `record({path, binding, established}) → void`. The original read supplies the established sequence; file I/O stays on the file node. |

### Model preparation, history and tools

| Caller → owner | New API |
| --- | --- |
| Loop → PrepareModelHost | `prepare({turnId, requestId, history, availableTools}) → ModelSettings`, with pinned tool definitions in order. |
| ContextPreparation → PrepareContextHost | `prepare({request, source:{turnId, requestId, through}}, cancelled) → ModelRequest`. |
| Context accounting and compaction → ContextModelHost | `describe(model facts) → {contextWindow, outputTokens, thresholdTokens, algorithm?}`, `count(request) → tokens`, `contributions({model, fixed?, messages}) → {algorithm, fixed?, messages}`, `estimate({model, algorithm, contribution}) → tokens`. |
| Loop and ContextPreparation → CompactionStrategyHost | `describe(model facts) → {name, replacementSize}`; `compact(input, cancelled) → completed, skipped or failed` with StrategyDiagnostics. Input carries history, checkpoint, request, legal cuts, trigger, budgets, prior diagnostics and model facts. |
| Loop and ContextPreparation → HistoryRenderer | `context({branchId, through}) → ContextHistory`, `renderedContext({branchId, through}) → {context, history: HistoryEntry[]}`, `snapshot({context, retainFrom, anchor}) → RecoverySnapshot`, `streamCheckpoint({branchId, through, retainFrom, batch}) → ProjectionCheckpoint`. |
| Loop and ModelProvider → ContextPreparation | `prepare({source: ContextPrepareData, projection?}, cancelled) → ModelRequest`, `validate({requestId, request}, cancelled) → void`. Reuse the prepared projection at the original cutoff. |
| ContextPreparation and usage readers → ContextUsage | `read({through?}, cancelled) → {through, contextWindow, outputTokens, thresholdTokens, estimatedInputTokens, baselineSource}`. |
| Loop, ToolExecutor and Code Mode definition readers → `toolBinding` | `bind({requestId, through, tools, toolSources}) → void`, `bindCurrent({requestId, declared?}) → ToolDefinition[]`, `definitions({requestId}) → ToolDefinition[]`, `transfer({from, to}) → void`, `release({requestId}) → void`. ToolBinding retains the pinned handler generation. |
| Nested tool execution → `toolExecutor` | `open({parentCallId, context}, cancelled) → gateId`, `invoke({gateId, call}) → ToolResult`, `close({gateId}) → ToolEffect[]`. Close joins children in order; interruption releases the gate. |
