# 002: Plugin scope assignment and configuration placement

Decision status: Proposed. Applies to the plugin designs in `docs/design/` and to future plugins.

## Context

Dim assembles plugins at three scopes: the process, a Session, and a branch
([single-tree](../architecture/rust-agent/single-tree.md)). A plugin's scope decides which services
it can depend on, which events it can observe, which facts it can write and how long it lives.

An earlier revision of the plugin designs moved most new features to the process scope to reduce
the number of capabilities. Those features still had to offer tools to a Session, observe a
Session's events and write a branch's inputs, so the designs added wiring to reach back into
Sessions: an extension registry, registration ports, a Session access host, a process-level
Session event subscription, a process-level tool contribution, admission records and a
management aggregation plugin. Each piece existed only to connect plugins placed at the wrong
scope.

Plugin reloads are rare: they follow structural operations such as installing, upgrading,
enabling or disabling a plugin, or a crash. Changing business settings must not reload anything.

## Decisions

### D1. Scope follows the owner of the state and its lifecycle

| Scope | Holds | Existing examples |
| --- | --- | --- |
| Process | Resources and state shared across Sessions or outliving them: connections, catalogs, stores, external entries, work that spans Sessions | Provider, Mcp, SkillsCatalog, SessionStore, Transport |
| Session | Configuration and capabilities shared by a Session's branches: tools and prompt sources offered to the Session, observation of the Session's events, the Session's own data | ToolRegistry, Skills, WorkspaceTools, TimelineTree |
| Branch | Facts written to the branch Timeline, ordered consumption of branch events, work bound to a turn | Loop, InputQueue, Approval, BackgroundExec |

A feature is split across scopes only where its parts genuinely have different owners, and every
part has exactly one scope. A feature with no state shared across Sessions is not a process plugin.

### D2. No wiring modules

No plugin exists only to connect, aggregate or adapt other plugins (registries, ports, access
hosts, aggregation or adapter plugins). Plugins integrate through existing mechanisms at their
own scope:

| Need | Mechanism |
| --- | --- |
| Offer tools to a Session | Session-scoped `toolContribution` (named tool service) or `SessionToolSource` |
| Offer instructions | PromptRegistry prompt sources |
| Observe a Session's events | the existing Session broadcast, from a Session-scoped plugin |
| Observe and act on a branch's events in order | the branch Dispatcher, from a branch-scoped plugin |
| Put content into the model context | Session or branch input (adopted inputs), tool results |
| Act on a write before it is committed | a Timeline before callback registered by a branch-scoped plugin: it synchronously transforms the draft inside the commit lock, never blocks the write, never waits for external work, and the original draft commits on failure ([principles §4 rule 3](../architecture/rust-agent/principles.md)) |
| Withdraw an input before it is applied | the `input/withdraw {input_id, reason?}` command event (clients or process callers); InputQueue consumes it in order and writes `input/deleted {input_id}` only while it holds the input unapplied |
| Expose a client method about a Session | a Session-scoped `sessionMethods` service that Rpc dispatches through the Session |
| Expose a host management method | a process plugin's `managementMethods` service in Rpc's method table; connections from paired devices are never served management methods |
| Reach other Sessions | Agents' `sessions` service (AgentIndex and the Session public interface), as Collaboration does |
| React to another process plugin's outcomes | an event notice, not a dependency |

### D3. Dependencies follow scope and point from extensions to the core

