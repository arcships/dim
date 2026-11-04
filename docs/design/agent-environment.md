# Agent environment awareness

Status: Draft. Issues: [#1018](https://github.com/arcships/dim/issues/1018) (this design), [#1019](https://github.com/arcships/dim/issues/1019) (clients), [#1020](https://github.com/arcships/dim/issues/1020) (settings), [#1021](https://github.com/arcships/dim/issues/1021) (line view, deferred) and [#1022](https://github.com/arcships/dim/issues/1022) (settings write path). Scope: process, Session and branch. Rules: [design rules](../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../adr/002-plugin-scope.md), [model-facing prompts](model-prompts.md).

This document owns the pattern by which facts about the agent's environment reach the model, the list of facts and their owners, the noise budget and the boundaries. Each fact's contract belongs to its own design: [EnvironmentClients](plugins/EnvironmentClients.md), [EnvironmentSettings](plugins/EnvironmentSettings.md), [ClientView](plugins/ClientView.md) and [AgentSettings](plugins/AgentSettings.md). It does not restate their block wording or host APIs.

## 1. Problem and goal

The model reads the working directory, home, shell, platform, time zone, date and Skills list as `<dim-state>` blocks ([PromptSources](plugins/PromptSources.md)). It does not know:

- who is attached to the Session, through which channel, when they came or left;
- whether the operator is still watching, or which line they are on;
- which settings that change its behaviour have changed;
- what the connected clients can render or answer.

A Session can be attached by several clients at once, can outlive every client, and can have its settings changed from another client or another process. The model's behaviour depends on those facts: whether to ask, what output the operator can see, what boundary a command will run under, and whether to wait for an answer. Today it works from stale assumptions.

The goal is one reusable pattern: an owner publishes a fact as a prompt source, the model reads it as state, and a branch adopts changes without a new context path.

## 2. Facts and their owners

| Fact | Owner | Scope | Delivery | Status |
| --- | --- | --- | --- | --- |
| Attached clients | App Server connection and UI registry | Session | `dim/environment/clients` state | Designed in [EnvironmentClients](plugins/EnvironmentClients.md) |
| Client view (line) | Clients report; App Server stores | Session | management/UI first; model deferred | Designed in [ClientView](plugins/ClientView.md) |
| Execution sandbox and network | App Server `ExecutionSettings` | Session/process | `dim/settings/execution` state | Designed in [EnvironmentSettings](plugins/EnvironmentSettings.md) |
| Reply language | PromptSources and the client that owns the setting | Session | `dim/language` | Designed in [PromptSources §3](plugins/PromptSources.md#3-the-agent-prompt-language-state-source); the value carrier is missing |
| Exec shell | PromptSources | Session | `dim/environment/context` | Existing |
| Working directory, home, platform, time zone | PromptSources | Session | `dim/environment/context` | Existing |
| Date | PromptSources | Session | `dim/environment/date` | Existing |
| Skills | Skills | Session | `dim/skills/list` | Existing |
| Tools, MCP servers | ToolRegistry and their owners | Session | tool declarations and adopted configuration | Existing |
| Model, effort, fast mode | Adopted configuration | Branch | `context/configured` | Existing; not duplicated |
| Permission preset | Permission | Branch | none | Branch-scoped; see §4 and §15 |
| Client capabilities | Clients declare | Session | not designed | Candidate for a later issue |
| Agent-writable settings | Each settings owner | Process/Session | tool or CLI, not state | Designed in [AgentSettings](plugins/AgentSettings.md) |

A fact enters this list only when the model can act on it. A setting or a client detail the model cannot use is not a model-facing fact.

## 3. Delivery pattern

Each fact is delivered the way Skills already delivers its list ([Skills §Session](../architecture/rust-agent/plugins/Skills.md)):

1. The **owner** registers one prompt source with the Session's `PromptRegistry` from a Session part's `apply`; the registration ends with that part.
2. Prompt turns a `State` source into one `<dim-state source="…">` Append input. The body is final text produced by the owner, escaped by the owner. The source declares its `order`.
3. A **branch part** checks the owner periodically while no turn is open and asks Prompt to adopt the source through `ConfigureControl::configure(targets([SOURCE]))`. A change during a turn waits for the turn to end.
4. The branch's Prompt loads the source, compares it with the adopted entry, and writes a state input only when the body changed.

Consequences:

- **No new channel and no new context path.** The block is an adopted input; system stays static; the prompt cache prefix does not move.
- **No Timeline event.** Presence, settings and view are process or Session state; they do not enter the Timeline (rule 9). The model-facing projection is an input.
- **No new marker.** `<dim-state>` is Prompt's marker and its input guide entry is Prompt's. A fact that reuses a state source contributes no guide entry. A fact that needs its own marker would need a guide entry from its writer and a separate decision.
- **Old blocks remain until compaction.** A state block is a user-role message in history; a later block with the same source replaces it semantically, but the old one stays in the transcript until the next compaction. The model is told this by the guide. The owner must therefore keep the number of changes small (§6).
- **Latest key survives compaction.** After a compaction, the keyed baseline replays the latest block of each live key; the summary carries the history ([Compaction requirements](compaction-quality.md)).

## 4. Scope

Current prompt sources are Session-wide in practice. `targets()` fixes `PromptScope::Branch { timeline: "main" }`; a Session part registers the source, and each branch's adoption writes a local override in that branch's own history ([prompt_registry.rs](../../crates/dim-agent-capabilities/src/prompt_registry.rs)). A child branch does not see later changes in `main` unless its own branch part adopts them, which is why every branch runs the watching part.

- Session-wide facts (clients, execution settings, language, shell, date, Skills) fit this pattern.
- A **branch-scoped** fact, such as the permission preset, does not: the source loader is a Session part and cannot read a branch's state, and `targets()` cannot address a different branch's scope. Version 1 does not pretend `main`'s value is every branch's value. It either leaves the fact out or raises a branch-scoped source requirement to Prompt/PromptSources (§12, §15).
- A branch that opens later adopts the current value at its open; a branch that is already open adopts on its own trigger.
- A closing branch adopts nothing; its writers withdraw.

## 5. Refresh and ordering

| Fact | Trigger | Boundary |
| --- | --- | --- |
| Clients | a check while no turn is open; branch open | One snapshot per change, before the next input; not per attach/detach |
| Settings | a check while no turn is open; branch open | One snapshot per change, before the next input; coalesce |
| Client view | operator input; branch open | Only if promoted to the model; a boolean, not per-switch |
| Existing environment, date, instructions | PromptSources' check, including [#986](https://github.com/arcships/dim/issues/986)'s request-boundary direction | Unchanged |

- **No wake.** A changed fact does not start a turn. The block is consumed with the next request; an idle branch stays idle.
- **The check task owns the work.** A per-branch task reads the owner while no turn is open and calls `configure`; no event handler waits for the adoption (rule 3).
- **One-request lag is possible.** Adoption is asynchronous and runs only while no turn is open. A change during a turn waits for the turn to end; a client that changes the fact and immediately sends can miss the first request. The design accepts that lag; a request-boundary check remains the direction of #986. For safety-relevant facts such as the sandbox, the state is advisory: the process boundary and the tools enforce the real rule.
- **Coalescing.** The task remembers the last adopted snapshot, so several changes before the next check collapse into one read of the latest snapshot.

## 6. Noise budget

The failure mode of this design is environment chatter: many small state blocks in the transcript, multiplied by the number of open lines.

- **A change is not automatically a block.** It becomes a block only when a model-relevant value changes and the branch adopts it. Attach, detach, view switch and no-op settings writes are not each a block.
- **Bound bodies.** Presence is counts by channel, not a client roster. Settings are the effective values the model acts on, not a document dump. No labels, stable ids, attach times, revisions, timestamps or "last departure" in the model body.
- **Canonical text.** Sort channels and fields; the same situation must produce the same body, so an unchanged value produces no adoption.
- **Settings compare the model-facing subset.** A Settings source may hold fields the model does not use; changing them must not rewrite the block.
- **Per-branch multiplication is expected.** Every open line has its own branch part and its own history. The design reduces the number of changes, not the number of branches.
- **A fact with no demonstrated use is not included.** Client view is the first example: it stays out of the model until a concrete behaviour and an evaluation show value. Client capabilities wait for a design.

## 7. Text and trust

- The block is **state**: the situation, not a request. It grants no permission and changes no approval. The input guide's `<dim-state>` entry already says this.
- The body is the owner's final text. External content (a client name, if ever included) is escaped with the context-block rule; a body must not be able to forge a marker.
- The class is **fact**, not policy: it states what is true now. Guidance such as "ask only when a client is attached" is policy, belongs in the base prompt or a capability's own policy text, and is judged as [model-facing prompts §8](model-prompts.md#8-judging-a-change) requires. This design adds no such guidance.
- Protocol text (source names, attribute names, the meaning of `cleared`) changes with the behaviour and its flow test; the source names here are protocol.

## 8. Compaction and restore

- Before compaction, every adopted block stays in history; the model is told the latest block of a source is the one in force.
- A compaction replaces the range with the summary and the keyed baseline; the latest block of each live key is replayed after the summary. A live state block is therefore not a saving: the compaction accounting excludes it from the removed content. The state blocks are not duplicated into the running-turn pin.
- A manual compaction reads only the branch's published sequence; a concurrent state adoption can advance the publication first, and the command waits for the next publication instead of reading a future entry.
- A restored branch loads its sources at `branch/resume` and adopts differences, so it reads the current presence and settings rather than replaying an old block as "now".
- A source that is unavailable at restore produces no new entry and does not write `cleared="true"`; the previously adopted entry stays as the last known value unless the owner says it no longer applies.
- A setting that the operator genuinely removed may write `cleared="true"`; that is a business statement, not an error path.

## 9. Absence and errors

| Situation | Result |
| --- | --- |
| The provider is not installed (native/direct CLI, evaluation, a profile without App Server) | The feature is not installed or registers no source; no block; no error |
| The fact is empty (no client attached) | A body that states it: `No client is attached.` |
| The owner cannot read the fact | Keep the adopted value; log; write no block |
| The source no longer applies (a setting removed) | The owner returns no entry; Prompt writes `cleared="true"` for the key |
| The branch is closing | Adopt nothing; no write after close |

`cleared="true"` means "this state no longer applies", never "the provider is missing" and never "the read failed".

## 10. Identity and privacy

- Client labels and connection ids are **runtime handles**, not durable identities. They may be used inside the process for management, but the model body uses only channel counts.
- No token, device id, device name, secret, `writeOnly` field or file content reaches the model, a log entry or a block.
- The remote channel is reported as `remote`, without the device identity. This changes the "no model-visible effect" sentence in [RemoteGateway](plugins/RemoteGateway.md); its owner updates that document.
- A client name is user-controlled; if a later design includes one, it is escaped and length-bounded. Version 1 does not include names.
- The facts are per Session; one Session's clients are not visible to another Session.

## 11. Non-interactive Sessions

A Scheduler or Workflow Session has no client. A Collaboration member may have no client but a leader that answers its questions and approvals. "No client attached" is not the same as "no counterpart".

- Presence states the client facts only. It does not change `SessionConfig.interactive` or the behaviour of Questions, Approval or Permission.
- The design must avoid making the model conclude that no one can answer a member. The non-interactive wording is an open decision in [EnvironmentClients](plugins/EnvironmentClients.md); version 1 may write only the client count and leave the counterpart to the Collaboration markers.

## 12. Requirements raised to other owners

The format follows [design rules §4 rule 8](../architecture/rust-agent/principles.md).

### 12.1 App Server: the `sessionClientsHost` host

**Owner:** App Server (Connection, UiHost, Rpc).

- Provide a process read-only service that returns, for one Session, the number of attached clients by channel. It is computed from the existing connection registry and UI registry; no second registry, no attach/detach bookkeeping.
- Keep the protocol `hello.clientKind` and `hello.clientName` on the connection; map the channel; a UI leaf's connection is `tui`, not `cli`.
- Keep the device identity out of the projection.
- The exact contract is in [EnvironmentClients](plugins/EnvironmentClients.md).

### 12.2 RemoteGateway: the remote channel is visible

**Owner:** RemoteGateway.

- Its current document says the model never sees device ids, names, tokens, entry names or diagnostics. Reporting a `remote` channel count does not expose any of those. The owner updates that sentence and confirms the channel label.

### 12.3 Prompt and PromptSources: adoption and scope

**Owner:** Prompt and PromptSources.

- A branch part can adopt a registered state source on its own trigger through the existing `ConfigureControl`; no new mechanism is required.
- If a model-facing fact must differ per branch (permission preset, per-branch model), this design needs a branch-scoped source route: a branch part registers a source for its own branch scope, and the Session's registry can hold per-branch names. That is a separate requirement and decision; version 1 does not fake it.
- The request-boundary freshness of #986 applies to facts that are cheap and in-memory; the owner decides the mechanism.

### 12.4 Clients: line view

**Owner:** CLI/TUI, Web, Desktop and AcpGateway clients.

- A client may report the line it views with the additive method in [ClientView](plugins/ClientView.md). Version 1 uses it for management/UI only; the model body remains a boolean on an operator input if the fact is later promoted.

### 12.5 Settings owners

**Owner:** each settings owner.

- Declare whether its setting is model-facing, and if so expose the effective value through its own state source with the owner's wording.
- Declare whether the setting is agent-writable; the write path is [AgentSettings](plugins/AgentSettings.md). Security-boundary settings default to not writable.

## 13. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Plugins and capabilities bounded | Each fact is a part of its owner's feature; the App Server host is process state; no wiring module. |
| §4 rule 1 Command events | The design writes no other module's state; settings writes through the owner are in [AgentSettings](plugins/AgentSettings.md). |
| §4 rule 2 Handover | No new handover; presence and settings do not control a turn. |
| §4 rule 3 Before callbacks | No `before` callback; event handlers register tasks and return. |
| §4 rule 4 Core carries no business meaning | The host returns generic counts; the source renders the owner's fact; no core branch. |
| §4 rule 5 Batch | No new facts; a state block is one adopted input, not a batch. |
| §4 rule 6 Context from consumed events | The block reaches the model only as an adopted input through Prompt; no second context path. |
| §4 rule 7 Public events and services | The new `sessionClientsHost` service is public and named; the facts use existing prompt sources and `ConfigureControl`. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. The App Server host and the capability parts are plugin-level. If a branch-scoped source is needed, it is raised to Prompt/PromptSources, not implemented here. |
| §4 rule 9 Timeline holds only decision facts | Presence, settings and view do not enter the Timeline; they are process/Session state. |
| §4 rule 10 Adopted inputs | The model reads only `<dim-state>` inputs produced by Prompt; no runtime context injection. |
| ADR 002 D1 Scope | Clients and settings are Session/process facts; the branch part only adopts; the line view is a client report. |
| ADR 002 D2 No wiring module | The App Server host is its own connection state; the capability parts own their projection. |
| ADR 002 D3 Dependencies | The Session part depends on the process host; the branch part depends on Session/branch services; no core depends on the optional feature. |
| ADR 002 D4 Configuration | Settings stay in their owners' sources; the state source is a read projection, not a second store. |
| ADR 002 D5 Language-neutral contracts | The host service and the shape of the counts are data; Rust is a binding. |

## 14. Verification

| Scenario | Expected |
| --- | --- |
| Two TUIs attach the same Session | The model sees one presence block at the next idle check with 2 tui; not two attach blocks |
| A TUI leaves while idle | No block; the next idle check shows 1 tui |
| The last client leaves | The next idle check says no client is attached |
| Web, ACP or a remote device attaches | The channel count names it; no device id/name/token |
| No provider | No source, no block, no error |
| Settings change from another client | The next request after the change carries the new execution state; no reload |
| A non-model field of the same source changes | No block |
| Restore | Current facts, not an old block as "now" |
| Compaction | The latest block of each key is replayed; old blocks are gone |
| Multiple open lines | Each line has its own local block; no cross-branch write |
| Closing branch | No adoption, no write |

## 15. Open decisions

1. **Branch-scoped sources.** Permission preset and per-branch model facts need a Prompt/PromptSources decision. Until then they are not model state.
2. **Client view promotion.** Line view is management/UI first; promoting it to the model needs a concrete behaviour and an evaluation.
3. **Client capabilities.** A later design: what a client can render/answer, declared in `hello` or an additive method, and how the model uses it.
4. **Non-interactive wording.** Whether the presence block names the counterpart or only the absence of clients.
5. **Settings write path.** The exact allowlist and the choice between a tool and a CLI + Skill; see [AgentSettings](plugins/AgentSettings.md).
6. **Freshness.** Whether cheap in-memory facts are read at the request boundary (#986) or accept a one-request lag.
