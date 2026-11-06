# ClientView

Status: Deferred for model state; protocol/management proposal. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1021](https://github.com/arcships/dim/issues/1021). Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

ClientView records which line a client is viewing. It is a management/UI fact first. It is **not** model state in version 1: the model cannot act on a client's cursor, and every line switch would otherwise become a state block. A later promotion needs a concrete behaviour, an evaluation and the branch-scoped source decision.

## 1. Scope and non-goals

Owns:

- the additive protocol method by which a client reports the line it views;
- the per-client view record in the App Server's client registry;
- the management/UI read of that record.

Does not own:

- the client list and channel counts; that is [EnvironmentClients](EnvironmentClients.md);
- the branch tree, line switching or the TUI's local navigation;
- focus, selected message, open panel or scroll position;
- model-visible presence. The model body stays counts-only unless this fact is promoted.

## 2. Contract

### 2.1 Method

| Element | Form |
| --- | --- |
| Method | `session.view {sessionId, branchId}`, additive protocol v3 |
| Authorization | The caller must be attached to `sessionId` |
| Branch | `branchId` must exist and not have ended |
| Idempotence | Reporting the same branch again changes nothing |
| Old clients | A client that never calls it has an unknown view |
| Clear | A client clears its view with `session.view {sessionId, branchId: ""}` or by disconnecting; the design decides the exact clear form |
| Errors | `session_not_attached`, `branch_missing`, `branch_ended`, invalid params |

The method is a built-in Session call, not a management method: a paired remote client may call it under the same Session authorization as `session.submit`. It records presentation state; it does not write a Timeline fact and does not change which events the client receives.

### 2.2 Record

For each attached client the App Server keeps, in memory only:

```text
{ connectionId, channel, viewedBranch? }
```

`connectionId` and `channel` are already part of the presence read; `viewedBranch` is the branch id the client last reported. It is not durable, not a Timeline fact and not an identity. A UI leaf and its connection are one client.

### 2.3 Read

The management/UI read lists the clients and their viewed branch. It may be folded into the presence read or exposed as a separate management query; the model body is not affected.

## 3. Model visibility

Default: **off**. `dim/environment/clients` stays counts-only.

If a later design promotes it, the only allowed model fact is a small aggregate computed on operator input and at branch open:

```text
<dim-state source="dim/environment/clients">2 clients attached: 2 tui. 1 client is viewing this line.</dim-state>
```

The exact "this line" rendering needs a branch-scoped source: the current `targets()` fixes the source to `main`, and a Session-level loader does not know which branch is adopting. Until that route exists, the fact stays out of the model; naming the viewed branch ids instead would be more detail than the model can use.

Not allowed: one block per line switch, per-client lines, focus/selection/panel state, or a "last viewed" history.

## 4. Why deferred

- A line switch is a UI cursor change. A user browsing lines with Ctrl+S or `/timeline` would produce a stream of user-role state blocks that do not change what the model can do.
- The model runs in a branch. It cannot switch the client's line, and output is branch-scoped or broadcast; knowing that someone is looking at another line can make the model hesitate or mis-address the answer.
- A turn-boundary boolean is the most that could be useful, and only for a concrete policy (for example, "do not ask a question nobody is looking at"). That policy needs an evaluation ([model-facing prompts §8](../model-prompts.md#8-judging-a-change)).

## 5. Events and state

No Timeline event. The view is process memory on the connection/registry. It is cleared when the connection detaches or closes, when the branch ends, or when the client reports another branch. It is not restored after a daemon restart; clients re-report on reconnect.

## 6. Cross-module requirements

### 6.1 App Server

- Add the additive method to the protocol and the connection handler.
- Keep `viewedBranch` on the connection's client record; clear it on detach, close and branch end.
- Extend the presence read with the view field for management/UI, not for the model body.

### 6.2 Clients

- TUI: report after `session.attach` and after every line switch, `/new`, resume and fork; report on reconnect.
- Web/Desktop: report the visible line(s); a split view may report one primary line or several, a decision for the client that implements it.
- ACP: report the editor's visible session line where the client can; otherwise omit.
- A client that cannot report simply leaves the view unknown; it is not an error.

### 6.3 Prompt/PromptSources

- A model-visible "this line" boolean needs a branch-scoped source. That is the same requirement as the permission preset in [EnvironmentSettings §6](EnvironmentSettings.md#6-branch-scoped-settings); the owner decides it.

## 7. Requirements raised to mechanism owners

None to the runtime or rutis; the per-connection record and its read are new App Server work.

## 8. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded | The record lives on the connection; the presence capability reads it only for management/UI. |
| §4 rule 1 Command events | No other module's state is changed; the client reports its own presentation. |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | None. |
| §4 rule 4 Core carries no business meaning | The method records a branch id; it has no line semantics. |
| §4 rule 5 Batch | No facts. |
| §4 rule 6 Context from consumed events | No model context in v1. |
| §4 rule 7 Public services | The method is a Session call; the read is a management query. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope or assembly change. |
| §4 rule 9 Timeline holds decision facts | The view is process memory, not a Timeline fact. |
| §4 rule 10 Adopted inputs | No model input in v1. |
| ADR 002 D1 | Presentation state belongs to the connection/Session surface. |
| ADR 002 D2 | No adapter plugin. |
| ADR 002 D3 | The client reports through the existing Session authorization. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | The method shape is data; clients in any language can call it. |

## 8. Verification

| Scenario | Expected |
| --- | --- |
| Two TUIs view different lines | Management/UI shows each; model context has no new block |
| A TUI switches lines | Management/UI updates; no per-switch block |
| A TUI detaches | Its view is cleared |
| A branch ends | Views pointing to it are cleared |
| An old client never calls `session.view` | Unknown view; no error |
| An unattached connection calls it | Rejected |
| An unknown or ended branch | Rejected |
| Reconnect | The client re-reports; no stale view |
| If promoted later | One boolean on operator input, not a per-switch history |

## 9. Open decisions

1. The clear form: empty `branchId`, a separate `session.view.clear`, or disconnect only.
2. Web/Desktop split views: one primary line or a set.
3. Whether management/UI exposes per-client views or only an aggregate.
4. The branch-scoped source route required to put the boolean in model context.
