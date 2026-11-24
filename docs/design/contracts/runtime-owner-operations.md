# Session and branch owner operations

## Retained operations

| Caller → owner | Final API |
| --- | --- |
| Line directory, lifecycle and history callers → `timelineTree` | 本节由 [TimelineTree 契约](../../architecture/rust-agent/plugins/TimelineTree.md#own-contract) 取代。 |
| Language readers → PromptSources | `sessionLanguage.get`. |
| Language updates → App Server | [`session.language.update`](session-access.md) accepts the existing language code or clear action. The owner serializes persistence, live-state replacement and Prompt adoption on main and loaded branches before replying. |
| Prompt → PromptRegistry; guide and tool contributors → their registries | `initialPromptConfiguration.get`, `promptHost.load`, `guideEntries.register`, `toolRegistry.register`, `toolSourcesView.get/observe`. |
| Session and branch parts → their lifecycle; branch fact consumers → Dispatcher | `sessionLifecycle.activated/closing/hold/release`, `branchLifecycle.activated/closing`, `dispatcher.listen/consume/committed`. |

## New rutis APIs on existing owners

### Admission and branch control

| Caller → owner | New API |
| --- | --- |
| Skills content and MCP publication → Session loader | `sessionLoader.create({group, definitionId, instanceData}) → {instanceId}`; `remove({instanceId}) → void`. The Session loader admits groups whose configured parent is the Session group. Registered Session extension definitions must activate their instance rows. Host-added optional groups retain their optional rows. Skills supplies its content definition; MCP supplies its connection definition ID and retains the resolved transport configuration. |
| Context RPC → target branch | `agentLoop.compact` and `contextUsage.read`. |
| InputQueue → target line | 本节由 [TimelineTree 契约](../../architecture/rust-agent/plugins/TimelineTree.md#session-parts-reaching-a-lines-owners) 取代。 |
| Collaboration approvals → target line | `approval.pending` returns the target's pending requests; `approval.decide` settles one. Both are that owner's own operations, reached by line id. Collaboration keeps its ended answer on closed, ended or unknown detail lookup. |
| PromptSuggestion → ContextUsage | `contextUsage.read`, on the target line's own face. |
| Report and Agent configuration → Prompt | `prompt.adoptedConfiguration`; Report reads its evidence through the tree's `read`, whose contract is [TimelineTree](../../architecture/rust-agent/plugins/TimelineTree.md#own-contract). |
| Prompt and ToolBinding → `branchLifecycle` | `registerOpening(completion) → void`. Opening waits for admitted completions under its existing deadline and propagates their errors. |
| Fault producers and Session observers → `sessionLifecycle` | `reportFault(error) → void`, `failure({}) → SessionFault or null`, `observeFault(listener) → id`, `release(id) → boolean`. The listener receives the current failure (initially null) and subsequent reports. Caller unload releases the observation. The first fault wins until a storage failure supersedes it; the first storage failure wins. |
| Check providers and input admission → SubmitChecks | `register(name, check) → void`; `check({entry, branchId, user?, mode}, cancelled) → proceed{blocks, warnings} or block{message}`. Run in name order through the first block. Interrupted checks write nothing. |
| Compaction, stop and activity callers → `agentLoop` | `agentLoop.compact({}) → sequence`, `stopTurn({turnId, reason}) → sequence or null`; `agentLoop.state({}) → {phase, turnId?}`, `observe(listener)` delivers that snapshot. |

### Calls from a Session to a selected branch

本节由 [TimelineTree 契约](../../architecture/rust-agent/plugins/TimelineTree.md#session-parts-reaching-a-lines-owners) 取代。

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
