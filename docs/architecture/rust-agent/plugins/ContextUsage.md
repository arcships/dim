# ContextUsage

Owner: [context_usage.rs](../../../../crates/dim-agent-capabilities/src/context_usage.rs).
A branch part of the ContextPreparation feature. It owns persisted numeric context
contributions and request calibration under [ContextUsage](../../../design/plugins/ContextUsage.md).

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:Timeline` | Read source fields and cumulative numeric results; commit changed results. |
| `B:Dispatcher` | Observe publication and report owned failures. |
| `B:BranchLifecycle` | Start after activation and stop when closing. |
| `S:ContextModelHost` | Resolve the additive counter and algorithm. |
| `S:ToolRegistry` | Read current declarations when configuration does not specify tools. |

## Events and lifetime

Consumes configured, consumed-input, output-end, tool-result, completed-compaction
and provider-usage sources. Contribution reads require `context.render`; configuration
and provider measurements remain separate controls. Child execution facts are excluded
before their bodies are selected. Handlers notify its own task; it reads source identities
in pages of 128 and consumes each necessary body immediately. Each source that
changes numeric state saves one `context-usage/recovered` v5 result containing its
cumulative contribution. It writes no numeric tree nodes or request-base facts.
A `context-usage/covered` v1 extends a saved result only for an already checked
no-effect source tail at a fixed read or turn-end handoff.

Recovery reads the latest numeric result, matching covered position and counter
controls only when a kind/position probe proves there is no uncovered source matching the live render/control filter. An uncovered source invalidates the derived numbers; cold recovery never consumes its body. Old v1-v3 aggregates are read directly; old prefix numbers are read by their
existing node identities only when requested. v4 numbers remain readable. Unknown
schemas or algorithms fail. Without saved statistics, usage
reports that it is not yet initialized. A real model preparation passes its existing projection and resolved counter to
Usage, which returns initial contributions and the budget estimate from one
message count. It saves those contributions only after the final request is
confirmed to retain that projection. A compaction adopted after that projection cutoff defers initialization until the next actual request. Resume reads no historical messages to initialize a counter.
Current reads use the in-memory totals. Earlier compression cuts seek cumulative
numbers using epoch, branch ordinal and source sequence. A missing predecessor makes statistics unavailable until an actual request establishes a new total; no history recount fills that gap.

Current dynamic tools use a small version/identity/fixed-number cache; no handler
binding or extra Timeline event is required. New messages are counted once and
fixed controls are reused. The first stable projection establishes the same existing request association before invocation, so its provider measurement can calibrate the next request. Settled-output projection is shared with history.
Provider calibration uses one Weak reference to the actual numerically verified
request and one current-call slot, invalidated by incompatible controls or content.
Delivered measurements retain only their numeric basis and source positions until
the numeric task consumes them; it waits for delivery before reading that basis.
It never reloads old request bodies to establish calibration. ContextPreparation
reuses the estimate for the same final request and counter; transformed requests
receive the ordinary final full-request check.

## Tests

[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs):
`persisted_usage_reads_and_restores_without_projecting_history`,
`unconfigured_usage_does_not_project_legacy_messages`,
`provider_usage_baseline_counts_new_history_and_ignores_unusable_measurements`.

<!-- generated:plugin ContextUsage — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`S:ContextModelHost`、`S:ToolRegistry` |
| 提供 | `B:ContextUsage` |
| 被注入于 | `ContextPreparation` |
<!-- /generated -->
