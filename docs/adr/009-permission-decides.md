# 009: Permission decides; HostPlatform enforces

Decision status: Proposed.

Supersedes the permission-preset clause of the second sentence of
[ADR 003](003-approval-scope.md) D5 — “permission presets confer nothing beyond what they confer
today” — and the `escalation` gate introduced for
[#503](https://github.com/arcships/dim/pull/503) in the
[F5 isolation design](../design/host-platform-isolation.md). File-root authorization in the same
sentence is unchanged. ADR 003 is not edited.

## Context

A tool call is admitted by `Permission` through the session permission mode, the tool lists and
the user's command rules. A process-starting tool then calls `HostPlatform`, which reads
`host-isolation.json` and refuses a wider `sandbox` request unless the file says
`escalation: "ask"`. That is a second decision: it can veto a call `Permission` already allowed,
and the only way to change it is a file the user has to know about. `full-access` therefore does
not mean what its label says for a process boundary.

## Decision

### D1. The permission decision is final for admission

The session permission mode decides whether a tool call may run. `HostPlatform` enforces the
environment it is given; it does not refuse an admitted call and does not read a separate
admission field. `escalation` is removed.

### D2. The mode decides the requested boundary

`full-access` admits a requested boundary, `workspace-write` asks for it through the existing
execute-process approval, and `read-only` denies it. The request remains the existing `sandbox`
argument only while this decision is read alone. [ADR 010](010-sandbox-opt-in.md)
removes the argument and replaces it with the user sandbox setting; the two decisions land
together, so a full-access call cannot request a boundary.

### D3. No new event, type or stored grant

The approval request carries the call and, while the `sandbox` argument exists, the requested
boundary. Running processes keep the environment they started with. HostPlatform keeps its
platform defaults and roots.

## Consequences

- `HostPlatform` no longer makes an admission decision; `isolation_denied` from the node policy
  disappears.
- While the argument exists, `full-access` admits a requested `host` or `network` boundary; an
  approved `workspace-write` command runs at the boundary the prompt showed. ADR 010 removes the
  argument, so the pair has no model-requested boundary.
- The node file's `escalation` field is ignored. The remaining policy file and the `sandbox`
  argument are replaced by [ADR 010](010-sandbox-opt-in.md); 009 is not implemented
  before 010.
- No Timeline event, wire field, event fixture or persisted grant is added.
- AGENTS.md rule 11's "permission presets" clause is updated by this decision.

## Non-goals

- The default sandbox policy, the policy file and the `sandbox` argument. ADR 010 decides them.
- Changing which commands are agent decisions (ADR 003 D1–D3).

## Supersession

Supersedes the permission-preset clause of [ADR 003](003-approval-scope.md) D5's second sentence
and the `escalation` gate in the [F5 isolation design](../design/host-platform-isolation.md).
ADR 003 remains as written; the relation is recorded in [`docs/adr/README.md`](README.md).

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `Permission` and `Approval` remain branch Capabilities; `ToolExecutor` and `ToolBinding` remain branch parts; tool owners keep execution; `HostPlatform` remains a process part. No part moves. |
| §4 rule 1 | `approval/request` and `approval/respond` keep instructing the owner of admission. |
| §4 rule 2 | The admitted call is handed to execution once. |
| §4 rule 3 | No `before` callback is added. |
| §4 rule 4 | No core mechanism changes. |
| §4 rule 5 | No new Timeline batch. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | Parts interact through existing services and events. |
| §4 rule 8 | No infrastructure change; no requirement is raised to the runtime. |
| §4 rule 9 | No new Timeline fact. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Dependencies are unchanged. |
| ADR 002 D4 | The policy file remains business configuration until ADR 010 replaces it. |
| ADR 002 D5 | No new contract; the existing call input carries the request. |
