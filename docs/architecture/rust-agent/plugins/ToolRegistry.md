# ToolRegistry

Owner: [tool_registry.rs](../../../../crates/dim-agent-capabilities/src/tool_registry.rs).
Keeps the Session's tools: the host tools, the tools parts register with it
and the sets of the tool sources parts register, declaring those the Session
mounts and exposing snapshots and a source-state watch for branch adoption and
pinning.

## Declarations

ToolRegistry carries declarations and owns no wording: the names, descriptions
and schemas it hands on are their owners', stored and declared unchanged
([model-facing prompts §9.3](../../../design/model-prompts.md#93-toolregistry-and-toolbinding)).

In a declaration, the description's guidance is policy; the schema, the result
format and the refusal codes are protocol
([§4](../../../design/model-prompts.md#4-protocol-and-policy)). A mixed
description is marked sentence by sentence on its owner's page. A declaration's
policy change is judged against the current text
([§8](../../../design/model-prompts.md#8-judging-a-change)); its protocol change
follows the behaviour it describes, and a schema or result-format change lands
with the flow test that asserts it.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:ToolHost` | Host definitions and execution handlers. |
| `S:SessionConfig` | The mount list (`tools`): which host and registered tools are declared, and in which order. |
| `S:SessionIdentity` | Supply the owning Session id in a foreign tool's neutral call and the `tool.attach` span. |
| `S:SessionLifecycle` | `activated()` starts the background refresh task once the Session is published; `closing()` ends it. |

## Events and lifetime

No Timeline events. The fixed table holds every host tool in the host's order.
Every other tool is registered: a part calls
`register(ctx, ToolContribution)` for a tool and `register_source(ctx, name, provider)`
for a dynamic source, from its own `apply`, and the registration is withdrawn
when that part unloads ([rutis usage R6](../../../design/rutis-usage.md#r6-contributions-register-with-their-owner)).
A part of another language calls `toolRegistry.register(declaration, execute)`
through the bridge; the registration belongs to the calling row
(`caller_row`), and `execute` is the row's function, called back through the
bridge. The registry provides that entry itself, at its dispatcher key, with
the native service: a row waiting for it starts loading as the registry becomes
active, and Session creation settles such a row before publication, so its
tools are in the first request. The host registers no names in advance and the
registry discovers nothing: there are no framework event listeners and no calls
into another runtime when the tools are read.

A tool's name follows the tool name grammar and may be neither a host tool's
nor another registration's; a declaration from another language also names its
Permission risk. `SessionConfig.tools` is the Session's optional mount list,
fixed at creation. Without one, the Session declares every host tool in the
host's order and then every registered tool by name (#364). With one, it
declares the host and registered tools the list names, in its order; `[]`
declares none of them. Either way the other sources' tools follow. A mounted
registered tool is offered as a registry-owned versioned set keyed
`#contributed:<tool>` (#241) while a part has it registered. A listed name that
nothing has registered is not declared and is reported once with a
`tool.attach` span in the process log; Session creation does not refuse it, and
the tool is declared once something registers it. Only duplicate names in the
mount list fail initialization. Permission decides whether a declared tool's
call runs and never changes what is declared, so the declarations in the prompt
prefix stay stable while Permission changes; dynamic sources follow their own
configuration, not the mount list. Source names follow the same grammar, so
their keys (`<name>` or `<name>:<instance>`) never meet the registry's own
`#contributed` keys.

A set's version changes only with its registration. Each version is a new
UUID, as MCP's are, so it is never reused, also across registry instances and
processes: a version persisted in a branch's `tool_sources` names one
generation, and a restored branch with a tool part adopts the restored
generation under a new version before its first request. Every read
(`snapshot()`, `definitions()`) takes in what is registered then. Once the
Session is published (`SessionLifecycle::activated`), the registry's task also
rereads after every registration or withdrawal, until the Session starts
closing (`SessionLifecycle::closing`), so a registration withdrawn during the
close changes no source state; unloading the registry aborts the task and waits
for a refresh already running. Source providers emit `ToolProviderChanged` under
the Session's private name `tool_provider_changes(instance)`
(`dim/tools/{instance}/provider-changed`) after updating their state; the
notification carries no data because every refresh reads every source.
Replacing or withdrawing sets updates the source-state watch (`sources_state()`),
which ToolBinding follows. ToolBinding owns adoption and request pins. Dynamic
source validation reserves the mount list's names, host tool names and every
registered tool's name; only a registered tool's own set may offer that name.
Already bound requests retain their handlers. A refresh is neither a durable
configuration save nor permission authorization; a new tool is not
model-visible until adoption, or until the next `bind_current` request of a
branch whose configuration has no tool part.

A part depends on ToolRegistry to register with it, so a reloaded registry
reloads the parts registered with it, and they register again. A native tool
keeps its own reload behavior. A foreign tool's handler holds its row's
generation token: once the row starts unloading, a call through it is not made
(the model-visible error says the provider generation is unloading and the tool
is offered again once its provider is active), and a call in flight ends with an
error warning that it may already have taken effect. Execution maps the native
call and context and the Session configuration to the neutral A.5 request, then
decodes `ToolResult` once, its effect declarations included and unchanged; the
registry does not interpret them
([session boundary A.6](../../../design/contracts/session-boundary.md#a6-tool-effects)).
Each call's cancellation Promise uses a child of the
batch token, so finishing its lease cannot cancel another call in that batch.
TypeScript effects still own settlement of the JavaScript work. The host decides
how Sessions unload and restore; App Server recovery is described in
[the process binding](../../../design/process-typescript.md#51-p6p7-access-through-declared-feature-dependencies).

Initial attachment and adoption satisfy
[Session boundary A.5](../../../design/contracts/session-boundary.md#a5-tool-contribution)
through ToolBinding initialization and the existing Prompt / BranchOpening path,
before Session publication and input admission. A branch whose configuration
has no tool part declares the registry's current host and registered tools
through PrepareModelHost / `bind_current` on each request; a registration change
needs no adoption there.

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs):
`echo_tools_and_observable_state_follow_the_public_flow`;
[local_product.rs](../../../../tests/integration/tests/local_product.rs)
exercises the real-tool registry with a mount list.
[tool_binding.rs](../../../../tests/integration/tests/tool_binding.rs) covers
immediate first requests, Pending providers, restoration, provider replacement,
request snapshots and the declarations with and without a mount list. A live
Session follows a provider's withdrawal and next registration without a request, nothing changes once the Session is closing, a
restarted registry is registered with again, and a persisted contributed version
resolves to nothing after a restore. A queued source notification whose emitter closes still
reaches branch adoption, while another Session keeps its own source version.
The real Node/Rpc flows in
[session_typescript.rs](../../../../crates/dim-app-server/tests/transport_flow/session_typescript.rs)
cover foreign registration, restore, Pending/disabled parts, a part that registers later, a refused declaration,
malformed results, retained handlers of an unloaded row, and cancellation without cancelling sibling tools in the same batch.

## Rule check

| Rule | Contract |
| --- | --- |
| §4 rule 1 | No state owned by another capability is written. |
| §4 rule 2 | ToolBinding keeps accepted request snapshots; attachment never rebinds them. |
| §4 rule 3 | No before callback is added. |
| §4 rule 4 | Timeline and Dispatcher have no tool-provider-specific behavior. |
| §4 rule 5 | Memory notifications add no Timeline batches; existing producers retain atomic commit ownership. |
| §4 rule 6 | Declarations reach model context through Prompt adoption; results use ToolExecutor. |
| §4 rule 7 | Parts register through ToolRegistry's public `register`; private provider state is not read. |
| §4 rule 8 | No infrastructure change: registration is an effect of the caller (`ctx.effect`, the bridge's `caller_row`); adoption uses `SessionLifecycle`, ToolBinding initialization and BranchOpening. |
| §4 rule 9 | Registry availability stays in memory and process logs, not new Timeline facts. |
| §1 | ToolRegistry owns its table and attachment; App Server remains assembly only. |
| ADR 002 D1 | The registry and contributions belong to one Session. |
| ADR 002 D2 | The owner keeps its table; no adapter plugin is added. |
| ADR 002 D3 | A registration follows its part's lifetime, so optional parts are followed as they register and unload; the registry depends on none of them. |
| ADR 002 D4 | Business configuration is not turned into loader reloads. |
| ADR 002 D5 | Registration is declared by the plugin in its code, in every language; ToolContribution is the Rust form. |

## Requirements raised to mechanism owners

None. Registration is [rutis usage R6](../../../design/rutis-usage.md#r6-contributions-register-with-their-owner),
owned by Dim and built on existing rutis effects and the bridge's calling row.

<!-- generated:plugin ToolRegistry — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionConfig`、`S:SessionIdentity`、`S:SessionLifecycle`、`S:ToolHost` |
| 提供 | `S:ToolRegistry`、`S:ToolSourcesView`、`S:dyn HostDispatch#toolRegistry` |
| 被注入于 | `BranchTool`、`HooksBranch`、`Questions`、`Recall`、`Skills`、`ToolBinding`、`Wait` |
<!-- /generated -->
