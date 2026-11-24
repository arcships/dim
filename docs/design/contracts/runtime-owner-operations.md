# Session and branch owner operations

## Retained operations

| Caller → owner | Final API |
| --- | --- |
| Branch management, input and history callers → `timelineTree` | `create`, `end`, `set_default`, `directory`, `line` and `read` on the single Session tree face; creation returns the line id ([TimelineTree](https://github.com/arcships/dim/pull/2092), #2024). `query` is not the tree's: a stored Session's history is `store.query` on the process face ([SessionStore](https://github.com/arcships/dim/pull/2091), #2018). |
| Language readers → PromptSources | `sessionLanguage.get`. |
| Language updates → App Server | [`session.language.update`](session-access.md) accepts the existing language code or clear action. The owner serializes persistence, live-state replacement and Prompt adoption on main and loaded branches before replying. |
| Prompt → PromptRegistry; guide and tool contributors → their registries | `initialPromptConfiguration.get`, `promptHost.load`, `guideEntries.register`, `toolRegistry.register`, `toolSourcesView.get/observe`. |
| Session and branch parts → their lifecycle; branch fact consumers → Dispatcher | `sessionScope.activated/closing/hold/release` and its fault operations ([SessionScope](../../../docs/design/plugins/SessionScope.md), #2093); `branchLifecycle.activated/closing`, `dispatcher.listen/consume/committed`. The old `sessionLifecycle` name is superseded. |

## New rutis APIs on existing owners

### Admission and branch control

| Caller → owner | New API |
| --- | --- |
| Branch directory readers → `timelineTree` | `timelineTree.directory({}) → {default, lines}` ([TimelineTree](https://github.com/arcships/dim/pull/2092), #2024). |
| Skills content and MCP publication → Session mounts | `sessionScope.mount({group, definitionId, instanceData}) → {instanceId}`; `unmount({instanceId})`. The owner admits groups whose configured parent is the Session group, waits for the definition's rows and settles a rejected instance. Skills supplies its content definition; MCP supplies its connection definition ID and retains the resolved transport configuration ([SessionScope](../../../docs/design/plugins/SessionScope.md), #2093). |
| Goal → the target line's input owner | `line_owner::<InputQueue>(line).submit(...)`; one validated atomic batch for the Goal record and trigger ([ADR 027](https://github.com/arcships/dim/pull/2029)). |
| Branch-detail RPC → timelineTree | `line({line})` for the record and `read({line, …})` for the facts; there is no `display` operation. |
| History RPC and line-read tools → timelineTree | `read({line, through, …})` at one cutoff, with the complete batches and the fixed head of an ended line; the reply carries the events or the selected fields. |
| Context RPC → target branch | `line_owner::<AgentLoop>(line).compact(...)` and `line_owner::<ContextUsage>(line).read(...)` ([ADR 027](https://github.com/arcships/dim/pull/2029)). |
| Goal and Shell admission → timelineTree | `line({line})` returns the matching record or `unknown` ([TimelineTree](https://github.com/arcships/dim/pull/2092)). Goal reads ended history through `read` at the line's fixed head; writes retain the ended/unknown errors. |
| InputQueue and Collaboration approvals → target branch | `line_owner::<InputQueue>(line)`: `submit(...)` and `withdraw(...)` after submission checks; `line_owner::<Approval>(line)`: `pending` and `decide`. |
| PromptSuggestion → ContextUsage; Report and Agent configuration → Prompt | `line_owner::<ContextUsage>(line).read(...)`, `line_owner::<Prompt>(line).adoptedConfiguration(...)`. Report's own tree reads are [TimelineTree's](https://github.com/arcships/dim/pull/2092), not this row's. |
| Prompt and ToolBinding → `branchLifecycle` | `registerOpening(completion) → void`. Opening waits for admitted completions under its existing deadline and propagates their errors. |
| Fault producers and Session observers → `sessionScope` | `reportFault(fault) → void`, `failure() → SessionFault or none`, `observeFault(listener) → id`, `release({leaseId}) → boolean`. The listener receives the current failure (none included) and later reports; the calling row's unload releases the observation. The first fault wins until a storage failure supersedes it ([SessionScope](../../../docs/design/plugins/SessionScope.md), #2093). |
| Check providers and input admission → SubmitChecks | `register(name, check) → void`; `check({entry, branchId, user?, mode}, cancelled) → proceed{blocks, warnings} or block{message}`. Run in name order through the first block. Interrupted checks write nothing. |
| Compaction, stop and activity callers → `agentLoop` | `agentLoop.compact({}) → sequence`, `stopTurn({turnId, reason}) → sequence or null`; `agentLoop.state({}) → {phase, turnId?}`, `observe(listener)` delivers that snapshot. |

### Calls from a Session to a selected branch

Within a branch, inject its owner face directly. A Session part reaches one line's owners by
line id: `line_owner::<T>(line)?.op(..)` resolves the branch-group service at the instance the
runtime built for that line, and the caller calls that owner's typed operation. Any branch-group
data face is reachable this way except registration points and lifecycle services; there is no
generic operation-name dispatch and no `timelineTree.call`. A line that is not live answers
`NoLine`; a Session that is closing answers `SessionClosing`. `NoLine` is final for that line:
the caller takes no hold and does not retry it. `SessionClosing` ends the Session part's work. A
Session part never receives `NoSession`: only a process caller does, and it takes a hold from Agents
before calling again ([#2127](https://github.com/arcships/dim/issues/2127)).

Report's draft head, its bounded conversation window and the cutoff it keeps are stated by the
[TimelineTree contract](https://github.com/arcships/dim/pull/2092) and Report's own contract; this document keeps neither.

### Prompt, configuration and files

| Caller → owner | New API |
| --- | --- |
| ToolBinding, Session language and Agent configuration → `prompt` | `prompt.initialCompletion({}) → void or opening error`; `prompt.configure({sources, model?}) → unchanged, adopted or superseded`. |
| Prompt and guide adoption → `contextPreparation` | `contextPreparation.publish({adopted, changes, model?, toolAdoption?, inputs}) → sequence or null`; `awaitPublished({sequence}) → sequence`. Publication commits adoption atomically. |
| File read/edit/write tools → branch FileReads | `binding({path}) → TextFileBinding or null`, `writeBinding({path}) → absent, compacted or bound`, `record({path, binding, established}) → void`. The original read supplies the established sequence; file I/O stays on the file node. |

### Model preparation, history and tools

| Caller → owner | New API |
| --- | --- |
| Loop → `requestSettings` | `settings() → {providerId, modelId, capabilitySnapshot?, config?, system?, metadata?}`; Loop adds the pinned tools, the guide's system and the turn's ids ([Request settings](../../../docs/design/plugins/RequestSettings.md), #2093). |
| ContextPreparation → no host port | The request transformation has no owner and leaves the catalog; a content change enters only through adopted inputs or tool results (#2093). |
| Context accounting and compaction → `contextFacts` | `describe({model})`, `count({request})`, `contributions({model, fixed?, messages}) → {algorithm, fixed?, messages}`, `estimate({model, algorithm, contribution})` ([Context facts](../../../docs/design/plugins/ContextFacts.md), #2093). |
| Loop and ContextPreparation → `summaryStrategy` | `describe({model}) → {name, replacementSize}`; `compact(input, cancelled) → completed, skipped or failed` with diagnostics. The input carries history, checkpoint, request, legal cuts, trigger, budgets and prior diagnostics, and no model object ([Summary strategy](../../../docs/design/plugins/SummaryStrategy.md), #2093). |
| Loop and ContextPreparation → `contextProjection` | `renderedStream({through, batch})`, `renderedContext({through})`, `snapshot({context, retainFrom, anchor})`, `streamCheckpoint({through, retainFrom, batch})`, `snapshotFromCheckpoint({checkpoint, anchor})`; the fold is the branch's own ([Context projection](../../../docs/design/plugins/ContextProjection.md), #2093). |
| Loop and ModelProvider → ContextPreparation | `prepare({source: ContextPrepareData, projection?}, cancelled) → ModelRequest`, `validate({requestId, request}, cancelled) → void`. Reuse the prepared projection at the original cutoff. |
| ContextPreparation and usage readers → ContextUsage | `read({through?}, cancelled) → {through, contextWindow, outputTokens, thresholdTokens, estimatedInputTokens, baselineSource}`. |
| Loop, ToolExecutor and Code Mode definition readers → `toolBinding` | `bind({requestId, through, tools, toolSources}) → void`, `bindCurrent({requestId, declared?}) → ToolDefinition[]`, `definitions({requestId}) → ToolDefinition[]`, `transfer({from, to}) → void`, `release({requestId}) → void`. ToolBinding retains the pinned handler generation. |
| Nested tool execution → `toolExecutor` | `open({parentCallId, context}, cancelled) → gateId`, `invoke({gateId, call}) → ToolResult`, `close({gateId}) → ToolEffect[]`. Close joins children in order; interruption releases the gate. |