A plugin depends on plugins at its own scope or an enclosing scope, and dependencies point from
extensions to the core. A core part (for example Session assembly, ToolRegistry, Rpc, and content
owners such as Mcp and SkillsCatalog) never takes a hard dependency on an optional plugin: it looks
the optional provider up on each call, or attaches the providers active when it loads (later ones
on its next reload). An optional plugin that is
missing, Pending, Failed or reloading therefore affects only its own features (#34). Within this
rule, because reloads are rare and structural, a reload propagating to its real dependents (and
failing their in-flight turns) is an accepted cost; no mechanism is added to prevent it. A runtime
crash is an infrastructure failure, like an App Server crash, and nothing is designed around it.

### D4. Structural configuration and business configuration are separate

| Kind | Examples | Location | On change |
| --- | --- | --- | --- |
| Structural plugin configuration | enable or disable, runtime and package paths, node binding | loader rows (issue #3 editable layer) | may reload the row |
| Business configuration | search services, notification channels, hook rules, scheduled jobs, workflow definitions, each consumer's auxiliary model choice, MCP server and Skill documents, external agent entries and bridges, gateway listener settings, OCR component path, exec shell | the owning plugin's business source (Settings source or the plugin's own store) | data update plus notification; read at use time; no reload |

### D5. Contracts are language-neutral

rutis calls plugins and services across languages by name (rutis-bridge runtimes, `host_key(name)`
with `dyn HostDispatch`, `TypeKey::named` and `TypeKey::dynamic`), and Rust bindings can be
generated from a named contract. A contract written only as a Rust trait keyed by
`TypeKey::of<T>()` cannot be satisfied by a TypeScript or Python plugin without a hand-written
Rust binding. Every contract the plugin designs define or use is therefore specified in a
language-neutral form:

| Element | Form |
| --- | --- |
| Identity | Service name (camelCase string), scope (process, Session or branch) and instance key (namespace, tool name, kind) |
| Methods | Name, `sync` or `async`, request and reply as serializable data (field tables or JSON shapes) |
| Errors | Data: `{code, message}` with documented codes |
| Cancellation | A protocol action: a cancel request, or the consumer dropping the call |
| Registration | Declared by the plugin (its `provides` and loader row), not by host-side `AgentBuilder` calls; consumers collect by service name and scope |
| Rust specifics | Trait signatures, `TypeKey`, `Contributions<T>` and `AgentBuilder` calls are one binding, listed in a "Rust binding" appendix |

Canonical services: `sessionMethods` (Session, per namespace), `managementMethods` (process, per
namespace), `toolContribution` (Session, per tool name), `sessions` (process) and `modelKind`
(process, per kind). They are defined in the [contracts](../design/contracts/session-boundary.md)
and the owning plugin documents. The runtime's existing typed services (Timeline, Dispatcher,
ToolHost and others) are unchanged; exposing them by name to non-Rust capability parts is the work
of issue #4.

## Assignment of the new plugins

| Plugin | Scope | Reason |
| --- | --- | --- |
| Questions | Session (`ask_user` tool, `session.question` methods and the waiters of running `ask_user` calls; no events) | the waiter is the Session-scoped tool handler's own call; branch lifecycle reaches it through ToolExecutor's cancel token |
| Goal | Session (`goal_update` tool) + branch (goal state, continuation) | a goal belongs to a branch and continuation follows its events in order |
| ShellInput | Session | commands run in the Session's workspace and feed its input |
| Attachments | Session | attachments are Session data |
| Web | Session | no state shared across Sessions; it only offers tools |
| SessionTitle | Session | observes the adoption (`input/applied`) of the first user input on main; the title is stored by SessionStore |
| Notifications | Session | observes the Session's events and offers `notify` |
| Hooks | Session (hook documents, runner) + branch (tool advice to Permission, answers to Approval, prompt holds with InputQueue; post-tool, Stop and Session-start hooks) | rules apply to a Session; execution happens in a branch |
| Scheduler | process (jobs, triggers) + Session (`schedule` tool) | schedules span Sessions |
| Workflow | process (runs) + Session (`workflow` tool) | a run spans Sessions |
| ExternalAgent | process | external agent connections are shared |
| Modality | process | tasks and provider connections are shared |
| PluginManager | process | installation is per host |
| RemoteGateway | process | an external entry |
| AcpGateway | CLI process | a client |

Management is not a plugin: management methods live in Rpc's method table, and connections from
paired devices are never served them.

## Withdrawn

The extension registry contract, registration ports and their host plugins, the Session access
host, the process-level Session event subscription, the process-level tool contribution, admission
records (`RequiredCheck`) and the Management plugin are withdrawn.

## Existing plugins

Existing plugins follow D1. HostPlatform follows D4 by reading the exec shell's business
configuration through `exec_shell()` for each command; changing it does not reload plugins.

## Consequences

- The design documents are revised to this assignment and to the neutral contract form (D5): the overview, the contracts (only those
  describing supplements of existing plugins remain) and every plugin document.
- Optional Session and branch plugins are installed only when enabled; they are not placeholders
  in every assembly.
- Rare structural operations may fail in-flight turns; this is documented, not prevented.
