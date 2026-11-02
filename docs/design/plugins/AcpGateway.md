# AcpGateway

Status: the supported first-delivery subset is implemented in [PR #441](https://github.com/arcships/dim/pull/441), unreleased; evidence and limits are in the [acceptance record](../acp-gateway-acceptance.md). Requirement: [#436](https://github.com/arcships/dim/issues/436). Scope: the client host process, outside the App Server daemon. Language: Rust. The [implementation contract](../acp-gateway-implementation.md) defines the method matrix, wiring, connection state machines and unsupported features. [中文](../../zh/design/plugins/AcpGateway.md)

`AcpGateway` is a reusable rutis plugin that serves Dim as an ACP agent. It owns the ACP connection and its independent public-RPC connection to the existing App Server. `dimcode acp` is its first host and stdio entry; another host can mount the same implementation or launch that command. The gateway has no TUI dependency. Desktop continues to use the same daemon and persisted Sessions; it needs no separate ACP protocol implementation.

Agent core, RPC dispatch, built-in Session protocol, Timeline and persistence formats are unchanged. The MCP business module registers the separate [TemporaryMcp](TemporaryMcp.md) Session part for disposable client servers; configured MCP management is unchanged. A protocol feature that existing public methods and facts cannot express is explicitly unsupported. It is not implemented by a second business state machine in the gateway. Dim calling another ACP agent remains [ExternalAgent](ExternalAgent.md), a separate direction and lifecycle.

## 1. Structure and ownership

```text
ACP client (Lody / acpx / editor)
             |
             | ACP over stdio
             v
client host's rutis tree
  host I/O and connect configuration
             |
        AcpGateway
    ACP requests / updates / Lody
    request and connection lifecycle
             |
             | existing protected local RPC v3
             v
App Server (existing RPC; MCP business parts)
  existing methods and notifications
             |
Session / InputQueue / Loop / Approval / Questions / Attachments / ...
```

| Part | Scope | Owns | Uses |
| --- | --- | --- | --- |
| AcpGateway | client host process | ACP and RPC I/O; negotiation; requests; subscriptions; replay and update projection; cleanup | public RPC v3 and the ACP schema |
| CLI entry | client host composition root | command parsing, existing daemon discovery/start, plugin mount and exit | the reusable gateway and existing client startup policy |
| TemporaryMcp | native Session, owned by the MCP feature | disposable client server bindings, tools and cleanup | existing McpServer plugins, ToolRegistry and Session lifecycle |
| Existing business owners | their current process / Session / branch scopes | execution, persistent facts, configuration, approvals, questions, goals, tasks and files | unchanged |

The Gateway itself has one client part and no native Session or branch part. It is not a Capability: it does not own a state machine on a branch Timeline. Its connection resources and in-flight operations make it a real plugin, rather than a row whose only purpose is wiring other plugins.

The gateway implementation is `crates/dim-acp`. The public RPC engine, product-event boundary, question contract and loopback callback I/O live in `crates/dim-client`, shared with the CLI. The extraction and host boundary are [plan §3](../acp-gateway-implementation.md#3-模块宿主与接线). No `AcpHost`, new daemon endpoint, exported rutis link or private `RpcService` dependency is assumed.

## 2. Protocol boundary

The wire target is ACP v1, plus individually advertised Lody Core 0.2.0 features. ACP v2 and additional transports are outside the first delivery. Sources are pinned in [plan §2](../acp-gateway-implementation.md#2-核对基线和协议清单); SDK versions do not name protocol versions.

Every stable v1 agent/client/protocol method, the selected unstable surface and the Lody methods are classified in [plan §4](../acp-gateway-implementation.md#4-稳定-acp-v1-方法逐项映射) and [§8](../acp-gateway-implementation.md#8-lody-方法和能力逐项决策). Capabilities follow implemented handlers and actual native availability, never a copied historical capability object.

The first delivery covers standard Session creation/list/load/resume, prompts and cancellation, tool output, approvals, images and text resources. Account authentication, mode/model selection, form questions, close/delete and fork use their existing native interfaces when available. The selected Lody subset is history, compaction, terminal-turn fork anchors, startup model and permission mode selection for new Sessions, detached-exec task lifecycles and mutually negotiated snapshot-only subagent events. Steering, Goal control, token accounting, quotas, subagent query/output/cancel, scheduled tasks, session activity, title ownership, worktree projects and file rollback remain outside the first delivery; the plan distinguishes native contract gaps from delivery choices.

Lody methods and metadata follow the pinned Core DTOs and their semantic comments. There is no connection-wide Lody switch or inferred support for the entire extension set. A subagent snapshot can truthfully advertise no output stream, progress, read or cancel; an old SDK task API is not a prerequisite for that minimal projection. The exact negotiation, native fact correlation, startup configuration and per-feature limits are [plan §8](../acp-gateway-implementation.md#8-lody-方法和能力逐项决策). Canonical Lody tool IDs must not be populated with arbitrary native tool names.

The gateway does not call client `fs/*` or `terminal/*`, expose arbitrary account management, or inject raw blocks. It binds stdio/HTTP/SSE `mcpServers` through TemporaryMcp on new/load/resume/fork. Configurations stay volatile, alongside native tools. An older daemon without the temporary binding interface retains accept-and-ignore behavior and advertises no HTTP/SSE MCP capability. Non-empty `additionalDirectories` fail before Session creation or attachment. Unknown non-control metadata follows ACP extensibility; recognized unsupported control metadata fails before an input is submitted.

Socket/pipe authentication is separate from model-account authentication. The gateway discovers existing `account.providers/status` and exposes executable auth methods for the managed-provider plugin. An agent-type browser method uses `authenticate` and the existing native begin/complete/status/cancel operations; a terminal method launches a separate interactive helper and must not be passed to `authenticate`. OAuth, credential exchange and persistence stay inside the provider. The exact calls, callback ownership, headless limits and cancellation contract are [plan §4.4](../acp-gateway-implementation.md#44-账户认证与现有-oauth-插件). No account plugin means no forced managed-account login for configured/API-key models.

Provider management already belongs to the daemon's ProviderPlugin, with public methods used by the CLI. The gateway reuses that owner rather than creating another provider registry. [Plan §4.5](../acp-gateway-implementation.md#45-provider-管理与现有插件) maps `providers/list` to `provider.management`, and set/disable to versioned `provider.update`, including full-header replacement and credential ownership. Whether ACP edits shared persisted settings or creates temporary routing remains a product decision; persistence alone does not prohibit the capability, and Core does not define this provider API. The combined capability remains off while that choice is unresolved. Only temporary routing lacks an existing override interface; shared management already exists.

## 3. State and lifecycle

Gateway memory contains only connection negotiation and generation, mapped Session ids, operation slots, ACP/native request correlation, submission/input/turn ids, fixed-history cursors, streamed Unicode offsets, local permission/question waiters and derived task/run presentation links. Task/run projections rebuild from committed native history; the gateway does not decide their business state. It has no persistent store. Native ids remain identities; runtime handles do not become durable ids.

[Plan §6](../acp-gateway-implementation.md#6-生命周期与连接管理) defines the complete lifecycle, including close-once disposal, task drain, queue limits, startup admission, reconnect timing, generation fencing and multi-client behavior. The host supplies owned ACP I/O and connection configuration. Cleanup is registered before I/O begins. A client tree has no Dim Session/branch publication stage; mounting, RPC hello and ACP initialize gate its own work.

- ACP EOF, signal, output failure and plugin unload stop gateway admission, release local waiters and detach/close its RPC connection. They do not stop or close Sessions or the shared daemon.
- Explicit ACP `session/close` is a business close through native `session.close`, including its effects on other clients; it is not a synonym for disconnect or detach.
- RPC loss pauses mutations and recovers through the existing endpoint and handshake. The gateway reattaches and reads facts, never blindly replays mutations.
- A disposed generation cannot register a late new/load/fork result, emit a late answer or write to the next connection.

No plugin loader, lifecycle registry or per-Session daemon relay is added. The host transfers owned I/O, a connection and its reconnect configuration through construction; the gateway reads no external service and declares `Deps = ()`. Existing CLI dependency migration in [#434 U3](https://github.com/arcships/dim/issues/434) stays separate. No `AgentBuilder` client host or raw service-key dependency is introduced.

## 4. Session and prompt flows

### 4.1 Creation, loading and replay

The gateway creates ordinary interactive Sessions (`session.create` omits `interactive`), then attaches through RPC. An existing Session keeps its stored configuration. Each Session has a single preparation/runtime slot, even across concurrent load/resume calls.

Loading fixes a native history head, reads pages using the native `nextAfter`/`done`, merges buffered live events by branch sequence and sends replay before the ACP response. Resume rebuilds the same projection without replaying old messages. Raw-only and deleted inputs do not become ordinary user messages. Title revisions and progress snapshots are handled by their own public contracts, since they have no durable event sequence. [Plan §5.2](../acp-gateway-implementation.md#52-固定头与-live-合流) owns the algorithm.

### 4.2 Prompt and cancellation

An ACP prompt becomes one Queue `session.submit` with a unique submission id. The response is determined by deletion of that input, or the terminal fact of the turn that consumed it. Other clients' turns do not decide it. Progress and committed text share per-request offsets so output is not duplicated.

Cancellation respects the handover from InputQueue to Loop: try the existing withdrawal entry while held, then use `session.stop`. The gateway cannot guarantee a stop targets one expected turn, or that an adopted but unconsumed input was deleted; these are limitations of the existing native contracts. [Plan §5](../acp-gateway-implementation.md#5-prompt回放和响应关联) and [§6.4](../acp-gateway-implementation.md#64-取消的交接边界) define response ordering and the races.

The gateway's own cancel must end the prompt with ACP `cancelled` even if cancellation exposes an underlying error. A Hooks `input_blocked` rejection from `session.submit` is shown as the message and ends with `refusal`; Hooks is not implemented by this gateway.

### 4.3 Approvals and questions

Native approval requests from every branch become ACP permission requests. Answers use `approval.respond`; current native policy and first-answer ordering remain authoritative. Resolved/cancelled notices retire local waiters. Always options are not offered.

For clients advertising form elicitation, pending `ask_user` calls become `elicitation/create`, answered through existing `session.question.respond`. Pending questions are derived from tool-call/result facts, not a new server read API. Unknown or unsupported client elicitation does not remove the native tool; the pending question remains answerable by another Dim client, and the gateway shows that state. MCP-server elicitation and ACP URL elicitation are separate, unsupported surfaces.

### 4.4 Submission uncertainty

A submission id is a lookup key, not a native exactly-once guarantee. The [native rejection contract](../../../crates/dim-app-server/README.md#session-submission-errors) decides whether a failure proves that nothing was committed. After uncertain failure, accepted lookup/history reconnects the existing prompt; an unknown lookup does not prove the old call cannot still commit. No automatic resend or fresh identity is introduced. Other mutations have the same no-blind-replay rule unless their owner explicitly provides a stronger contract.

## 5. Security and context

The gateway retains the socket ownership/permissions, authenticated Windows pipe, protocol/profile checks and frame limits of the existing CLI client. It uses an explicit native-method allowlist; ACP methods never become arbitrary RPC proxies. Outside Session operations it permits only safe provider catalog reads and the narrowly owned account-login flow in plan §4.4. Native provider-management reuse and its first-delivery write boundary are specified in §4.5. Logout, quota queries and arbitrary account management are not exposed. Provider-private realms, client IDs and token flows remain outside this repository.

ACP content enters through existing `session.submit.user`, with parsing and content conversion at the external boundary. The gateway writes no arbitrary Timeline events, raw context blocks, plugin settings or host tool registrations. The model sees only the ordinary adopted input and tool results; ACP identities and transport diagnostics are not injected into its context.

stdout is exclusively ACP. Diagnostic output must not expose credentials, account secrets, full prompts or unrelated tool output. A slow or failed output connection is closed explicitly when reliable delivery cannot be maintained; committed results and approval requests are never silently dropped.

## 6. Rule check

Rules: [principles §1 and §4](../../architecture/rust-agent/principles.md), [ADR 002 D1–D5](../../adr/002-plugin-scope.md), [plugin contract](../plugin-contract.md).

| Rule | Application |
| --- | --- |
| §1 Capability and server boundary | One client-process plugin owns actual I/O and requests; no branch state machine and no ACP business implementation in RPC; temporary MCP belongs to its native owner. |
| §4.1 Commands change another owner's state | Calls existing Session entries; owners decide and write results. The gateway projects their outcomes only. |
| §4.2 Handover is explicit | Withdrawal ends at input/applied; stopping uses the receiving owner's command. Adopted input is never pretended to be revoked. |
| §4.3 before callbacks | None; external parameter/content validation is at the ACP boundary. |
| §4.4 Mechanisms stay generic | No change to Timeline, Dispatcher, runtime, RPC framework or rutis; unsupported semantics stay unsupported. |
| §4.5 Atomic facts and order | Uses committed native facts in branch sequence order; local protocol queues do not invent a batch or execution decision. |
| §4.6 Context after handover | Only normal input submission and existing tool answers contribute context; no runtime injection. |
| §4.7 Public contracts and ownership | Reads RPC/history/notices, not other plugins' private state. No Goal, task or usage business replica. |
| §4.8 Mechanism requirements | No first-delivery requirement; deferred gaps are recorded with owners in plan §12. |
| §4.9 Necessary facts only | Defines no Timeline event; transient connection/presentation state stays in gateway memory. |
| ADR 002 D1 Scope | Connections and in-flight work span Sessions and belong to the client process. |
| ADR 002 D2 No wiring modules | The gateway owns protocol resources, requests and cleanup; host composition is direct. |
| ADR 002 D3 Dependencies | Points to existing public RPC; core mechanisms never depend on the gateway. |
| ADR 002 D4 Configuration | Host startup inputs are separate from daemon business settings; no new business source or structural management path. |
| ADR 002 D5 Language-neutral contracts | ACP and RPC are wire contracts. Rust/SDK types and local I/O ownership are implementation bindings. |

## 7. Requirements raised to mechanism owners

None for the first delivery. It does not reserve future mechanism changes as hidden prerequisites. [Plan §12](../acp-gateway-implementation.md#12-机制缺口与首版后的评估) records the unsupported semantics, why existing interfaces cannot express them, the generic question to ask and its owner. They are evaluated after first-version acceptance.

## Appendix A. Implementation and acceptance

### A.1 CLI entry

`dimcode acp [--agent ID]` uses the existing explicit home/cwd/server/model-profile options. It mounts the gateway in a client rutis tree, without Tui, UiLeaf or a daemon-side ACP part. It does not run a terminal update prompt or read stdin as a one-shot chat prompt. Stdout contains protocol only. The separate `--auth-setup METHOD_ID` helper uses the existing interactive login flow.

Zed's existing `dim acp` invocation and legacy environment configuration follow [plan §3.2](../acp-gateway-implementation.md#32-插件输入和装配顺序).

### A.2 ACP methods

The exhaustive stable, unstable and extension decisions are [plan §4](../acp-gateway-implementation.md#4-稳定-acp-v1-方法逐项映射) and [§8](../acp-gateway-implementation.md#8-lody-方法和能力逐项决策). No capability is inferred from an SDK type or the historical implementation.

### A.3 Prompt content and updates

Content mappings are in plan §4.3; event projections are in §7. The existing CLI display projection is incomplete for ACP correlation and must not substitute UI state for missing typed facts.

### A.4 Acceptance

[Plan §10](../acp-gateway-implementation.md#10-交付单元和验收顺序) defines public-flow checks and isolated client commands. The [acceptance record](../acp-gateway-acceptance.md) contains actual results: acpx basic chat and public native MCP-tool flows pass. Lody GUI MCP-tool verification is blocked by the current login screen and remains unverified. Client MCP configurations use disposable native Session bindings. Desktop UI, Dim consuming external ACP agents, resource nodes and full Lody parity are outside this delivery.

### A.5 Client labels

`session/new` records the Session's creation origin as `acp` ([Session classification](../appserver/session-classification.md)), so a picker can label it and its own "Show ACP sessions" preference can hide it. The gateway sends the origin only when the daemon advertises `session.create.origin`; it reads no connection label and keeps no private map, and a later attachment never changes the stored origin. The ordinary Session remains visible to other native clients. Earlier gateways marked nothing, and their Sessions read as `client` or `unknown`.
