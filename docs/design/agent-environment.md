# Agent environment awareness

Status: Draft. Issues: [#1018](https://github.com/arcships/dim/issues/1018) (this design), [#1019](https://github.com/arcships/dim/issues/1019) (clients), [#1020](https://github.com/arcships/dim/issues/1020) (settings), [#1021](https://github.com/arcships/dim/issues/1021) (line view, deferred) and [#1022](https://github.com/arcships/dim/issues/1022) (settings tool). Rules: [design rules](../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../adr/002-plugin-scope.md), [model-facing prompts](model-prompts.md).

## 1. Problem

The model reads its working directory, shell, platform and Skills list, but does not know:

- which client connections are attached to its Session;
- which settings the user changed while the Session was running;
- what the values mean for the next action.

The goal is awareness of the user's environment and choices, not a new permission model.

## 2. Facts

| Fact | Producer | Delivered as | Design |
| --- | --- | --- | --- |
| Attached connections | App Server connection and UI registries | `<dim-state>` snapshot | [EnvironmentClients](plugins/EnvironmentClients.md) |
| Execution sandbox and network | Execution settings module | `<dim-state>` snapshot | [EnvironmentSettings](plugins/EnvironmentSettings.md) |
| Per-session model, effort, fast mode | Their setting modules | `<dim-state>` snapshot when provided | [EnvironmentSettings](plugins/EnvironmentSettings.md) |
| Settings tool | One `settings` tool registered by the setting owner (v1 `ExecutionSettingsPlugin`) | tool call and tool result | [AgentSettings](plugins/AgentSettings.md) |
| Viewed branch | Client report | deferred | [ClientView](plugins/ClientView.md) |
| Permission preset | Permission | not delivered | Branch-specific; outside this design |

## 3. Delivery

Environment facts are snapshots, not Prompt sources.

- The branch part appends a Raw block to the branch input.
- The model sees a `<dim-state>` input; the UI does not render the block.
- One snapshot at branch open.
- One snapshot per observed change.
- The body is compared with the last appended body; an unchanged body appends nothing.
- No `PromptRegistry`, `LoadPrompt`, version, keyed state or compaction replay.
- A snapshot is normal history. Compaction may remove it; it is not replayed.
- No Timeline event for presence or settings.

### Presence

`ClientsChanged { session_id }` wakes the branch part of that Session. The branch part reads the current connection list and appends the snapshot. Other Sessions are untouched.

### Settings

Every open branch wakes on `SettingsChanged`, reads the current value and appends the snapshot when the body differs. There is no origin routing and no `turn/start` refresh. An `Append` input does not drive a turn, so an idle Session gets a new history input but no turn.

The settings tool is separate from the snapshot. Its result is a tool result; the new value reaches context through the snapshot.

## 4. Placement

- Presence: one branch part, registered by `ConnectionPlugin`.
- Settings snapshot: one branch part, registered by `SettingsPlugin`.
- Settings tool: one Session part, registered by the process plugin that owns the first setting. In v1 that is `ExecutionSettingsPlugin`.
- No new process plugin.
- No Session prompt-source part for either fact.
- A branch part appends only to its own branch.

## 5. Settings tool

There is exactly one tool, `settings`, with `op` and `name`.

- `list` returns each setting's name, description, type, current value, allowed values and writability.
- `get` returns one setting.
- v1 has no `set`; a write path is deferred.
- No source, session or visibility parameter.
- Permission decides allow, ask or deny. The read tool uses the `session` risk; it has no write security level.
- Secrets never appear in list, get, result or snapshot.

The full contract is [AgentSettings](plugins/AgentSettings.md).

## 6. Noise

- A change is not automatically a block. It becomes one only when the body differs.
- Presence lists connections, not people; a TUI and its line connection may both appear.
- Settings state the effective value, not the document, revision or author.
- A setting changed and changed back produces two snapshots; the old snapshot stays until compaction.
- No timestamps, ids, device names or departure history.
- An idle Session does not wake for another Session's settings change.

## 7. Privacy and trust

- Presence exposes a channel and an optional claimed label. A claimed label is not an identity and grants nothing.
- A label is escaped and bounded.
- No token, device id, client id, entry name or secret enters a snapshot, a tool result or a log.
- A settings `set` is an agent action admitted by Permission; the snapshot does not grant authority.
- The model is told that a snapshot is state, not a request.

## 8. Non-interactive Sessions

A Session can outlive every client. The presence snapshot says no connection is attached. The model must not conclude that no one can answer; a Collaboration member or a later client may still answer. The exact wording is an open decision in [EnvironmentClients](plugins/EnvironmentClients.md).

## 9. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded | Connection and setting values stay with their producers; branch parts only append snapshots. |
| §4 rule 1 Command events | The design changes no state; the settings tool writes through its setting module. |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | Branch parts append in their own tasks; event handlers only wake them. |
| §4 rule 4 Core carries no business meaning | Bodies are module projections; no core branch. |
| §4 rule 5 Batch | A snapshot is one input; a settings write is one CAS write. |
| §4 rule 6 Context from consumed events | Snapshots are inputs; tool results are tool results; no second path. |
| §4 rule 7 Public services | `SessionClientsHost` and the setting modules' read ports are public. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Presence and settings are not Timeline facts. |
| §4 rule 10 Adopted inputs | The model reads the appended input directly. |
| ADR 002 D1 | Each fact stays with its producer; parts are placed in Session or branch. |
| ADR 002 D2 | No adapter plugin; one projection per fact, one tool. |
| ADR 002 D3 | Parts depend on public ports; no core depends on them. |
| ADR 002 D4 | No new configuration; settings stay with their modules. |
| ADR 002 D5 | Bodies and tool inputs are data; Rust is a binding. |

## 10. Open decisions

1. The exact wording of the non-interactive presence case.
2. Whether a long connection list is capped.
3. Whether a claimed label is shown by default or only when the client opts in.
4. The client view (viewed branch) remains deferred.
5. Whether a snapshot should ever be replayed after compaction; the current answer is no.
6. Per-message input origin is deferred.
