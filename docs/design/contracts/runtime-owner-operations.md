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
| Context RPC → target branch | `agentLoop.compact` and `contextUsage.read`. |
| Goal and Shell admission → timelineTree | `directory({id})` returns the matching directory entry or the unknown-branch error using a single-branch lookup. Goal reads ended history through `query`; writes retain the ended/unknown errors. |
| InputQueue and Collaboration approvals → target branch | `timelineTree.input`, `inputQueue.withdraw` after submission checks, and `approval.pending/decide` on the selected branch. |
| PromptSuggestion → ContextUsage; Report and Agent configuration → Prompt | `contextUsage.read`, `prompt.adoptedConfiguration({}) → adopted configuration or null`. Report also uses `timelineTree.query` at the selected head for evidence. |
| Prompt, ToolBinding, EnvironmentClients and EnvironmentSettings → `branchLifecycle` | `registerOpening(completion) → void`. Opening waits for admitted completions under its existing deadline and propagates their errors. On a branch that opens without an opening input, the environment completions append their snapshot and wait for the branch to dispatch it, so the snapshot and the `input/applied` the queue writes for it are inside the sealed opening; a child line's identity input is written after the opening, so those completions append nothing there and the published branch's first pass appends instead. |
| Fault producers and Session observers → `sessionLifecycle` | `reportFault(error) → void`, `failure({}) → SessionFault or null`, `observeFault(listener) → id`, `release(id) → boolean`. The listener receives the current failure (initially null) and subsequent reports. Caller unload releases the observation. The first fault wins until a storage failure supersedes it; the first storage failure wins. |
| Check providers and input admission → SubmitChecks | `register(name, check) → void`; `check({entry, branchId, user?, mode}, cancelled) → proceed{blocks, warnings} or block{message}`. Run in name order through the first block. Interrupted checks write nothing. |
| Compaction, stop and activity callers → `agentLoop` | `agentLoop.compact({}) → sequence`, `stopTurn({turnId, reason}) → sequence or null`; `agentLoop.state({}) → {phase, turnId?}`, `observe(listener)` delivers that snapshot. |

### Calls from a Session to a selected branch

Within a branch, inject its owner face directly. A Session caller names the line and the
owner's face, and the owner of the line's index resolves it
([#2127](https://github.com/arcships/dim/issues/2127),
[ADR 027](../../adr/027-cross-scope-access-by-id.md)): `sessionScope.lineCall` reaches a
line of the caller's own Session, `sessions.call` a Session and a line of it for the
process, and both pass the owner's arguments, reply and error through. The two surfaces spell
the same failure differently: the `sessions` envelope answers snake_case codes, and a Session
part's scope throws `RouteError`, whose variants are camelCase and whose owner variant carries
the owner's name, message, data and reloading flag. A routed method that replies nothing answers
`null` through `sessionScope` (the bridge maps an absent value to JSON null), and the `sessions`
envelope leaves `value` out so a process caller reads `undefined`; a direct call answers nothing
(`undefined`) either way. Agents loads the
Session for the call and holds it while it runs (ADR 027 D5). A target that cannot be
selected answers `no_session`, `session_closing`, `session_reloading` (retry later),
`session_unavailable` (report the reason), `no_line` (final for that line) or `no_service`
(the selected target does not provide the service; final), before the target's operation
runs. Tree history, directory and batch input continue to use the
tree's own methods.

| Session caller | Target operation |
| --- | --- |
| Context RPC / PromptSuggestion | `contextUsage.read`, a direct call inside the Session's own face. A compaction that waits stays in the existing compact RPC handler: it takes its `Branch`, calls `AgentLoop::compact` there and waits on the branch's own `publication()`/`query`, then returns the existing completed/skipped/failed reply. These handlers are reached by the Session's own code today; routing them is written here only once `agentLoop`, `contextUsage`, `prompt` and `timelineTree` are routable faces with a wait operation, which they are not (`route::face` refuses them as internal). |
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
